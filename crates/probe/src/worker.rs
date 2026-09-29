use super::{report, DIAGNOSTIC, JVM, OWNER, STOP};
use crate::{control, snapshot};
use animeko_protocol::{CommandGate, Media, Packet, MAX_PACKET, VERSION};
use jni::{objects::GlobalRef, JNIEnv};
use std::{net::UdpSocket, sync::atomic::Ordering, thread, time::Duration};

fn connect() -> Option<(UdpSocket, String)> {
    let endpoint = std::env::var("ANIMEKO_SMTC_ENDPOINT").ok()?;
    let address = endpoint.parse::<std::net::SocketAddr>().ok()?;
    if !address.ip().is_loopback() {
        return None;
    }
    let token = std::env::var("ANIMEKO_SMTC_TOKEN").ok()?;
    let socket = UdpSocket::bind("127.0.0.1:0").ok()?;
    socket.connect(address).ok()?;
    socket.set_nonblocking(true).ok()?;
    Some((socket, token))
}

fn sample(env: &mut JNIEnv<'_>, owner: Option<&GlobalRef>) -> Option<Media> {
    let owner = owner?;
    match env.with_local_frame(512, |env| snapshot::read(env, owner.as_obj())) {
        Ok(media) => {
            report(if media.is_some() {
                "正在同步播放信息"
            } else {
                "等待媒体加载"
            });
            media
        }
        Err(error) => {
            let _ = env.exception_clear();
            report(format!("读取播放信息失败: {error:#}"));
            None
        }
    }
}

pub(super) fn poll() {
    let Some(vm) = JVM.get() else {
        return;
    };
    let Ok(mut env) = vm.attach_current_thread_as_daemon() else {
        return;
    };
    let Some((socket, token)) = connect() else {
        return;
    };
    let mut commands = CommandGate::new(token.clone());
    let mut command_buffer = [0u8; MAX_PACKET];
    let mut sequence = 0;
    while !STOP.load(Ordering::Acquire) {
        let owner = OWNER.lock().ok().and_then(|value| value.clone());
        let media = sample(&mut env, owner.as_ref());
        for _ in 0..16 {
            let Ok(size) = socket.recv(&mut command_buffer) else {
                break;
            };
            if let Some(action) = commands.accept(&command_buffer[..size], media.as_ref()) {
                if let Some(owner) = owner.as_ref() {
                    let result = env
                        .with_local_frame(32, |env| control::enqueue(env, owner.as_obj(), action));
                    if let Err(error) = result {
                        let _ = env.exception_clear();
                        report(format!("发送播放指令失败: {error:#}"));
                    }
                }
            }
        }
        let packet = Packet {
            version: VERSION,
            token: token.clone(),
            sequence,
            media,
            diagnostic: DIAGNOSTIC.lock().map(|v| v.clone()).unwrap_or_default(),
        };
        if let Ok(data) = serde_json::to_vec(&packet) {
            if data.len() < MAX_PACKET {
                let _ = socket.send(&data);
            }
        }
        sequence += 1;
        thread::sleep(Duration::from_millis(500));
    }
}
