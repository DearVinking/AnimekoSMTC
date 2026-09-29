use animeko_protocol::{Action, Media, Playback};
use anyhow::{bail, Result};
use std::{ptr::without_provenance, sync::mpsc::Sender};
use windows::{
    core::{factory, w, HSTRING, PCWSTR},
    Foundation::{TimeSpan, TypedEventHandler, Uri},
    Media::{
        MediaPlaybackStatus, MediaPlaybackType, SystemMediaTransportControls,
        SystemMediaTransportControlsButton, SystemMediaTransportControlsButtonPressedEventArgs,
        SystemMediaTransportControlsTimelineProperties,
    },
    Storage::Streams::RandomAccessStreamReference,
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        System::{
            LibraryLoader::GetModuleHandleW,
            WinRT::{
                ISystemMediaTransportControlsInterop, RoInitialize, RoUninitialize,
                RO_INIT_MULTITHREADED,
            },
        },
        UI::{Shell::SetCurrentProcessExplicitAppUserModelID, WindowsAndMessaging::*},
    },
};

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_CLOSE => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

pub(super) struct SmtcSession {
    hwnd: HWND,
    controls: SystemMediaTransportControls,
    button_handler: i64,
}

impl SmtcSession {
    pub(super) fn new(actions: Sender<Action>) -> Result<Self> {
        unsafe {
            RoInitialize(RO_INIT_MULTITHREADED)?;
            SetCurrentProcessExplicitAppUserModelID(w!("Ani"))?;
            let instance = GetModuleHandleW(None)?;
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance.into(),
                hIcon: LoadIconW(Some(instance.into()), PCWSTR(without_provenance(1)))?,
                lpszClassName: w!("AnimekoSMTCWindow"),
                ..Default::default()
            };
            if RegisterClassW(&class) == 0 {
                bail!("注册 SMTC 窗口失败");
            }
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class.lpszClassName,
                w!("Ani"),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance.into()),
                None,
            )?;
            let interop: ISystemMediaTransportControlsInterop =
                factory::<SystemMediaTransportControls, ISystemMediaTransportControlsInterop>()?;
            let controls: SystemMediaTransportControls = interop.GetForWindow(hwnd)?;
            controls.SetIsEnabled(false)?;
            controls.SetIsPlayEnabled(true)?;
            controls.SetIsPauseEnabled(true)?;
            controls.SetIsNextEnabled(false)?;
            controls.SetIsPreviousEnabled(false)?;
            let handler = TypedEventHandler::<
                SystemMediaTransportControls,
                SystemMediaTransportControlsButtonPressedEventArgs,
            >::new(move |_, args| {
                let button = args.ok()?.Button()?;
                let action = if button == SystemMediaTransportControlsButton::Play {
                    Some(Action::Play)
                } else if button == SystemMediaTransportControlsButton::Pause {
                    Some(Action::Pause)
                } else {
                    None
                };
                if let Some(action) = action {
                    let _ = actions.send(action);
                }
                Ok(())
            });
            let button_handler = controls.ButtonPressed(&handler)?;
            Ok(Self {
                hwnd,
                controls,
                button_handler,
            })
        }
    }

    pub(super) fn update(&self, media: Option<&Media>, previous: Option<&Media>) -> Result<()> {
        let Some(media) = media else {
            self.controls
                .SetPlaybackStatus(MediaPlaybackStatus::Stopped)?;
            self.controls.SetIsEnabled(false)?;
            if previous.is_some() {
                let updater = self.controls.DisplayUpdater()?;
                updater.ClearAll()?;
                updater.Update()?;
            }
            return Ok(());
        };
        self.controls.SetIsEnabled(true)?;
        if metadata_changed(media, previous) {
            self.update_metadata(media)?;
        }
        let status = match media.playback {
            Playback::Playing => MediaPlaybackStatus::Playing,
            Playback::Paused => MediaPlaybackStatus::Paused,
            Playback::Buffering => MediaPlaybackStatus::Changing,
            Playback::Stopped => MediaPlaybackStatus::Stopped,
        };
        self.controls.SetPlaybackStatus(status)?;
        // Position is authoritative at each sample; no extrapolation across buffering or seeking.
        self.controls.SetPlaybackRate(0.0)?;
        let timeline = timeline_properties(media)?;
        self.controls.UpdateTimelineProperties(&timeline)?;
        Ok(())
    }

    fn update_metadata(&self, media: &Media) -> Result<()> {
        let updater = self.controls.DisplayUpdater()?;
        updater.ClearAll()?;
        // Music properties expose title/artist/album to Windows and GSMTC consumers.
        updater.SetType(MediaPlaybackType::Music)?;
        updater.SetAppMediaId(&HSTRING::from(format!(
            "animeko:{}:{}",
            media.subject_id, media.episode_id
        )))?;
        let music = updater.MusicProperties()?;
        music.SetTitle(&HSTRING::from(&media.title))?;
        music.SetArtist(&HSTRING::from(&media.episode))?;
        music.SetAlbumTitle(&HSTRING::from("Animeko"))?;
        if !media.cover_url.is_empty() {
            if let Ok(uri) = Uri::CreateUri(&HSTRING::from(&media.cover_url)) {
                if let Ok(thumbnail) = RandomAccessStreamReference::CreateFromUri(&uri) {
                    let _ = updater.SetThumbnail(&thumbnail);
                }
            }
        }
        updater.Update()?;
        Ok(())
    }
}

