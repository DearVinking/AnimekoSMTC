#![cfg_attr(windows, windows_subsystem = "windows")]

mod launch;
#[cfg(windows)]
mod platform;

fn main() {
    #[cfg(windows)]
    if let Err(error) = platform::run() {
        platform::show_error(&format!("{error:#}"));
    }
    #[cfg(not(windows))]
    eprintln!("AnimekoSMTC requires Windows 10/11 x64.");
}
