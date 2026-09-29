use anyhow::{bail, Context, Result};
use std::{
    ffi::OsString,
    fs::OpenOptions,
    io::Read,
    path::{Path, PathBuf},
};
#[cfg(any(windows, target_os = "macos"))]
use std::{
    fs::File,
    io::Write,
    process::{Child, Command, Stdio},
};

pub fn select_path(
    args: &[OsString],
    config: &Path,
    choose: impl FnOnce() -> Result<PathBuf>,
) -> Result<PathBuf> {
    match args {
        [flag, path] if flag == "--animeko" => Ok(PathBuf::from(path)),
        [path] if path != "--choose" => Ok(PathBuf::from(path)),
        [] => match read_config(config) {
            Ok(path) => Ok(PathBuf::from(path.trim())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => choose(),
            Err(error) => Err(error.into()),
        },
        [_] => choose(),
        _ => bail!("用法：AnimekoSMTC [--animeko 路径 | --choose]"),
    }
}

pub fn options(existing: Option<OsString>, probe: &Path) -> Result<OsString> {
    let mut options = existing.unwrap_or_default();
    if options.to_string_lossy().contains("animeko_probe") {
        bail!("JAVA_TOOL_OPTIONS 已包含 Animeko 探针，请清除重复配置");
    }
    append_path_option(&mut options, "-agentpath:", probe)?;
    Ok(options)
}

fn append_path_option(options: &mut OsString, prefix: &str, path: &Path) -> Result<()> {
    let path = path.to_str().context("JVM 选项路径不是有效 Unicode")?;
    if path.contains(['"', '\n', '\r']) {
        bail!("JVM 选项路径包含不支持的字符");
    }
    options.push(format!(" \"{prefix}{path}\""));
    Ok(())
}

#[cfg(any(windows, target_os = "macos"))]
pub fn start(exe: &Path, probe: &Path, endpoint: &str, token: &str, log: &Path) -> Result<Child> {
    let java_options = options(std::env::var_os("JAVA_TOOL_OPTIONS"), probe)?;
    #[cfg(target_os = "macos")]
    let java_options = {
        let mut options = java_options;
        let icon = probe
            .parent()
            .and_then(Path::parent)
            .context("伴随应用包目录无效")?
            .join("Resources/ani.icns");
        append_path_option(&mut options, "-Dapple.awt.application.icon=", &icon)?;
        options
    };
    let output = create_file(log).context("无法创建启动日志")?;
    let mut command = Command::new(exe);
    command
        .current_dir(exe.parent().context("安装路径缺少父目录")?)
        .env("JAVA_TOOL_OPTIONS", java_options)
        .env("ANIMEKO_SMTC_ENDPOINT", endpoint)
        .env("ANIMEKO_SMTC_TOKEN", token)
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output);
    command.spawn().context("启动 Animeko 失败")
}

fn read_config(path: &Path) -> std::io::Result<String> {
    let mut value = String::new();
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    options.open(path)?.read_to_string(&mut value)?;
    Ok(value)
}

#[cfg(any(windows, target_os = "macos"))]
pub(crate) fn has_exited(child: &mut Child, log: &Path) -> Result<bool> {
    let Some(status) = child.try_wait()? else {
        return Ok(false);
    };
    if !status.success() {
        bail!("Animeko 异常退出（{status}）。请查看 {}。", log.display());
    }
    Ok(true)
}

#[cfg(any(windows, target_os = "macos"))]
pub(crate) fn create_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options.open(path)?;
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

