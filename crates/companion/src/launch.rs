#![cfg_attr(not(windows), allow(dead_code))]

use anyhow::{bail, Context, Result};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
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
        [] if config.is_file() => Ok(PathBuf::from(std::fs::read_to_string(config)?.trim())),
        [] | [_] => choose(),
        _ => bail!("用法：AnimekoSMTC.exe [--animeko 路径 | --choose]"),
    }
}

pub fn executable(path: &Path) -> Result<PathBuf> {
    let path = if path.is_dir() {
        path.join("Ani.exe")
    } else {
        path.to_path_buf()
    };
    if !path.is_file()
        || path
            .extension()
            .is_none_or(|e| !e.eq_ignore_ascii_case("exe"))
    {
        bail!("请选择 Animeko 安装目录内的 Ani.exe：{}", path.display());
    }
    if !path
        .parent()
        .is_some_and(|p| p.join("app/Ani.cfg").is_file())
    {
        bail!("该目录缺少 app/Ani.cfg，请选择 Animeko 桌面安装目录");
    }
    Ok(path)
}

pub fn options(existing: Option<OsString>, probe: &Path) -> Result<OsString> {
    let path = probe.to_str().context("探针路径不是有效 Unicode")?;
    if path.contains(['"', '\n', '\r']) {
        bail!("探针路径包含 JVM 不支持的字符");
    }
    let mut options = existing.unwrap_or_default();
    if options.to_string_lossy().contains("animeko_probe") {
        bail!("JAVA_TOOL_OPTIONS 已包含 Animeko 探针，请清除重复配置");
    }
    options.push(format!(" \"-agentpath:{path}\""));
    Ok(options)
}

pub fn start(exe: &Path, probe: &Path, endpoint: &str, token: &str, log: &Path) -> Result<Child> {
    let stderr = std::fs::File::create(log).context("无法创建启动日志")?;
    let mut command = Command::new(exe);
    command
        .current_dir(exe.parent().context("安装路径缺少父目录")?)
        .env(
            "JAVA_TOOL_OPTIONS",
            options(std::env::var_os("JAVA_TOOL_OPTIONS"), probe)?,
        )
        .env("ANIMEKO_SMTC_ENDPOINT", endpoint)
        .env("ANIMEKO_SMTC_TOKEN", token)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr);
    command.spawn().context("启动 Animeko 失败")
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
    fn installation_requires_executable_and_app_configuration() {
        let directory =
            std::env::temp_dir().join(format!("animeko-launch-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(directory.join("app")).unwrap();
        let check = || {
            assert!(executable(&directory).is_err());
            let exe = directory.join("Ani.exe");
            std::fs::write(&exe, []).unwrap();
            assert!(executable(&directory).is_err());
            std::fs::write(directory.join("app/Ani.cfg"), []).unwrap();
            assert_eq!(executable(&directory).unwrap(), exe);
            assert_eq!(executable(&exe).unwrap(), exe);
            let alternate = directory.join("alternate.EXE");
            std::fs::write(&alternate, []).unwrap();
            assert_eq!(executable(&alternate).unwrap(), alternate);
            let invalid = directory.join("Ani.txt");
            std::fs::write(&invalid, []).unwrap();
            assert!(executable(&invalid).is_err());
        };
        let result = std::panic::catch_unwind(check);
        std::fs::remove_dir_all(&directory).unwrap();
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
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
}