impl Drop for SmtcSession {
    fn drop(&mut self) {
        let _ = self.controls.SetIsEnabled(false);
        let _ = self.controls.RemoveButtonPressed(self.button_handler);
        unsafe {
            let _ = DestroyWindow(self.hwnd);
            RoUninitialize();
        }
    }
}

fn metadata_changed(media: &Media, previous: Option<&Media>) -> bool {
    previous.is_none_or(|old| {
        old.subject_id != media.subject_id
            || old.episode_id != media.episode_id
            || old.title != media.title
            || old.episode != media.episode
            || old.cover_url != media.cover_url
    })
}

fn timeline_properties(media: &Media) -> Result<SystemMediaTransportControlsTimelineProperties> {
    let timeline = SystemMediaTransportControlsTimelineProperties::new()?;
    timeline.SetStartTime(TimeSpan { Duration: 0 })?;
    timeline.SetMinSeekTime(TimeSpan { Duration: 0 })?;
    let end = media
        .duration_ms
        .unwrap_or(media.position_ms)
        .max(media.position_ms);
    timeline.SetEndTime(TimeSpan {
        Duration: end * 10_000,
    })?;
    timeline.SetMaxSeekTime(TimeSpan {
        Duration: end * 10_000,
    })?;
    timeline.SetPosition(TimeSpan {
        Duration: media.position_ms * 10_000,
    })?;
    Ok(timeline)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media() -> Media {
        Media {
            subject_id: 42,
            episode_id: 1,
            title: "Title".into(),
            episode: "01".into(),
            cover_url: "https://example.com/cover".into(),
            playback: Playback::Playing,
            position_ms: 1000,
            duration_ms: Some(2000),
        }
    }

    #[test]
    fn playback_and_timeline_changes_do_not_reload_metadata() {
        let previous = media();
        let mut current = previous.clone();
        current.position_ms = 1500;
        current.duration_ms = None;
        current.playback = Playback::Buffering;
        assert!(!metadata_changed(&current, Some(&previous)));
        assert!(metadata_changed(&current, None));
    }

    #[test]
    fn each_display_property_change_refreshes_metadata() {
        let previous = media();
        let variants = [
            Media {
                subject_id: 43,
                ..media()
            },
            Media {
                episode_id: 2,
                ..media()
            },
            Media {
                title: "Other".into(),
                ..media()
            },
            Media {
                episode: "02".into(),
                ..media()
            },
            Media {
                cover_url: String::new(),
                ..media()
            },
        ];
        for current in variants {
            assert!(metadata_changed(&current, Some(&previous)));
        }
    }

    #[test]
    fn timeline_uses_authoritative_position_and_bounds_unknown_duration() {
        unsafe {
            RoInitialize(RO_INIT_MULTITHREADED).unwrap();
        }
        {
            for (duration_ms, position_ms, expected_end) in [
                (Some(2000), 1000, 20_000_000),
                (None, 1500, 15_000_000),
                (Some(1000), 1500, 15_000_000),
            ] {
                let current = Media {
                    duration_ms,
                    position_ms,
                    ..media()
                };
                let timeline = timeline_properties(&current).unwrap();
                assert_eq!(timeline.StartTime().unwrap().Duration, 0);
                assert_eq!(timeline.MinSeekTime().unwrap().Duration, 0);
                assert_eq!(timeline.EndTime().unwrap().Duration, expected_end);
                assert_eq!(timeline.MaxSeekTime().unwrap().Duration, expected_end);
                assert_eq!(timeline.Position().unwrap().Duration, position_ms * 10_000);
            }
        }
        unsafe {
            RoUninitialize();
        }
    }
}
