#![allow(dead_code, unused_imports)]
#[cfg(target_os = "macos")]
#[path = "../src/app.rs"]
mod app;
#[cfg(target_os = "macos")]
#[path = "../src/launch.rs"]
mod launch;
#[cfg(target_os = "macos")]
#[path = "../src/platform.rs"]
mod platform;

fn main() {
    #[cfg(target_os = "macos")]
    if std::env::var_os("ANIMEKO_RUN_NATIVE_TESTS").as_deref() == Some(std::ffi::OsStr::new("1")) {
        platform::run_native_checks();
        return;
    }
    println!(
        "Skipped native media session checks; on macOS set ANIMEKO_RUN_NATIVE_TESTS=1 to run."
    );
}
