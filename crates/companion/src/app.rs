use animeko_protocol::{Action, Command, Media, Receiver, MAX_PACKET, VERSION};
use anyhow::Result;
use std::{
    net::{SocketAddr, UdpSocket},
    sync::{mpsc, Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug)]
pub(crate) enum ControlKind {
    Play,
    Pause,
    #[cfg(any(target_os = "macos", test))]
    Toggle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ControlTarget {
    subject_id: i64,
    episode_id: i64,
    generation: u64,
}

struct ControlRequest {
    target: ControlTarget,
    kind: ControlKind,
}

#[derive(Clone)]
pub(crate) struct ControlHandle {
    target: Arc<Mutex<Option<ControlTarget>>>,
    sender: mpsc::SyncSender<ControlRequest>,
}

impl ControlHandle {
    pub(crate) fn submit(&self, kind: ControlKind) -> bool {
        let Ok(guard) = self.target.lock() else {
            return false;
        };
        let Some(target) = *guard else { return false };
        self.sender
            .try_send(ControlRequest { target, kind })
            .is_ok()
    }
}

pub(crate) struct Bridge {
    socket: UdpSocket,
    receiver: Receiver,
    controls: ControlHandle,
    requests: mpsc::Receiver<ControlRequest>,
    token: String,
    peer: Option<SocketAddr>,
    generation: u64,
    sequence: u64,
    connected_once: bool,
}

impl Bridge {
    pub(crate) fn bind(token: String) -> Result<Self> {
        let socket = UdpSocket::bind("127.0.0.1:0")?;
        socket.set_nonblocking(true)?;
        let (sender, requests) = mpsc::sync_channel(32);
        Ok(Self {
            socket,
            receiver: Receiver::new(token.clone()),
            token,
            controls: ControlHandle {
                target: Arc::new(Mutex::new(None)),
                sender,
            },
            requests,
            peer: None,
            generation: 0,
            sequence: 0,
            connected_once: false,
        })
    }

    pub(crate) fn endpoint(&self) -> Result<SocketAddr> {
        Ok(self.socket.local_addr()?)
    }
    pub(crate) fn controls(&self) -> ControlHandle {
        self.controls.clone()
    }
    pub(crate) fn current(&self, now: Instant) -> Option<&Media> {
        self.receiver.current(now)
    }
    pub(crate) fn connected_once(&self) -> bool {
        self.connected_once
    }
    pub(crate) fn diagnostic(&self, now: Instant) -> &str {
        if self.connected_once && !self.receiver.connected(now) {
            "探针已断开连接"
        } else {
            &self.receiver.diagnostic
        }
    }

    fn refresh_target(&mut self, now: Instant) {
        let identity = self.current(now).map(|m| (m.subject_id, m.episode_id));
        let Ok(mut target) = self.controls.target.lock() else {
            return;
        };
        if target.map(|t| (t.subject_id, t.episode_id)) != identity {
            self.generation += 1;
            *target = identity.map(|(subject_id, episode_id)| ControlTarget {
                subject_id,
                episode_id,
                generation: self.generation,
            });
        }
    }

    fn ingest(&mut self, bytes: &[u8], peer: SocketAddr, now: Instant) {
        self.refresh_target(now);
        if peer.ip().is_loopback() && self.receiver.ingest(bytes, now) {
            self.peer = Some(peer);
            self.connected_once = true;
            self.refresh_target(now);
        }
    }

    fn next_command(&mut self, now: Instant) -> Option<Command> {
        self.refresh_target(now);
        let request = self.requests.try_recv().ok()?;
        if *self.controls.target.lock().ok()? != Some(request.target) {
            return None;
        }
        let media = self.current(now)?;
        let action = match request.kind {
            ControlKind::Play => Action::Play,
            ControlKind::Pause => Action::Pause,
            #[cfg(any(target_os = "macos", test))]
            ControlKind::Toggle if media.play_when_ready => Action::Pause,
            #[cfg(any(target_os = "macos", test))]
            ControlKind::Toggle => Action::Play,
        };
        let command = Command {
            version: VERSION,
            token: self.token.clone(),
            sequence: self.sequence,
            subject_id: media.subject_id,
            episode_id: media.episode_id,
            action,
        };
        self.sequence += 1;
        Some(command)
    }

