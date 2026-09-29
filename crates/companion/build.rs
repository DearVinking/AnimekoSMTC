use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=assets/ani.ico");
    println!("cargo:rerun-if-env-changed=RC_EXE");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is required"))
        .join("animeko-smtc.res");
    let compiler = env::var_os("RC_EXE").unwrap_or_else(|| "rc.exe".into());
    let status = Command::new(compiler)
        .args(["/nologo", "/fo"])
        .arg(&output)
        .arg("app.rc")
        .status()
        .expect("Windows SDK resource compiler (rc.exe) is required");
    assert!(status.success(), "Windows resource compilation failed");
    println!("cargo:rustc-link-arg-bin=AnimekoSMTC={}", output.display());
}
