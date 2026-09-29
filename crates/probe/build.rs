use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=JAVA_HOME");
    let java =
        PathBuf::from(env::var_os("JAVA_HOME").expect("CI must provide a JDK via JAVA_HOME"));
    let target = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let platform = match target.as_str() {
        "windows" => "win32",
        "macos" => "darwin",
        _ => "linux",
    };
    bindgen::Builder::default()
        .header(java.join("include/jvmti.h").to_string_lossy())
        .clang_arg(format!("-I{}", java.join("include").display()))
        .clang_arg(format!(
            "-I{}",
            java.join("include").join(platform).display()
        ))
        .allowlist_type("jvmti.*")
        .allowlist_var("JVMTI.*")
        .derive_default(true)
        .layout_tests(false)
        .generate()
        .expect("generate JVMTI bindings")
        .write_to_file(PathBuf::from(env::var("OUT_DIR").unwrap()).join("jvmti.rs"))
        .unwrap();
}