    pub(crate) fn poll(&mut self, now: Instant) -> Result<()> {
        self.refresh_target(now);
        let mut buffer = [0; MAX_PACKET];
        for _ in 0..64 {
            match self.socket.recv_from(&mut buffer) {
                Ok((size, peer)) => self.ingest(&buffer[..size], peer, now),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error.into()),
            }
        }
        for _ in 0..16 {
            if let (Some(command), Some(peer)) = (self.next_command(now), self.peer) {
                self.socket.send_to(&serde_json::to_vec(&command)?, peer)?;
            }
        }
        Ok(())
    }
}

#[cfg(any(windows, target_os = "macos"))]
pub(crate) struct RunningApp {
    bridge: Bridge,
    session: crate::platform::Session,
    child: std::process::Child,
    data: std::path::PathBuf,
    started: Instant,
    last_media: Option<Media>,
    last_diagnostic: String,
}

#[cfg(any(windows, target_os = "macos"))]
impl RunningApp {
    pub(crate) fn start(
        exe: &std::path::Path,
        probe: &std::path::Path,
        data: std::path::PathBuf,
    ) -> Result<Self> {
        let token = uuid::Uuid::new_v4().to_string();
        let bridge = Bridge::bind(token.clone())?;
        let session = crate::platform::Session::new(bridge.controls())?;
        let child = crate::launch::start(
            exe,
            probe,
            &bridge.endpoint()?.to_string(),
            &token,
            &data.join("animeko-startup.log"),
        )?;
        Ok(Self {
            bridge,
            session,
            child,
            data,
            started: Instant::now(),
            last_media: None,
            last_diagnostic: String::new(),
        })
    }

