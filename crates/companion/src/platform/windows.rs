use crate::{app::RunningApp, launch};
use anyhow::{bail, Context, Result};
use std::{
    mem::size_of,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use windows::{
    core::{w, HSTRING, PWSTR},
    Win32::{
        Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS},
        System::Threading::CreateMutexW,
        UI::{
            Controls::Dialogs::{
                GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_PATHMUSTEXIST, OPENFILENAMEW,
            },
            WindowsAndMessaging::*,
        },
    },
};

pub fn show_error(message: &str) {
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(message),
            w!("Animeko SMTC"),
            MB_OK | MB_ICONERROR,
        );
    }
}

fn choose_executable() -> Result<PathBuf> {
    let mut path = [0u16; 32768];
    let mut dialog = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        lpstrFilter: w!("Animeko (Ani.exe)\0Ani.exe\0"),
        lpstrFile: PWSTR(path.as_mut_ptr()),
        nMaxFile: path.len() as u32,
        lpstrTitle: w!("选择 Animeko 安装目录中的 Ani.exe"),
        Flags: OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST,
        ..Default::default()
    };
    if !unsafe { GetOpenFileNameW(&mut dialog) }.as_bool() {
        bail!("未选择 Animeko 程序");
    }
    let len = path.iter().position(|v| *v == 0).unwrap_or(path.len());
    Ok(PathBuf::from(String::from_utf16(&path[..len])?))
}

pub fn run() -> Result<()> {
    let mutex = unsafe { CreateMutexW(None, false, w!("Local\\DearVinking.AnimekoSMTC"))? };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            let _ = CloseHandle(mutex);
        }
        bail!("Animeko SMTC 已在运行，请先退出 Animeko。");
    }
    let result = run_inner();
    unsafe {
        let _ = CloseHandle(mutex);
    }
    result
}

fn run_inner() -> Result<()> {
    let data = PathBuf::from(std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA 不存在")?)
        .join("AnimekoSMTC");
    std::fs::create_dir_all(&data)?;
    let config = data.join("animeko-path.txt");
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let selected = launch::select_path(&args, &config, choose_executable)?;
    let exe = executable(&selected)?;
    let app_dir = std::env::current_exe()?
        .parent()
        .context("应用目录不存在")?
        .to_path_buf();
    let probe = app_dir.join("animeko_probe.dll");
    if !probe.is_file() {
        bail!("缺少 animeko_probe.dll，请完整解压便携包");
    }
    unsafe {
        if let Ok(existing) = windows::Win32::System::Threading::OpenMutexW(
            windows::Win32::System::Threading::SYNCHRONIZATION_ACCESS_RIGHTS(0x00100000),
            false,
            w!("AniAppSingleInstanceMutex"),
        ) {
            let _ = CloseHandle(existing);
            bail!("请先完全退出 Animeko（包括托盘），再通过 AnimekoSMTC 启动，以便加载播放探针。");
        }
    }
    launch::write_file(&config, exe.to_string_lossy().as_bytes())?;
    let mut app = RunningApp::start(&exe, &probe, data)?;
    while app.tick(Instant::now())?.is_continue() {
        if !process_messages() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn process_messages() -> bool {
    let mut message = MSG::default();
    unsafe {
        while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
            if message.message == WM_QUIT {
                return false;
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    true
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

#[cfg(test)]
mod tests {
    use super::*;
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
}
