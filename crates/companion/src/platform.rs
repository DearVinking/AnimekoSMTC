#[cfg(windows)]
#[path = "platform/smtc.rs"]
mod smtc;
#[cfg(windows)]
#[path = "platform/windows.rs"]
mod windows;
#[cfg(windows)]
pub(crate) use smtc::SmtcSession as Session;
#[cfg(windows)]
pub(crate) use windows::{run, show_error};

#[cfg(target_os = "macos")]
#[path = "platform/macos.rs"]
mod macos;

#[cfg(target_os = "macos")]
#[path = "platform/now_playing.rs"]
mod now_playing;

#[cfg(all(target_os = "macos", test))]
#[allow(unused_imports)]
pub(crate) use now_playing::run_native_checks;

#[cfg(target_os = "macos")]
pub(crate) use macos::{run, show_error};
#[cfg(target_os = "macos")]
pub(crate) use now_playing::NowPlayingSession as Session;

#[cfg(target_os = "macos")]
#[path = "platform/artwork.rs"]
mod artwork;
