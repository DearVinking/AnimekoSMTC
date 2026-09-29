mod smtc;

use crate::launch;
use animeko_protocol::{Command, Media, Receiver, MAX_PACKET};
use anyhow::{bail, Context, Result};
use smtc::SmtcSession;
use std::{
    mem::size_of,
    net::{SocketAddr, UdpSocket},
    path::PathBuf,
    sync::mpsc,
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
    let exe = launch::executable(&selected)?;
    let app_dir = std::env::current_exe()?
        .parent()
        .context("应用目录不存在")?
        .to_path_buf();
    let probe = app_dir.join("animeko_probe.dll");
    if !probe.is_file() {
        bail!("缺少 animeko_probe.dll，请完整解压便携包");
    }
    // Animeko's single-instance mutex identifies an already running uninstrumented process.
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
    std::fs::write(config, exe.to_string_lossy().as_bytes())?;
    let socket = UdpSocket::bind("127.0.0.1:0")?;
    socket.set_nonblocking(true)?;
    let token = uuid::Uuid::new_v4().to_string();
    let mut receiver = Receiver::new(token.clone());
    let (actions_tx, actions_rx) = mpsc::channel();
    let smtc = SmtcSession::new(actions_tx)?;
    let mut child = launch::start(
        &exe,
        &probe,
        &socket.local_addr()?.to_string(),
        &token,
        &data.join("animeko-startup.log"),
    )?;
    let start = Instant::now();
    let mut connected_once = false;
    let mut last_media: Option<Media> = None;
    let mut probe_address = None;
    let mut command_sequence = 0;
    let mut last_diagnostic = String::new();
    let mut buffer = [0u8; MAX_PACKET];
    loop {
        if !process_messages() {
            return Ok(());
        }
        receive_packets(&socket, &mut receiver, &mut probe_address, &mut buffer)?;
        let now = Instant::now();
        connected_once |= receiver.connected(now);
        let media = receiver.current(now);
        for action in actions_rx.try_iter().take(16) {
            if let (Some(media), Some(peer)) = (media, probe_address) {
                let command = Command {
                    version: 1,
                    token: token.clone(),
                    sequence: command_sequence,
                    subject_id: media.subject_id,
                    episode_id: media.episode_id,
                    action,
                };
                socket.send_to(&serde_json::to_vec(&command)?, peer)?;
                command_sequence += 1;
            }
        }
        if media != last_media.as_ref() {
            smtc.update(media, last_media.as_ref())?;
            last_media = media.cloned();
        }
        let diagnostic = if !receiver.connected(now) && connected_once {
            "探针已断开连接"
        } else {
            &receiver.diagnostic
        };
        if diagnostic != last_diagnostic {
            std::fs::write(data.join("status.txt"), diagnostic)?;
            last_diagnostic = diagnostic.to_owned();
        }
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        if !connected_once && start.elapsed() > Duration::from_secs(30) {
            bail!(
                "30 秒内未收到 JVM 探针心跳。请查看 {}。Animeko 保持运行。",
                data.join("animeko-startup.log").display()
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
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

fn receive_packets(
    socket: &UdpSocket,
    receiver: &mut Receiver,
    probe_address: &mut Option<SocketAddr>,
    buffer: &mut [u8; MAX_PACKET],
) -> Result<()> {
    for _ in 0..64 {
        match socket.recv_from(buffer) {
            Ok((size, peer)) if peer.ip().is_loopback() => {
                if receiver.ingest(&buffer[..size], Instant::now()) {
                    *probe_address = Some(peer);
                }
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
