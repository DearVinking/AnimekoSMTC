use anyhow::{Context, Result};
use jni::{
    objects::{JObject, JString, JValue},
    JNIEnv,
};

// Reflection keeps the probe independent of Kotlin's generated getter return descriptors.
pub(crate) fn get<'a>(
    env: &mut JNIEnv<'a>,
    object: &JObject<'_>,
    name: &str,
) -> Result<JObject<'a>> {
    invoke_getter(env, object, name).with_context(|| format!("getter {name}"))
}

fn invoke_getter<'a>(
    env: &mut JNIEnv<'a>,
    object: &JObject<'_>,
    name: &str,
) -> Result<JObject<'a>> {
    // Kotlin Flow wrappers are package-private. JNI can call their public interface
    // methods without Java reflection's caller-frame access checks.
    let flow_signature = match name {
        "getValue" => Some("()Ljava/lang/Object;"),
        "getReplayCache" => Some("()Ljava/util/List;"),
        _ => None,
    };
    if let Some(signature) = flow_signature {
        return Ok(env.call_method(object, name, signature, &[])?.l()?);
    }
    let class = env.get_object_class(object)?;
    let name_string = env.new_string(name)?;
    let method = env
        .call_method(
            class,
            "getMethod",
            "(Ljava/lang/String;[Ljava/lang/Class;)Ljava/lang/reflect/Method;",
            &[
                JValue::Object(&name_string),
                JValue::Object(&JObject::null()),
            ],
        )?
        .l()
        .with_context(|| format!("getter {name}"))?;
    env.call_method(&method, "setAccessible", "(Z)V", &[JValue::Bool(1)])?;
    Ok(env
        .call_method(
            method,
            "invoke",
            "(Ljava/lang/Object;[Ljava/lang/Object;)Ljava/lang/Object;",
            &[JValue::Object(object), JValue::Object(&JObject::null())],
        )?
        .l()?)
}

pub(crate) fn text(env: &mut JNIEnv<'_>, object: &JObject<'_>) -> Result<String> {
    if object.is_null() {
        return Ok(String::new());
    }
    let string = JString::from(
        env.call_method(object, "toString", "()Ljava/lang/String;", &[])?
            .l()?,
    );
    let result = env.get_string(&string)?.into();
    Ok(result)
}

pub(crate) fn number(env: &mut JNIEnv<'_>, object: &JObject<'_>) -> Result<i64> {
    Ok(env.call_method(object, "longValue", "()J", &[])?.j()?)
}

pub(crate) fn boolean(env: &mut JNIEnv<'_>, object: &JObject<'_>) -> Result<bool> {
    Ok(env.call_method(object, "booleanValue", "()Z", &[])?.z()?)
}

pub(crate) fn player<'local>(
    env: &mut JNIEnv<'local>,
    owner: &JObject<'_>,
) -> Result<JObject<'local>> {
    let session = get(env, owner, "getPlayerSession")?;
    get(env, &session, "getPlayer")
}

pub(crate) fn get_text(env: &mut JNIEnv<'_>, object: &JObject<'_>, name: &str) -> Result<String> {
    let value = get(env, object, name)?;
    text(env, &value)
}

pub(crate) fn get_boolean(env: &mut JNIEnv<'_>, object: &JObject<'_>, name: &str) -> Result<bool> {
    let value = get(env, object, name)?;
    boolean(env, &value)
}
