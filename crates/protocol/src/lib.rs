use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

pub const MAX_PACKET: usize = 16_384;
pub const STALE_AFTER: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Play,
    Pause,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Command {
    pub version: u32,
    pub token: String,
    pub sequence: u64,
    pub subject_id: i64,
    pub episode_id: i64,
    pub action: Action,
}

pub struct CommandGate {
    token: String,
    sequence: Option<u64>,
}

impl CommandGate {
    pub fn new(token: String) -> Self {
        Self {
            token,
            sequence: None,
        }
    }

    pub fn accept(&mut self, bytes: &[u8], media: Option<&Media>) -> Option<Action> {
        if bytes.len() >= MAX_PACKET {
            return None;
        }
        let command: Command = serde_json::from_slice(bytes).ok()?;
        if command.version != 1
            || command.token != self.token
            || self.sequence.is_some_and(|s| command.sequence <= s)
        {
            return None;
        }
        self.sequence = Some(command.sequence);
        let media = media?;
        (command.subject_id == media.subject_id && command.episode_id == media.episode_id)
            .then_some(command.action)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Playback {
    Playing,
    Paused,
    Buffering,
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Media {
    pub subject_id: i64,
    pub episode_id: i64,
    pub title: String,
    pub episode: String,
    pub cover_url: String,
    pub playback: Playback,
    pub position_ms: i64,
    pub duration_ms: Option<i64>,
}

impl Media {
    pub fn normalize(&mut self) {
        self.duration_ms = self
            .duration_ms
            .filter(|v| *v > 0)
            .map(|v| v.min(604_800_000));
        self.position_ms = self
            .position_ms
            .clamp(0, self.duration_ms.unwrap_or(604_800_000));
        if !self.cover_url.starts_with("https://") || self.cover_url.len() > 2048 {
            self.cover_url.clear();
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Packet {
    pub version: u32,
    pub token: String,
    pub sequence: u64,
    pub media: Option<Media>,
    pub diagnostic: String,
}

pub struct Receiver {
    token: String,
    sequence: Option<u64>,
    received: Option<Instant>,
    media: Option<Media>,
    pub diagnostic: String,
}

impl Receiver {
    pub fn new(token: String) -> Self {
        Self {
            token,
            sequence: None,
            received: None,
            media: None,
            diagnostic: "等待 Animeko 探针连接".into(),
        }
    }

    pub fn ingest(&mut self, bytes: &[u8], now: Instant) -> bool {
        if bytes.len() >= MAX_PACKET {
            return false;
        }
        let Ok(mut packet) = serde_json::from_slice::<Packet>(bytes) else {
            return false;
        };
        if packet.version != 1
            || packet.token != self.token
            || self.sequence.is_some_and(|s| packet.sequence <= s)
        {
            return false;
        }
        if let Some(media) = &mut packet.media {
            if media.title.trim().is_empty()
                || media.title.len() > 4096
                || media.episode.len() > 4096
            {
                return false;
            }
            media.normalize();
        }
        self.sequence = Some(packet.sequence);
        self.received = Some(now);
        self.media = packet.media;
        self.diagnostic = packet.diagnostic;
        true
    }

    pub fn current(&self, now: Instant) -> Option<&Media> {
        if self.connected(now) {
            self.media.as_ref()
        } else {
            None
        }
    }

    pub fn connected(&self, now: Instant) -> bool {
        self.received
            .is_some_and(|t| now.saturating_duration_since(t) < STALE_AFTER)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(sequence: u64) -> Packet {
        Packet {
            version: 1,
            token: "secret".into(),
            sequence,
            diagnostic: String::new(),
            media: Some(Media {
                subject_id: 1,
                episode_id: 2,
                title: "葬送的芙莉莲".into(),
                episode: "01 冒险的结束".into(),
                cover_url: "file:///private".into(),
                playback: Playback::Paused,
                position_ms: 5000,
                duration_ms: Some(4000),
            }),
        }
    }

    #[test]
    fn authenticates_and_rejects_reordered_packets() {
        let now = Instant::now();
        let mut rx = Receiver::new("secret".into());
        let mut p = packet(2);
        p.token = "wrong".into();
        assert!(!rx.ingest(&serde_json::to_vec(&p).unwrap(), now));
        assert!(rx.ingest(&serde_json::to_vec(&packet(2)).unwrap(), now));
        assert!(!rx.ingest(&serde_json::to_vec(&packet(1)).unwrap(), now));
        let media = rx.current(now).unwrap();
        assert_eq!(media.position_ms, 4000);
        assert!(media.cover_url.is_empty());
        assert_eq!(media.playback, Playback::Paused);
    }

    #[test]
    fn stale_disconnect_and_explicit_clear_remove_media() {
        let now = Instant::now();
        let mut rx = Receiver::new("secret".into());
        assert!(rx.ingest(&serde_json::to_vec(&packet(1)).unwrap(), now));
        assert!(rx.current(now + STALE_AFTER).is_none());
        let mut clear = packet(2);
        clear.media = None;
        assert!(rx.ingest(&serde_json::to_vec(&clear).unwrap(), now));
        assert!(rx.current(now).is_none());
    }

    #[test]
    fn malformed_and_oversized_packets_do_not_refresh_heartbeat() {
        let mut rx = Receiver::new("secret".into());
        let now = Instant::now();
        assert!(!rx.ingest(b"not json", now));
        assert!(!rx.ingest(&vec![b' '; MAX_PACKET], now));
        assert!(!rx.connected(now));
    }

    #[test]
    fn commands_require_token_current_episode_and_fresh_sequence() {
        let media = packet(0).media.unwrap();
        let mut gate = CommandGate::new("secret".into());
        let mut command = Command {
            version: 1,
            token: "wrong".into(),
            sequence: 1,
            subject_id: media.subject_id,
            episode_id: media.episode_id,
            action: Action::Pause,
        };
        assert!(gate
            .accept(&serde_json::to_vec(&command).unwrap(), Some(&media))
            .is_none());
        command.token = "secret".into();
        assert_eq!(
            gate.accept(&serde_json::to_vec(&command).unwrap(), Some(&media)),
            Some(Action::Pause)
        );
        assert!(gate
            .accept(&serde_json::to_vec(&command).unwrap(), Some(&media))
            .is_none());
        command.sequence = 2;
        command.episode_id += 1;
        assert!(gate
            .accept(&serde_json::to_vec(&command).unwrap(), Some(&media))
            .is_none());
        command.episode_id = media.episode_id;
        assert!(gate
            .accept(&serde_json::to_vec(&command).unwrap(), Some(&media))
            .is_none());
    }
}