#[cfg(any(windows, target_os = "macos"))]
pub(crate) fn write_file(path: &Path, bytes: impl AsRef<[u8]>) -> Result<()> {
    create_file(path)?.write_all(bytes.as_ref())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_paths_do_not_open_the_chooser() {
        let unused = Path::new("unused-config");
        for args in [
            vec!["--animeko".into(), "D:/Ani".into()],
            vec!["D:/Ani".into()],
        ] {
            assert_eq!(
                select_path(&args, unused, || panic!("unexpected chooser")).unwrap(),
                PathBuf::from("D:/Ani")
            );
        }
        assert_eq!(
            select_path(&["--unknown".into()], unused, || panic!(
                "unexpected chooser"
            ))
            .unwrap(),
            PathBuf::from("--unknown")
        );
        assert!(
            select_path(&["--wrong".into(), "path".into()], unused, || panic!(
                "unexpected chooser"
            ))
            .is_err()
        );
    }

    #[test]
    fn choose_overrides_saved_path_and_empty_arguments_use_it() {
        let config = std::env::temp_dir().join(format!("animeko-config-{}", uuid::Uuid::new_v4()));
        std::fs::write(&config, "  D:/saved/Ani.exe\n").unwrap();
        let result = std::panic::catch_unwind(|| {
            assert_eq!(
                select_path(&[], &config, || panic!("unexpected chooser")).unwrap(),
                PathBuf::from("D:/saved/Ani.exe")
            );
            assert_eq!(
                select_path(&["--choose".into()], &config, || Ok("chosen.exe".into())).unwrap(),
                PathBuf::from("chosen.exe")
            );
        });
        std::fs::remove_file(&config).unwrap();
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
        assert_eq!(
            select_path(&[], &config, || Ok("chosen.exe".into())).unwrap(),
            PathBuf::from("chosen.exe")
        );
        assert!(select_path(&["--choose".into()], &config, || bail!("cancelled")).is_err());
    }

    #[test]
    fn rejects_duplicate_probe_and_jvm_argument_delimiters() {
        for path in ["bad\"/probe.dll", "bad\n/probe.dll", "bad\r/probe.dll"] {
            assert!(options(None, Path::new(path)).is_err());
        }
        assert!(options(
            Some("-agentpath:animeko_probe.dll".into()),
            Path::new("probe.dll")
        )
        .is_err());
        assert_eq!(
            options(None, Path::new("probe.dll")).unwrap(),
            " \"-agentpath:probe.dll\""
        );
    }

    #[test]
    fn quotes_spaces_and_preserves_existing_jvm_flags() {
        let value = options(
            Some("-Xmx2g".into()),
            Path::new("D:/我的 应用/animeko_probe.dll"),
        )
        .unwrap();
        assert_eq!(
            value,
            "-Xmx2g \"-agentpath:D:/我的 应用/animeko_probe.dll\""
        );
        assert!(options(None, Path::new("D:/bad\"/probe.dll")).is_err());
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn config_and_logs_are_private_and_do_not_follow_symlinks() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let root = std::env::temp_dir().join(format!("animeko-private-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let config = root.join("config");
        write_file(&config, "missing Ani.app").unwrap();
        assert_eq!(
            std::fs::metadata(&config).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            select_path(&[], &config, || panic!("must retain saved selection")).unwrap(),
            PathBuf::from("missing Ani.app")
        );
        let link = root.join("link");
        symlink(&config, &link).unwrap();
        assert!(select_path(&[], &link, || panic!("symlink must be rejected")).is_err());
        assert!(write_file(&link, "overwrite").is_err());
        assert!(create_file(&link).is_err());
        assert_eq!(std::fs::read_to_string(&config).unwrap(), "missing Ani.app");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn failed_child_exit_reports_status_and_log_while_success_is_quiet() {
        let log = Path::new("/tmp/Animeko 中文/startup.log");
        let mut child = Command::new("/bin/sh")
            .args(["-c", "exit 9"])
            .spawn()
            .unwrap();
        child.wait().unwrap();
        let error = has_exited(&mut child, log).unwrap_err().to_string();
        assert!(error.contains('9'), "missing exit status: {error}");
        assert!(
            error.contains(log.to_str().unwrap()),
            "missing log path: {error}"
        );
        let mut child = Command::new("/usr/bin/true").spawn().unwrap();
        child.wait().unwrap();
        assert!(has_exited(&mut child, log).unwrap());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn child_receives_scoped_quoted_agent_environment() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("animeko 中文 空格-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let exe = root.join("child");
        let icon = root.join("Resources/ani.icns");
        let frameworks = root.join("Frameworks");
        std::fs::create_dir(&frameworks).unwrap();
        std::fs::create_dir(root.join("Resources")).unwrap();
        std::fs::write(&icon, include_bytes!("../assets/ani.icns")).unwrap();
        let probe = frameworks.join("libanimeko_probe.dylib");
        let existing = std::env::var_os("JAVA_TOOL_OPTIONS").unwrap_or_default();
        let expected = format!(
            "{} \"-agentpath:{}\" \"-Dapple.awt.application.icon={}\"",
            existing.to_string_lossy(),
            probe.display(),
            icon.display()
        );
        std::fs::write(root.join("expected"), expected).unwrap();
        std::fs::write(
            &exe,
            r#"#!/bin/sh
[ "$ANIMEKO_SMTC_TOKEN" = 'test-token' ] || exit 1
[ "$ANIMEKO_SMTC_ENDPOINT" = '127.0.0.1:45678' ] || exit 2
[ "$JAVA_TOOL_OPTIONS" = "$(cat expected)" ] || exit 3
printf 'stdout diagnostic\n'
printf 'stderr diagnostic\n' >&2
"#,
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
        let before = std::env::var_os("ANIMEKO_SMTC_TOKEN");
        let before_options = std::env::var_os("JAVA_TOOL_OPTIONS");
        let log = root.join("startup.log");
        let mut child = start(&exe, &probe, "127.0.0.1:45678", "test-token", &log).unwrap();
        assert!(child.wait().unwrap().success());
        let output = std::fs::read_to_string(&log).unwrap();
        assert!(
            output.contains("stdout diagnostic"),
            "stdout lost: {output}"
        );
        assert!(
            output.contains("stderr diagnostic"),
            "stderr lost: {output}"
        );
        assert_eq!(std::env::var_os("ANIMEKO_SMTC_TOKEN"), before);
        assert_eq!(std::env::var_os("JAVA_TOOL_OPTIONS"), before_options);
        assert_eq!(
            std::fs::metadata(log).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
