use crate::access::player;
use animeko_protocol::Action;
use anyhow::Result;
use jni::{
    objects::{JObject, JValue},
    JNIEnv,
};

pub fn enqueue(env: &mut JNIEnv<'_>, owner: &JObject<'_>, action: Action) -> Result<()> {
    let player = player(env, owner)?;
    let runnable = env.find_class("java/lang/Runnable")?;
    let method = env.new_string(match action {
        Action::Play => "play",
        Action::Pause => "pause",
    })?;
    let proxy = env
        .call_static_method(
            "java/beans/EventHandler",
            "create",
            "(Ljava/lang/Class;Ljava/lang/Object;Ljava/lang/String;)Ljava/lang/Object;",
            &[
                JValue::Object(&runnable),
                JValue::Object(&player),
                JValue::Object(&method),
            ],
        )?
        .l()?;
    env.call_static_method(
        "java/awt/EventQueue",
        "invokeLater",
        "(Ljava/lang/Runnable;)V",
        &[JValue::Object(&proxy)],
    )?;
    Ok(())
}
