mod access;
mod control;
mod snapshot;
mod worker;
#[allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code,
    clippy::all
)]
mod jvmti {
    include!(concat!(env!("OUT_DIR"), "/jvmti.rs"));
}

use jni::{
    objects::{GlobalRef, JClass, JObject},
    sys, JNIEnv, JavaVM,
};
use std::{
    ffi::{c_char, c_void, CStr},
    ptr,
    sync::{
        atomic::{AtomicBool, AtomicPtr, Ordering},
        Mutex, OnceLock,
    },
    thread::{self, JoinHandle},
};

static OWNER: Mutex<Option<GlobalRef>> = Mutex::new(None);
static READY_METHOD: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static CLOSE_METHOD: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static STOP: AtomicBool = AtomicBool::new(false);
static WORKER: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);
static JVM: OnceLock<JavaVM> = OnceLock::new();
static DIAGNOSTIC: Mutex<String> = Mutex::new(String::new());

fn report(message: impl Into<String>) {
    if let Ok(mut value) = DIAGNOSTIC.lock() {
        *value = message.into();
    }
}

fn callback(f: impl FnOnce() + std::panic::UnwindSafe) {
    if std::panic::catch_unwind(f).is_err() {
        report("探针回调失败");
    }
}

/// # Safety
/// The JVM must call this entry point with its valid JavaVM pointer during agent
/// loading. The function tables and event callback arguments must satisfy the
/// JNI and JVMTI contracts for the entire lifetime of the registered callbacks.
#[no_mangle]
pub unsafe extern "system" fn Agent_OnLoad(
    vm: *mut sys::JavaVM,
    _: *mut c_char,
    _: *mut c_void,
) -> sys::jint {
    callback(|| {
        if std::env::var("ANIMEKO_SMTC_ENDPOINT").is_err() {
            return;
        }
        let Ok(java) = JavaVM::from_raw(vm) else {
            return;
        };
        if JVM.set(java).is_err() {
            return;
        }
        let mut raw: *mut c_void = ptr::null_mut();
        let result = ((**vm).GetEnv.unwrap())(vm, &mut raw, 0x30010200);
        if result != 0 || raw.is_null() {
            report("JVMTI 1.2 不可用");
            return;
        }
        let env = raw as *mut jvmti::jvmtiEnv;
        let mut caps = jvmti::jvmtiCapabilities::default();
        caps.set_can_access_local_variables(1);
        caps.set_can_generate_breakpoint_events(1);
        if ((**env).AddCapabilities.unwrap())(env, &caps) != 0 {
            report("JVM 不支持所需 JVMTI 能力");
            return;
        }
        let callbacks = jvmti::jvmtiEventCallbacks {
            VMInit: Some(vm_init),
            VMDeath: Some(vm_death),
            ClassPrepare: Some(class_prepare),
            Breakpoint: Some(breakpoint),
            ..Default::default()
        };
        if ((**env).SetEventCallbacks.unwrap())(
            env,
            &callbacks,
            size_of::<jvmti::jvmtiEventCallbacks>() as i32,
        ) != 0
        {
            return;
        }
        for event in [50, 51, 56, 62] {
            if ((**env).SetEventNotificationMode.unwrap())(env, 1, event, ptr::null_mut()) != 0 {
                report(format!("无法启用 JVMTI 事件 {event}"));
            }
        }
    });
    sys::JNI_OK
}

unsafe extern "C" fn vm_init(_: *mut jvmti::jvmtiEnv, _: *mut jvmti::JNIEnv, _: jvmti::jthread) {
    callback(|| {
        if let Ok(mut worker) = WORKER.lock() {
            *worker = thread::Builder::new()
                .name("animeko-smtc-probe".into())
                .spawn(worker::poll)
                .ok();
        }
    });
}

unsafe extern "C" fn vm_death(_: *mut jvmti::jvmtiEnv, _: *mut jvmti::JNIEnv) {
    callback(|| {
        STOP.store(true, Ordering::Release);
        if let Ok(mut worker) = WORKER.lock() {
            if let Some(worker) = worker.take() {
                let _ = worker.join();
            }
        }
        if let Ok(mut owner) = OWNER.lock() {
            *owner = None;
        }
    });
}

unsafe extern "C" fn class_prepare(
    env: *mut jvmti::jvmtiEnv,
    raw_jni: *mut jvmti::JNIEnv,
    _: jvmti::jthread,
    class: jvmti::jclass,
) {
    callback(|| {
        let mut signature = ptr::null_mut();
        if ((**env).GetClassSignature.unwrap())(env, class, &mut signature, ptr::null_mut()) != 0 {
            return;
        }
        if signature.is_null() {
            return;
        }
        let matches = CStr::from_ptr(signature).to_bytes()
            == b"Lme/him188/ani/app/domain/episode/EpisodeFetchSelectPlayState;";
        ((**env).Deallocate.unwrap())(env, signature.cast());
        if !matches {
            return;
        }
        let Ok(mut jni) = JNIEnv::from_raw(raw_jni.cast()) else {
            return;
        };
        let class = JClass::from_raw(class.cast());
        for (name, descriptor, slot) in [
            ("onUIReady", "()V", &READY_METHOD),
            (
                "onClose",
                "(Lkotlin/coroutines/Continuation;)Ljava/lang/Object;",
                &CLOSE_METHOD,
            ),
        ] {
            match jni.get_method_id(&class, name, descriptor) {
                Ok(method) => {
                    let method = method.into_raw();
                    slot.store(method.cast(), Ordering::Release);
                    if ((**env).SetBreakpoint.unwrap())(env, method.cast(), 0) != 0 {
                        report(format!("无法观察 {name}"));
                    }
                }
                Err(_) => {
                    let _ = jni.exception_clear();
                    report(format!("当前 Animeko 缺少 {name}，需要更新兼容配置"));
                }
            }
        }
        if READY_METHOD.load(Ordering::Acquire).is_null() {
            return;
        }
        report("已识别 Animeko 播放模块，等待播放");
    });
}

unsafe extern "C" fn breakpoint(
    env: *mut jvmti::jvmtiEnv,
    raw_jni: *mut jvmti::JNIEnv,
    thread: jvmti::jthread,
    method: jvmti::jmethodID,
    _: jvmti::jlocation,
) {
    callback(|| {
        let ready = method.cast() == READY_METHOD.load(Ordering::Acquire);
        let close = method.cast() == CLOSE_METHOD.load(Ordering::Acquire);
        if !ready && !close {
            return;
        }
        let mut object = ptr::null_mut();
        if ((**env).GetLocalObject.unwrap())(env, thread, 0, 0, &mut object) != 0 {
            report("读取播放对象失败");
            return;
        }
        let Ok(jni) = JNIEnv::from_raw(raw_jni.cast()) else {
            return;
        };
        let object = JObject::from_raw(object.cast());
        if let Ok(mut owner) = OWNER.lock() {
            if ready {
                if let Ok(reference) = jni.new_global_ref(&object) {
                    *owner = Some(reference);
                }
            } else if owner
                .as_ref()
                .is_some_and(|value| jni.is_same_object(value.as_obj(), &object).unwrap_or(false))
            {
                *owner = None;
            }
        }
        let _ = jni.delete_local_ref(object);
    });
}
