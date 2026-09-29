use crate::access::{get, get_boolean, get_text, number, playback_rate, player, text};
use animeko_protocol::{Media, Playback};
use anyhow::Result;
use jni::{
    objects::{JObject, JValue},
    JNIEnv,
};

fn preferred_name(env: &mut JNIEnv<'_>, object: &JObject<'_>) -> Result<String> {
    let cn = get_text(env, object, "getNameCn")?;
    if !cn.trim().is_empty() {
        return Ok(cn);
    }
    get_text(env, object, "getName")
}

pub fn read(env: &mut JNIEnv<'_>, owner: &JObject<'_>) -> Result<Option<Media>> {
    match read_inner(env, owner) {
        Ok(media) => Ok(media),
        Err(error) => {
            let exception = env.exception_occurred()?;
            env.exception_clear()?;
            let detail = text(env, &exception).unwrap_or_default();
            let _ = env.exception_clear();
            Err(error.context(detail))
        }
    }
}

fn read_inner(env: &mut JNIEnv<'_>, owner: &JObject<'_>) -> Result<Option<Media>> {
    let sessions = get(env, owner, "getEpisodeSessionFlow")?;
    let session = get(env, &sessions, "getValue")?;
    let infos = get(env, &session, "getInfoBundleFlow")?;
    let replay = get(env, &infos, "getReplayCache")?;
    let size = env.call_method(&replay, "size", "()I", &[])?.i()?;
    if size == 0 {
        return Ok(None);
    }
    let info = env
        .call_method(
            &replay,
            "get",
            "(I)Ljava/lang/Object;",
            &[JValue::Int(size - 1)],
        )?
        .l()?;
    if info.is_null() {
        return Ok(None);
    }
    let player = player(env, owner)?;
    let state_flow = get(env, &player, "getState")?;
    let state = get(env, &state_flow, "getValue")?;
    let status = get_text(env, &state, "getMediaStatus")?;
    if status != "Ready" && status != "Ended" {
        return Ok(None);
    }
    let playing = get_boolean(env, &state, "isPlaying")?;
    let buffering = get_boolean(env, &state, "isBuffering")?;
    let playback = playback(&status, playing, buffering);
    let subject = get(env, &info, "getSubjectInfo")?;
    let episode = get(env, &info, "getEpisodeInfo")?;
    let subject_id = get(env, &info, "getSubjectId")?;
    let episode_id = get(env, &info, "getEpisodeId")?;
    let session_episode_id = get(env, &session, "getEpisodeId")?;
    let episode_id = number(env, &episode_id)?;
    if number(env, &session_episode_id)? != episode_id {
        return Ok(None);
    }
    let sort = get(env, &episode, "getSort")?;
    let cover = get(env, &subject, "getImageLarge")?;
    let position_flow = env
        .call_method(
            &player,
            "getCurrentPositionMillis",
            "()Lkotlinx/coroutines/flow/StateFlow;",
            &[],
        )?
        .l()?;
    let position = get(env, &position_flow, "getValue")?;
    let properties_flow = get(env, &player, "getMediaProperties")?;
    let properties = get(env, &properties_flow, "getValue")?;
    let duration = if properties.is_null() {
        None
    } else {
        let duration = get(env, &properties, "getDurationMillis")?;
        if duration.is_null() {
            None
        } else {
            Some(number(env, &duration)?)
        }
    };
    let mut media = Media {
        subject_id: number(env, &subject_id)?,
        episode_id,
        title: preferred_name(env, &subject)?,
        episode: format!("{} · {}", text(env, &sort)?, preferred_name(env, &episode)?),
        cover_url: text(env, &cover)?,
        playback,
        playback_rate: playback_rate(env, &player)?,
        play_when_ready: get_boolean(env, &state, "getPlayWhenReady")?,
        position_ms: number(env, &position)?,
        duration_ms: duration,
    };
    let final_session = get(env, &sessions, "getValue")?;
    let final_state = get(env, &state_flow, "getValue")?;
    if !env.is_same_object(&session, &final_session)?
        || !env.is_same_object(&state, &final_state)?
    {
        return Ok(None);
    }
    anyhow::ensure!(media.normalize(), "无效的媒体文本或播放速率");
    Ok(Some(media))
}

fn playback(status: &str, playing: bool, buffering: bool) -> Playback {
    if status == "Ended" {
        Playback::Stopped
    } else if buffering {
        Playback::Buffering
    } else if playing {
        Playback::Playing
    } else {
        Playback::Paused
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ended_takes_priority_over_buffering_and_playing() {
        for playing in [false, true] {
            for buffering in [false, true] {
                assert_eq!(playback("Ended", playing, buffering), Playback::Stopped);
            }
        }
    }

    #[test]
    fn ready_distinguishes_buffering_playing_and_paused() {
        for (playing, buffering, expected) in [
            (false, false, Playback::Paused),
            (true, false, Playback::Playing),
            (false, true, Playback::Buffering),
            (true, true, Playback::Buffering),
        ] {
            assert_eq!(playback("Ready", playing, buffering), expected);
        }
    }
}
