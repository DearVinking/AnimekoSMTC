#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(any(windows, target_os = "macos", test))]
mod app;
#[cfg(any(windows, target_os = "macos", test))]
mod launch;
#[cfg(any(windows, target_os = "macos"))]
mod platform;

fn main() {
    #[cfg(any(windows, target_os = "macos"))]
    if let Err(error) = platform::run() {
        platform::show_error(&format!("{error:#}"));
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    eprintln!("AnimekoSMTC requires Windows 10/11 or macOS 11+.");
}
