use animeko_protocol::{
    Action, Command, CommandGate, Media, Packet, Playback, Receiver, MAX_PACKET,
};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

fn packet(sequence: u64) -> Value {
    json!({
        "version": 1, "token": "secret", "sequence": sequence,
        "diagnostic": "connected",
        "media": {
            "subject_id": 42, "episode_id": 7, "title": "标题", "episode": "07 · 剧集",
            "cover_url": "https://example.com/cover.jpg", "playback": "playing",
            "position_ms": 12000, "duration_ms": 24000
        }
    })
}

fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}

fn media() -> Media {
    serde_json::from_value(packet(0)["media"].clone()).unwrap()
}

fn command(sequence: u64) -> Command {
    Command {
        version: 1,
        token: "secret".into(),
        sequence,
        subject_id: 42,
        episode_id: 7,
        action: Action::Play,
    }
}

#[test]
fn heartbeat_expires_at_three_seconds_but_clear_packets_remain_connected() {
    let now = Instant::now();
    let mut receiver = Receiver::new("secret".into());
    assert!(receiver.ingest(&bytes(&packet(0)), now));
    assert!(receiver
        .current(now + Duration::from_millis(2999))
        .is_some());
    assert!(!receiver.connected(now + Duration::from_secs(3)));
    assert!(receiver.current(now + Duration::from_secs(3)).is_none());
    assert!(receiver.connected(now.checked_sub(Duration::from_secs(1)).unwrap()));

    let mut clear = packet(1);
    clear["media"] = Value::Null;
    assert!(receiver.ingest(&bytes(&clear), now + Duration::from_secs(3)));
    assert!(receiver.connected(now + Duration::from_secs(3)));
    assert!(receiver.current(now + Duration::from_secs(3)).is_none());
    assert_eq!(receiver.diagnostic, "connected");
}

#[test]
fn invalid_packets_do_not_commit_sequence_diagnostic_or_heartbeat() {
    let mut invalid = Vec::new();
    for (field, value) in [("version", json!(2)), ("token", json!("wrong"))] {
        let mut p = packet(2);
        p[field] = value;
        invalid.push(p);
    }
    for (field, value) in [
        ("title", json!(" \t\n")),
        ("title", json!("x".repeat(4097))),
        ("episode", json!("x".repeat(4097))),
    ] {
        let mut p = packet(2);
        p["media"][field] = value;
        invalid.push(p);
    }
    for mut p in invalid {
        let now = Instant::now();
        let mut receiver = Receiver::new("secret".into());
        assert!(receiver.ingest(&bytes(&packet(1)), now));
        p["diagnostic"] = json!("must not be stored");
        assert!(!receiver.ingest(&bytes(&p), now + Duration::from_secs(2)));
        assert_eq!(receiver.diagnostic, "connected");
        assert!(!receiver.connected(now + Duration::from_secs(3)));
        assert!(receiver.ingest(&bytes(&packet(2)), now + Duration::from_secs(3)));
    }
}

#[test]
fn packet_size_and_text_limits_count_utf8_bytes() {
    let now = Instant::now();
    let mut receiver = Receiver::new("secret".into());
    let mut p = packet(0);
    p["media"]["title"] = json!("界".repeat(1365) + "a");
    p["media"]["episode"] = json!("x".repeat(4096));
    let mut data = bytes(&p);
    data.resize(MAX_PACKET - 1, b' ');
    assert!(receiver.ingest(&data, now));
    p["sequence"] = json!(1);
    let mut exact_limit = bytes(&p);
    exact_limit.resize(MAX_PACKET, b' ');
    assert!(!receiver.ingest(&exact_limit, now));
    p["media"]["title"] = json!("界".repeat(1366));
    assert!(!receiver.ingest(&bytes(&p), now));
}

#[test]
fn normalization_preserves_unknown_duration_and_bounds_position() {
    for (duration, position, expected_duration, expected_position) in [
        (None, -1, None, 0),
        (Some(0), 100, None, 100),
        (Some(-1), 100, None, 100),
        (None, i64::MAX, None, 604_800_000),
        (Some(100), 101, Some(100), 100),
        (Some(i64::MAX), i64::MAX, Some(604_800_000), 604_800_000),
    ] {
        let mut m = media();
        m.duration_ms = duration;
        m.position_ms = position;
        m.normalize();
        assert_eq!(
            (m.duration_ms, m.position_ms),
            (expected_duration, expected_position)
        );
    }
}

#[test]
fn covers_require_lowercase_https_and_at_most_2048_bytes() {
    for (url, valid) in [
        ("https://example.com/cover".to_owned(), true),
        ("https://".to_owned() + &"x".repeat(2040), true),
        ("https://".to_owned() + &"x".repeat(2041), false),
        ("HTTPS://example.com".to_owned(), false),
        ("http://example.com".to_owned(), false),
        ("file:///cover.jpg".to_owned(), false),
    ] {
        let mut m = media();
        m.cover_url = url.clone();
        m.normalize();
        assert_eq!(m.cover_url, if valid { url } else { String::new() });
    }
}

#[test]
fn authenticated_commands_without_matching_media_still_consume_sequence() {
    for current in [
        None,
        Some(Media {
            episode_id: 8,
            ..media()
        }),
    ] {
        let mut gate = CommandGate::new("secret".into());
        let data = serde_json::to_vec(&command(1)).unwrap();
        assert_eq!(gate.accept(&data, current.as_ref()), None);
        assert_eq!(gate.accept(&data, Some(&media())), None);
        assert_eq!(
            gate.accept(&serde_json::to_vec(&command(2)).unwrap(), Some(&media())),
            Some(Action::Play)
        );
    }
}

#[test]
fn rejected_command_authentication_and_size_do_not_consume_sequence() {
    let mut gate = CommandGate::new("secret".into());
    let mut c = command(1);
    c.version = 2;
    assert_eq!(
        gate.accept(&serde_json::to_vec(&c).unwrap(), Some(&media())),
        None
    );
    c.version = 1;
    c.token = "wrong".into();
    assert_eq!(
        gate.accept(&serde_json::to_vec(&c).unwrap(), Some(&media())),
        None
    );
    c.token = "secret".into();
    let mut data = serde_json::to_vec(&c).unwrap();
    data.resize(MAX_PACKET, b' ');
    assert_eq!(gate.accept(&data, Some(&media())), None);
    data.pop();
    assert_eq!(gate.accept(&data, Some(&media())), Some(Action::Play));
    assert_eq!(gate.accept(&data, Some(&media())), None);
}

#[test]
fn wire_contract_retains_enum_names_optional_media_and_unknown_fields() {
    let c = command(1);
    assert_eq!(
        serde_json::to_value(c).unwrap(),
        json!({
            "version": 1, "token": "secret", "sequence": 1,
            "subject_id": 42, "episode_id": 7, "action": "play"
        })
    );
    for (state, name) in [
        (Playback::Playing, "playing"),
        (Playback::Paused, "paused"),
        (Playback::Buffering, "buffering"),
        (Playback::Stopped, "stopped"),
    ] {
        assert_eq!(serde_json::to_value(state).unwrap(), json!(name));
    }
    assert_eq!(serde_json::to_value(Action::Pause).unwrap(), json!("pause"));
    let mut p = packet(0);
    p["future_field"] = json!(true);
    p.as_object_mut().unwrap().remove("media");
    let decoded: Packet = serde_json::from_value(p.clone()).unwrap();
    assert!(decoded.media.is_none());
    p.as_object_mut().unwrap().remove("token");
    assert!(serde_json::from_value::<Packet>(p).is_err());
}
