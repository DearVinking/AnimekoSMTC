use animeko_protocol::{Media, Playback};
use objc2::{rc::Retained, runtime::AnyObject, AnyThread};
use objc2_foundation::{NSMutableDictionary, NSNumber, NSString};
use objc2_media_player::*;

type NowPlayingInfo = NSMutableDictionary<NSString, AnyObject>;

fn metadata(media: &Media) -> Retained<NowPlayingInfo> {
    let info = NowPlayingInfo::new();
    unsafe {
        for (key, value) in [
            (MPMediaItemPropertyTitle, media.title.as_str()),
            (MPMediaItemPropertyArtist, media.episode.as_str()),
            (MPMediaItemPropertyAlbumTitle, "Animeko"),
        ] {
            info.insert(key, &*NSString::from_str(value));
        }
        info.insert(
            MPNowPlayingInfoPropertyExternalContentIdentifier,
            &*NSString::from_str(&format!(
                "animeko:{}:{}",
                media.subject_id, media.episode_id
            )),
        );
        info.insert(
            MPNowPlayingInfoPropertyMediaType,
            &*NSNumber::new_usize(MPNowPlayingInfoMediaType::Video.0),
        );
        info.insert(
            MPNowPlayingInfoPropertyIsLiveStream,
            &*NSNumber::new_bool(false),
        );
        info.insert(
            MPNowPlayingInfoPropertyDefaultPlaybackRate,
            &*NSNumber::new_f64(1.0),
        );
    }
    update_progress(&info, media);
    info
}

fn update_progress(info: &NowPlayingInfo, media: &Media) {
    unsafe {
        info.insert(
            MPNowPlayingInfoPropertyElapsedPlaybackTime,
            &*NSNumber::new_f64(media.position_ms as f64 / 1000.0),
        );
        info.insert(
            MPNowPlayingInfoPropertyPlaybackRate,
            &*NSNumber::new_f64(media.effective_rate()),
        );
        if let Some(duration) = media.duration_ms {
            info.insert(
                MPMediaItemPropertyPlaybackDuration,
                &*NSNumber::new_f64(duration as f64 / 1000.0),
            );
        } else {
            info.removeObjectForKey(MPMediaItemPropertyPlaybackDuration);
        }
    }
}

fn playback_state(playback: &Playback) -> MPNowPlayingPlaybackState {
    match playback {
        Playback::Playing => MPNowPlayingPlaybackState::Playing,
        Playback::Paused => MPNowPlayingPlaybackState::Paused,
        Playback::Buffering => MPNowPlayingPlaybackState::Interrupted,
        Playback::Stopped => MPNowPlayingPlaybackState::Stopped,
    }
}