    pub(crate) fn tick(&mut self, now: Instant) -> Result<std::ops::ControlFlow<()>> {
        self.bridge.poll(now)?;
        if crate::launch::has_exited(&mut self.child, &self.data.join("animeko-startup.log"))? {
            return Ok(std::ops::ControlFlow::Break(()));
        }
        if !self.bridge.connected_once()
            && now.saturating_duration_since(self.started) > Duration::from_secs(30)
        {
            anyhow::bail!(
                "30 秒内未收到 JVM 探针心跳。请查看 {}。Animeko 保持运行。",
                self.data.join("animeko-startup.log").display()
            );
        }
        let media = self.bridge.current(now);
        self.session.update(media, self.last_media.as_ref())?;
        self.last_media = media.cloned();
        let mut diagnostic = self.bridge.diagnostic(now).to_owned();
        if let Some(detail) = self.session.diagnostic() {
            diagnostic.push('；');
            diagnostic.push_str(detail);
        }
        if diagnostic != self.last_diagnostic {
            crate::launch::write_file(&self.data.join("status.txt"), &diagnostic)?;
            self.last_diagnostic = diagnostic;
        }
        Ok(std::ops::ControlFlow::Continue(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use animeko_protocol::{Packet, Playback, STALE_AFTER};

    fn media(episode_id: i64) -> Media {
        Media {
            subject_id: 42,
            episode_id,
            title: "测试".into(),
            episode: "剧集".into(),
            cover_url: String::new(),
            playback: Playback::Playing,
            position_ms: 0,
            duration_ms: Some(1000),
            playback_rate: 1.0,
            play_when_ready: true,
        }
    }
    fn packet(sequence: u64, media: Option<Media>) -> Vec<u8> {
        serde_json::to_vec(&Packet {
            version: VERSION,
            token: "secret".into(),
            sequence,
            media,
            diagnostic: "connected".into(),
        })
        .unwrap()
    }
    fn receive(bridge: &mut Bridge, sequence: u64, media: Option<Media>, now: Instant) {
        bridge.ingest(
            &packet(sequence, media),
            "127.0.0.1:40000".parse().unwrap(),
            now,
        );
    }

    #[test]
    fn clear_and_same_episode_reopen_invalidates_queued_actions() {
        let now = Instant::now();
        let mut bridge = Bridge::bind("secret".into()).unwrap();
        receive(&mut bridge, 0, Some(media(7)), now);
        assert!(bridge.controls().submit(ControlKind::Pause));
        receive(&mut bridge, 1, None, now);
        receive(&mut bridge, 2, Some(media(7)), now);
        assert!(bridge.next_command(now).is_none());
    }

    #[test]
    fn switching_or_expiring_media_invalidates_queued_actions() {
        let now = Instant::now();
        for (episode, elapsed) in [(8, Duration::ZERO), (7, STALE_AFTER)] {
            let mut bridge = Bridge::bind("secret".into()).unwrap();
            receive(&mut bridge, 0, Some(media(7)), now);
            assert!(bridge.controls().submit(ControlKind::Play));
            receive(&mut bridge, 1, Some(media(episode)), now + elapsed);
            assert!(bridge.next_command(now + elapsed).is_none());
        }
        let mut bridge = Bridge::bind("secret".into()).unwrap();
        assert!(!bridge.controls().submit(ControlKind::Pause));
        receive(&mut bridge, 0, Some(media(7)), now);
        assert!(bridge.controls().submit(ControlKind::Pause));
        assert!(bridge.next_command(now + STALE_AFTER).is_none());
        assert!(!bridge.controls().submit(ControlKind::Play));
    }

    #[test]
    fn rejected_packets_do_not_change_control_target_or_peer() {
        let now = Instant::now();
        let mut bridge = Bridge::bind("secret".into()).unwrap();
        receive(&mut bridge, 2, Some(media(7)), now);
        assert!(bridge.controls().submit(ControlKind::Pause));
        receive(&mut bridge, 1, Some(media(8)), now);
        let bad = packet(3, Some(media(8)));
        let bad = String::from_utf8(bad).unwrap().replace("secret", "wrong");
        bridge.ingest(bad.as_bytes(), "127.0.0.1:40001".parse().unwrap(), now);
        bridge.ingest(
            &packet(4, Some(media(9))),
            "192.0.2.1:40000".parse().unwrap(),
            now,
        );
        let command = bridge.next_command(now).unwrap();
        assert_eq!(
            (command.episode_id, command.sequence, command.action),
            (7, 0, Action::Pause)
        );
        assert_eq!(bridge.peer.unwrap().port(), 40000);
        assert_eq!(bridge.diagnostic(now), "connected");
    }

    #[test]
    fn native_callbacks_use_a_bounded_nonblocking_queue() {
        let now = Instant::now();
        let mut bridge = Bridge::bind("secret".into()).unwrap();
        receive(&mut bridge, 0, Some(media(7)), now);
        let handle = bridge.controls();
        for _ in 0..32 {
            assert!(handle.submit(ControlKind::Play));
        }
        assert!(!handle.submit(ControlKind::Pause));
        assert!(bridge.next_command(now).is_some());
        assert!(handle.submit(ControlKind::Pause));
        drop(bridge);
        assert!(!handle.submit(ControlKind::Play));
    }

    #[test]
    fn poll_authenticates_and_sends_commands_over_loopback() {
        let mut bridge = Bridge::bind("secret".into()).unwrap();
        let probe = UdpSocket::bind("127.0.0.1:0").unwrap();
        probe
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        probe
            .send_to(&packet(0, Some(media(7))), bridge.endpoint().unwrap())
            .unwrap();
        let now = Instant::now();
        let deadline = Instant::now() + Duration::from_secs(1);
        while !bridge.connected_once() && Instant::now() < deadline {
            bridge.poll(now).unwrap();
            std::thread::yield_now();
        }
        assert!(bridge.connected_once());
        assert_eq!(bridge.current(now).unwrap().episode_id, 7);
        assert!(bridge.controls().submit(ControlKind::Pause));
        bridge.poll(now).unwrap();
        let mut buffer = [0; MAX_PACKET];
        let size = probe.recv(&mut buffer).unwrap();
        let command: Command = serde_json::from_slice(&buffer[..size]).unwrap();
        assert_eq!(
            (command.version, command.token.as_str(), command.action),
            (2, "secret", Action::Pause)
        );
        bridge.poll(now + STALE_AFTER).unwrap();
        assert_eq!(bridge.diagnostic(now + STALE_AFTER), "探针已断开连接");
    }
    #[test]
    fn buffering_toggle_uses_latest_playback_intent() {
        let now = Instant::now();
        for (intent, expected) in [(true, Action::Pause), (false, Action::Play)] {
            let mut bridge = Bridge::bind("secret".into()).unwrap();
            receive(&mut bridge, 0, Some(media(7)), now);
            assert!(bridge.controls().submit(ControlKind::Toggle));
            let mut current = media(7);
            current.playback = Playback::Buffering;
            current.play_when_ready = intent;
            receive(&mut bridge, 1, Some(current), now);
            assert_eq!(bridge.next_command(now).unwrap().action, expected);
        }
    }
}