use crate::app::{ControlHandle, ControlKind};
use anyhow::{Context, Result};
use block2::RcBlock;
use objc2::MainThreadMarker;
use std::{
    panic::{catch_unwind, AssertUnwindSafe},
    ptr::NonNull,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

type CommandHandler =
    RcBlock<dyn Fn(NonNull<MPRemoteCommandEvent>) -> MPRemoteCommandHandlerStatus>;
struct RegisteredCommand {
    command: Retained<MPRemoteCommand>,
    target: Retained<AnyObject>,
    _handler: CommandHandler,
}

pub(crate) struct NowPlayingSession {
    _main_thread: MainThreadMarker,
    center: Retained<MPNowPlayingInfoCenter>,
    handlers: Vec<RegisteredCommand>,
    accepting: Arc<AtomicBool>,
    info: Option<Retained<NowPlayingInfo>>,
    artwork: super::artwork::ArtworkLoader,
    cover: Option<Retained<MPMediaItemArtwork>>,
    artwork_error: Option<String>,
}

fn dispatch_command(
    accepting: &AtomicBool,
    controls: &ControlHandle,
    kind: ControlKind,
) -> MPRemoteCommandHandlerStatus {
    let delivered = catch_unwind(AssertUnwindSafe(|| {
        accepting.load(Ordering::Acquire) && controls.submit(kind)
    }))
    .unwrap_or(false);
    if delivered {
        MPRemoteCommandHandlerStatus::Success
    } else {
        MPRemoteCommandHandlerStatus::CommandFailed
    }
}

impl NowPlayingSession {
    pub(crate) fn new(controls: ControlHandle) -> Result<Self> {
        let main_thread = MainThreadMarker::new().context("Now Playing 必须在主线程初始化")?;
        let artwork = super::artwork::ArtworkLoader::new()?;
        let accepting = Arc::new(AtomicBool::new(false));
        unsafe {
            let center = MPNowPlayingInfoCenter::defaultCenter();
            let commands = MPRemoteCommandCenter::sharedCommandCenter();
            for command in [
                commands.stopCommand(),
                commands.nextTrackCommand(),
                commands.previousTrackCommand(),
                commands.skipForwardCommand().into_super(),
                commands.skipBackwardCommand().into_super(),
                commands.seekForwardCommand(),
                commands.seekBackwardCommand(),
                commands.changePlaybackPositionCommand().into_super(),
                commands.changePlaybackRateCommand().into_super(),
                commands.changeRepeatModeCommand().into_super(),
                commands.changeShuffleModeCommand().into_super(),
                commands.ratingCommand().into_super(),
                commands.likeCommand().into_super(),
                commands.dislikeCommand().into_super(),
                commands.bookmarkCommand().into_super(),
                commands.enableLanguageOptionCommand(),
                commands.disableLanguageOptionCommand(),
            ] {
                command.setEnabled(false);
            }
            let mut handlers = Vec::new();
            for (command, kind) in [
                (commands.playCommand(), ControlKind::Play),
                (commands.pauseCommand(), ControlKind::Pause),
                (commands.togglePlayPauseCommand(), ControlKind::Toggle),
            ] {
                command.setEnabled(false);
                let controls = controls.clone();
                let accepting = accepting.clone();
                let handler = RcBlock::new(move |_event: NonNull<MPRemoteCommandEvent>| {
                    dispatch_command(&accepting, &controls, kind)
                });
                let target = command.addTargetWithHandler(&handler);
                handlers.push(RegisteredCommand {
                    command,
                    target,
                    _handler: handler,
                });
            }
            Ok(Self {
                _main_thread: main_thread,
                center,
                handlers,
                accepting,
                info: None,
                artwork,
                cover: None,
                artwork_error: None,
            })
        }
    }

    pub(crate) fn update(
        &mut self,
        current: Option<&Media>,
        previous: Option<&Media>,
    ) -> Result<()> {
        let cover_changed = current.map(|m| (m.subject_id, m.episode_id, &m.cover_url))
            != previous.map(|m| (m.subject_id, m.episode_id, &m.cover_url));
        if cover_changed {
            self.cover = None;
            self.artwork_error = None;
        }
        if let Err(error) = self.artwork.select(current) {
            self.artwork_error = Some(error.to_string());
        }
        let completed = self.artwork.poll();
        let artwork_updated = completed.is_some();
        if let Some(result) = completed {
            match result {
                Ok(image) => {
                    let size = image.size();
                    let handler = RcBlock::new(move |_| NonNull::from(&*image));
                    self.cover = Some(unsafe {
                        MPMediaItemArtwork::initWithBoundsSize_requestHandler(
                            MPMediaItemArtwork::alloc(),
                            size,
                            &handler,
                        )
                    });
                }
                Err(error) => {
                    self.cover = None;
                    self.artwork_error = Some(error.to_string());
                }
            }
        }
        if current == previous && !artwork_updated {
            return Ok(());
        }
        let Some(media) = current else {
            self.clear();
            return Ok(());
        };
        let rebuild = previous.is_none_or(|old| {
            old.subject_id != media.subject_id
                || old.episode_id != media.episode_id
                || old.title != media.title
                || old.episode != media.episode
        });
        if rebuild || self.info.is_none() {
            self.info = Some(metadata(media));
        }
        let info = self.info.as_ref().expect("metadata initialized above");
        update_progress(info, media);
        unsafe {
            if let Some(cover) = &self.cover {
                info.insert(MPMediaItemPropertyArtwork, &**cover);
            } else {
                info.removeObjectForKey(MPMediaItemPropertyArtwork);
            }
            self.center.setNowPlayingInfo(Some(info));
            self.center
                .setPlaybackState(playback_state(&media.playback));
            for handler in &self.handlers {
                handler.command.setEnabled(true);
            }
        }
        self.accepting.store(true, Ordering::Release);
        Ok(())
    }

    fn clear(&mut self) {
        self.accepting.store(false, Ordering::Release);
        let _ = self.artwork.select(None);
        self.cover = None;
        self.artwork_error = None;
        unsafe {
            for handler in &self.handlers {
                handler.command.setEnabled(false);
            }
            self.center.setNowPlayingInfo(None);
            self.center
                .setPlaybackState(MPNowPlayingPlaybackState::Stopped);
        }
        self.info = None;
    }

    pub(crate) fn diagnostic(&self) -> Option<&str> {
        self.artwork_error.as_deref()
    }
}

impl Drop for NowPlayingSession {
    fn drop(&mut self) {
        self.clear();
        for handler in &self.handlers {
            unsafe { handler.command.removeTarget(Some(&handler.target)) };
        }
    }
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn run_native_checks() {
    use crate::app::Bridge;
    use animeko_protocol::{Action, Command, Packet, STALE_AFTER, VERSION};
    use std::{
        net::UdpSocket,
        time::{Duration, Instant},
    };
    let mtm = objc2::MainThreadMarker::new().expect("native checks require the main thread");
    let _app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    let mut bridge = Bridge::bind("native-test".into()).unwrap();
    let probe = UdpSocket::bind("127.0.0.1:0").unwrap();
    probe
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut session = NowPlayingSession::new(bridge.controls()).unwrap();
    let mut previous = None;
    for (sequence, playback) in [Playback::Playing, Playback::Paused, Playback::Buffering]
        .into_iter()
        .enumerate()
    {
        let mut media = tests::media();
        media.playback = playback;
        media.play_when_ready = sequence != 1;
        let packet = Packet {
            version: VERSION,
            token: "native-test".into(),
            sequence: sequence as u64,
            media: Some(media.clone()),
            diagnostic: format!("native-{sequence}"),
        };
        probe
            .send_to(
                &serde_json::to_vec(&packet).unwrap(),
                bridge.endpoint().unwrap(),
            )
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while bridge.diagnostic(Instant::now()) != packet.diagnostic && Instant::now() < deadline {
            bridge.poll(Instant::now()).unwrap();
            std::thread::yield_now();
        }
        assert_eq!(bridge.current(Instant::now()), Some(&media));
        session.update(Some(&media), previous.as_ref()).unwrap();
        unsafe {
            assert_eq!(
                session.center.playbackState(),
                playback_state(&media.playback)
            );
            let info = session.center.nowPlayingInfo().unwrap();
            assert_eq!(
                info.objectForKey(MPMediaItemPropertyTitle)
                    .unwrap()
                    .downcast_ref::<NSString>()
                    .unwrap()
                    .to_string(),
                "测试番剧"
            );
            assert_eq!(
                info.objectForKey(MPNowPlayingInfoPropertyPlaybackRate)
                    .unwrap()
                    .downcast_ref::<NSNumber>()
                    .unwrap()
                    .doubleValue(),
                if sequence == 0 { 2.0 } else { 0.0 }
            );
        }
        assert_eq!(
            dispatch_command(&session.accepting, &bridge.controls(), ControlKind::Toggle),
            MPRemoteCommandHandlerStatus::Success
        );
        bridge.poll(Instant::now()).unwrap();
        let mut buffer = [0; 16384];
        let size = probe.recv(&mut buffer).unwrap();
        let command: Command = serde_json::from_slice(&buffer[..size]).unwrap();
        assert_eq!(
            command.action,
            if sequence == 1 {
                Action::Play
            } else {
                Action::Pause
            }
        );
        previous = Some(media);
    }
    let mut cover_media = previous.as_ref().unwrap().clone();
    cover_media.cover_url = "https://example.com/cover.png".into();
    super::artwork::stage_test_download(&mut session.artwork, &cover_media);
    session
        .update(Some(&cover_media), previous.as_ref())
        .unwrap();
    let retained_artwork = unsafe {
        session
            .center
            .nowPlayingInfo()
            .unwrap()
            .objectForKey(MPMediaItemPropertyArtwork)
            .unwrap()
            .downcast::<MPMediaItemArtwork>()
            .unwrap()
    };
    previous = Some(cover_media);
    let mut invalid_cover = previous.as_ref().unwrap().clone();
    invalid_cover.cover_url = "http://localhost/private.png".into();
    session
        .update(Some(&invalid_cover), previous.as_ref())
        .unwrap();
    assert!(session.diagnostic().is_some());
    session
        .update(Some(&invalid_cover), Some(&invalid_cover))
        .unwrap();
    assert!(session.diagnostic().is_some());
    unsafe {
        let info = session.center.nowPlayingInfo().unwrap();
        assert!(info.objectForKey(MPMediaItemPropertyTitle).is_some());
        assert!(info.objectForKey(MPMediaItemPropertyArtwork).is_none());
        assert!(session.handlers[0].command.isEnabled());
    }
    bridge.poll(Instant::now() + STALE_AFTER).unwrap();
    session.update(None, previous.as_ref()).unwrap();
    unsafe {
        assert!(session.center.nowPlayingInfo().is_none());
        assert_eq!(
            session.center.playbackState(),
            MPNowPlayingPlaybackState::Stopped
        );
        assert!(!session.handlers[0].command.isEnabled());
    }
    let late_handler = session.handlers[0]._handler.clone();
    let accepting = session.accepting.clone();
    drop(session);
    assert!(!accepting.load(std::sync::atomic::Ordering::Acquire));
    assert_eq!(
        late_handler.call((std::ptr::NonNull::dangling(),)),
        MPRemoteCommandHandlerStatus::CommandFailed
    );
    unsafe {
        assert!(retained_artwork
            .imageWithSize(objc2_foundation::NSSize::new(2.0, 1.0))
            .is_some());
    }
    super::artwork::run_native_checks();
    super::macos::run_loop_checks();
    println!("Verified native metadata, playback states, buffered intent, stale clear and late callback rejection.");
}

#[cfg(test)]
mod tests {
    use super::*;
    use animeko_protocol::{Media, Playback};
    use objc2_foundation::{NSNumber, NSString};

    pub(super) fn media() -> Media {
        Media {
            subject_id: 42,
            episode_id: 7,
            title: "测试番剧".into(),
            episode: "07 · 剧集".into(),
            cover_url: String::new(),
            playback: Playback::Playing,
            position_ms: 12_500,
            duration_ms: Some(1440000),
            playback_rate: 2.0,
            play_when_ready: true,
        }
    }

    #[test]
    fn metadata_publishes_titles_identity_and_seconds() {
        let info = metadata(&media());
        unsafe {
            for (key, expected) in [
                (MPMediaItemPropertyTitle, "测试番剧"),
                (MPMediaItemPropertyArtist, "07 · 剧集"),
                (MPMediaItemPropertyAlbumTitle, "Animeko"),
                (
                    MPNowPlayingInfoPropertyExternalContentIdentifier,
                    "animeko:42:7",
                ),
            ] {
                assert_eq!(
                    info.objectForKey(key)
                        .unwrap()
                        .downcast_ref::<NSString>()
                        .unwrap()
                        .to_string(),
                    expected
                );
            }
            for (key, expected) in [
                (MPMediaItemPropertyPlaybackDuration, 1440.0),
                (MPNowPlayingInfoPropertyElapsedPlaybackTime, 12.5),
                (MPNowPlayingInfoPropertyPlaybackRate, 2.0),
                (MPNowPlayingInfoPropertyDefaultPlaybackRate, 1.0),
                (MPNowPlayingInfoPropertyMediaType, 2.0),
                (MPNowPlayingInfoPropertyIsLiveStream, 0.0),
            ] {
                assert_eq!(
                    info.objectForKey(key)
                        .unwrap()
                        .downcast_ref::<NSNumber>()
                        .unwrap()
                        .doubleValue(),
                    expected
                );
            }
        }
    }

    #[test]
    fn paused_and_buffering_have_zero_effective_progress() {
        let mut media = media();
        media.duration_ms = None;
        for (state, expected) in [
            (Playback::Paused, MPNowPlayingPlaybackState::Paused),
            (Playback::Buffering, MPNowPlayingPlaybackState::Interrupted),
            (Playback::Stopped, MPNowPlayingPlaybackState::Stopped),
        ] {
            media.playback = state;
            let info = metadata(&media);
            unsafe {
                assert!(info
                    .objectForKey(MPMediaItemPropertyPlaybackDuration)
                    .is_none());
                assert_eq!(
                    info.objectForKey(MPNowPlayingInfoPropertyElapsedPlaybackTime)
                        .unwrap()
                        .downcast_ref::<NSNumber>()
                        .unwrap()
                        .doubleValue(),
                    12.5
                );
                assert_eq!(
                    info.objectForKey(MPNowPlayingInfoPropertyPlaybackRate)
                        .unwrap()
                        .downcast_ref::<NSNumber>()
                        .unwrap()
                        .doubleValue(),
                    0.0
                );
            }
            assert_eq!(playback_state(&media.playback), expected);
        }
        assert_eq!(
            playback_state(&Playback::Playing),
            MPNowPlayingPlaybackState::Playing
        );
    }
}
