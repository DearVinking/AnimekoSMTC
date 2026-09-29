use anyhow::{ensure, Context, Result};
use objc2_foundation::{ns_string, NSBundle, NSString};
use std::{
    fs::{File, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Component, Path, PathBuf},
};

const ANIMEKO_BUNDLE_ID: &str = "me.him188.ani.app.desktop";

fn contained_executable(bundle: &Path, name: &str) -> Result<PathBuf> {
    let mut components = Path::new(name).components();
    ensure!(
        matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none(),
        "应用可执行文件名必须是单个路径分量"
    );
    let bundle = bundle.canonicalize()?;
    let macos = bundle.join("Contents/MacOS").canonicalize()?;
    let exe = macos.join(name).canonicalize()?;
    ensure!(macos.starts_with(&bundle), "应用目录存在路径逃逸");
    ensure!(
        exe.starts_with(&macos) && exe.is_file(),
        "无效的应用可执行路径"
    );
    ensure!(
        exe.metadata()?.permissions().mode() & 0o111 != 0,
        "应用文件没有执行权限"
    );
    Ok(exe)
}

fn executable(path: &Path) -> Result<PathBuf> {
    let path = path.canonicalize().context("Animeko 应用路径不存在")?;
    ensure!(
        path.extension().is_some_and(|e| e == "app"),
        "请选择 Ani.app 应用包"
    );
    let bundle = NSBundle::bundleWithPath(&NSString::from_str(
        path.to_str().context("应用路径不是有效 Unicode")?,
    ))
    .context("无法读取应用包信息")?;
    ensure!(
        bundle
            .bundleIdentifier()
            .is_some_and(|id| id.to_string() == ANIMEKO_BUNDLE_ID),
        "请选择 Animeko 的 Ani.app"
    );
    let name = bundle
        .objectForInfoDictionaryKey(ns_string!("CFBundleExecutable"))
        .context("应用包缺少 CFBundleExecutable")?;
    let name = name
        .downcast_ref::<NSString>()
        .context("CFBundleExecutable 必须是字符串")?;
    let exe = contained_executable(&path, &name.to_string())?;
    for required in [
        "Contents/app/Ani.cfg",
        "Contents/runtime/Contents/Home/lib/server/libjvm.dylib",
    ] {
        let file = path
            .join(required)
            .canonicalize()
            .with_context(|| format!("应用包缺少 {required}"))?;
        ensure!(
            file.starts_with(&path) && file.is_file(),
            "应用包文件存在路径逃逸：{required}"
        );
    }
    #[cfg(target_arch = "aarch64")]
    let architecture = 0x0100_000c;
    #[cfg(target_arch = "x86_64")]
    let architecture = 0x0100_0007;
    let architectures = bundle
        .executableArchitectures()
        .context("无法识别应用可执行文件架构")?;
    ensure!(
        (0..architectures.count())
            .any(|index| architectures.objectAtIndex(index).intValue() == architecture),
        "Animeko 与伴随程序架构不匹配，请使用相同架构的安装包"
    );
    Ok(exe)
}

fn prepare_data_directory(data: &Path) -> Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(data)?;
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(data)?;
    directory.set_permissions(std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

struct InstanceLock {
    _file: File,
}
impl InstanceLock {
    fn acquire(data: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(data.join("instance.lock"))?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        ensure!(
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "Animeko SMTC 已在运行，请先退出 Animeko。"
        );
        Ok(Self { _file: file })
    }
}

use crate::{app::RunningApp, launch};
use objc2::{
    define_class, msg_send, rc::Retained, runtime::ProtocolObject, sel, DefinedClass,
    MainThreadMarker, MainThreadOnly,
};
use objc2_app_kit::{
    NSAlert, NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate,
    NSApplicationTerminateReply, NSEvent, NSEventModifierFlags, NSEventType, NSModalResponseOK,
    NSOpenPanel, NSRunningApplication,
};
use objc2_foundation::{
    NSFileManager, NSObject, NSObjectProtocol, NSPoint, NSRunLoop, NSRunLoopCommonModes,
    NSSearchPathDirectory, NSSearchPathDomainMask, NSTimer,
};
use std::{
    cell::{Cell, RefCell},
    ops::ControlFlow,
    panic::{catch_unwind, AssertUnwindSafe},
    time::Instant,
};

fn data_directory() -> Result<PathBuf> {
    let support = NSFileManager::defaultManager()
        .URLForDirectory_inDomain_appropriateForURL_create_error(
            NSSearchPathDirectory::ApplicationSupportDirectory,
            NSSearchPathDomainMask::UserDomainMask,
            None,
            true,
        )?;
    let path = PathBuf::from(
        support
            .path()
            .context("Application Support 路径不可用")?
            .to_string(),
    )
    .join("AnimekoSMTC");
    prepare_data_directory(&path)?;
    Ok(path)
}

fn choose_application() -> Result<PathBuf> {
    let mtm = MainThreadMarker::new().context("应用选择器必须在主线程打开")?;
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setTitle(Some(ns_string!("选择 Animeko 的 Ani.app")));
    panel.setCanChooseDirectories(false);
    panel.setCanChooseFiles(true);
    panel.setAllowsMultipleSelection(false);
    panel.setTreatsFilePackagesAsDirectories(false);
    ensure!(panel.runModal() == NSModalResponseOK, "未选择 Animeko 程序");
    Ok(PathBuf::from(
        panel
            .URL()
            .and_then(|url| url.path())
            .context("所选应用路径不可用")?
            .to_string(),
    ))
}

fn reject_running_animeko() -> Result<()> {
    ensure!(
        NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(
            ANIMEKO_BUNDLE_ID
        ))
        .is_empty(),
        "请先完全退出 Animeko，再通过 AnimekoSMTC 启动，以便加载播放探针。"
    );
    Ok(())
}

pub(crate) fn show_error(message: &str) {
    let Some(mtm) = MainThreadMarker::new() else {
        eprintln!("{message}");
        return;
    };
    let _app = NSApplication::sharedApplication(mtm);
    let alert = NSAlert::new(mtm);
    alert.setMessageText(ns_string!("Animeko SMTC"));
    alert.setInformativeText(&NSString::from_str(message));
    alert.runModal();
}

pub(crate) fn run() -> Result<()> {
    let mtm = MainThreadMarker::new().context("Cocoa 必须在主线程运行")?;
    let application = NSApplication::sharedApplication(mtm);
    application.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let data = data_directory()?;
    let _lock = InstanceLock::acquire(&data)?;
    reject_running_animeko()?;
    let config = data.join("animeko-path.txt");
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let selected = launch::select_path(&args, &config, choose_application)?;
    let exe = executable(&selected)?;
    let own_exe = std::env::current_exe()?;
    let contents = own_exe
        .parent()
        .and_then(Path::parent)
        .context("伴随应用包目录无效")?;
    let probe = contents.join("Frameworks/libanimeko_probe.dylib");
    ensure!(
        probe.is_file(),
        "缺少 libanimeko_probe.dylib，请使用完整的 AnimekoSMTC.app"
    );
    ensure!(
        contents.join("Resources/ani.icns").is_file(),
        "缺少 Ani 图标，请使用完整的 AnimekoSMTC.app"
    );
    reject_running_animeko()?;
    launch::write_file(
        &config,
        selected
            .canonicalize()?
            .to_str()
            .context("应用路径不是有效 Unicode")?,
    )?;
    run_loop(RunningApp::start(&exe, &probe, data)?)
}

struct AppState {
    running: RefCell<Option<RunningApp>>,
    error: RefCell<Option<String>>,
    quitting: Cell<bool>,
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = AppState]
    struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}
    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationShouldTerminate:))]
        fn should_terminate(&self, _sender: &NSApplication) -> NSApplicationTerminateReply {
            self.ivars().quitting.set(true);
            NSApplicationTerminateReply::TerminateCancel
        }
    }
    impl AppDelegate {
        #[unsafe(method(tick:))]
        fn tick(&self, timer: &NSTimer) {
            let result = catch_unwind(AssertUnwindSafe(|| {
                let Ok(mut running) = self.ivars().running.try_borrow_mut() else { return None };
                if self.ivars().quitting.get() { return Some(Ok(ControlFlow::Break(()))) }
                running.as_mut().map(|running| running.tick(Instant::now()))
            }));
            let finish = match result {
                Ok(None | Some(Ok(ControlFlow::Continue(())))) => return,
                Ok(Some(Ok(ControlFlow::Break(())))) => None,
                Ok(Some(Err(error))) => Some(format!("{error:#}")),
                Err(_) => Some("macOS 主循环回调失败".into()),
            };
            timer.invalidate();
            self.ivars().running.borrow_mut().take();
            *self.ivars().error.borrow_mut() = finish;
            stop_application(self.mtm());
        }
    }
);

fn stop_application(mtm: MainThreadMarker) {
    let application = NSApplication::sharedApplication(mtm);
    application.stop(None);
    if let Some(event) = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
        NSEventType::ApplicationDefined, NSPoint::new(0.0, 0.0), NSEventModifierFlags::empty(), 0.0, 0, None, 0, 0, 0) {
        application.postEvent_atStart(&event, true);
    }
}

fn run_loop(running: RunningApp) -> Result<()> {
    let mtm = MainThreadMarker::new().context("Cocoa 必须在主线程运行")?;
    let application = NSApplication::sharedApplication(mtm);
    let delegate = AppDelegate::alloc(mtm).set_ivars(AppState {
        running: RefCell::new(Some(running)),
        error: RefCell::new(None),
        quitting: Cell::new(false),
    });
    let delegate: Retained<AppDelegate> = unsafe { msg_send![super(delegate), init] };
    application.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    let timer = unsafe {
        NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
            0.1,
            &delegate,
            sel!(tick:),
            None,
            true,
        )
    };
    unsafe { NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
    application.run();
    timer.invalidate();
    application.setDelegate(None);
    delegate.ivars().running.borrow_mut().take();
    let error = delegate.ivars().error.borrow_mut().take();
    if let Some(error) = error {
        anyhow::bail!(error)
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn run_loop_checks() {
    let data = std::env::temp_dir().join(format!("animeko-loop-{}", uuid::Uuid::new_v4()));
    prepare_data_directory(&data).unwrap();
    let app = crate::app::RunningApp::start(
        Path::new("/usr/bin/true"),
        Path::new("/tmp/unused-probe.dylib"),
        data.clone(),
    )
    .unwrap();
    run_loop(app).unwrap();
    std::fs::remove_dir_all(data).unwrap();
    println!("Verified Cocoa timer stops and releases the native session after child exit.");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("animeko 带 空格-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
        fn bundle(&self, id: &str, executable_value: &str) -> PathBuf {
            let bundle = self.0.join("Ani.app");
            for dir in ["MacOS", "app", "runtime/Contents/Home/lib/server"] {
                std::fs::create_dir_all(bundle.join("Contents").join(dir)).unwrap();
            }
            std::fs::copy(
                std::env::current_exe().unwrap(),
                bundle.join("Contents/MacOS/Ani"),
            )
            .unwrap();
            std::fs::write(bundle.join("Contents/app/Ani.cfg"), "[Application]").unwrap();
            std::fs::write(
                bundle.join("Contents/runtime/Contents/Home/lib/server/libjvm.dylib"),
                [],
            )
            .unwrap();
            std::fs::write(bundle.join("Contents/Info.plist"), format!(r#"<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>{id}</string><key>CFBundleExecutable</key>{executable_value}<key>CFBundlePackageType</key><string>APPL</string></dict></plist>"#)).unwrap();
            bundle
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn executable_is_contained_even_with_symlinks_and_unicode() {
        let fixture = Fixture::new();
        let bundle = fixture.bundle("me.him188.ani.app.desktop", "<string>Ani</string>");
        let expected = bundle.join("Contents/MacOS/Ani").canonicalize().unwrap();
        assert_eq!(contained_executable(&bundle, "Ani").unwrap(), expected);
        for name in ["../Ani", "/tmp/Ani", "", "other/Ani", ".", ".."] {
            assert!(contained_executable(&bundle, name).is_err(), "{name}");
        }
        let outside = fixture.0.join("outside");
        std::fs::write(&outside, []).unwrap();
        symlink(&outside, bundle.join("Contents/MacOS/Escape")).unwrap();
        assert!(contained_executable(&bundle, "Escape").is_err());
        std::fs::rename(bundle.join("Contents/MacOS"), fixture.0.join("escaped")).unwrap();
        symlink(fixture.0.join("escaped"), bundle.join("Contents/MacOS")).unwrap();
        assert!(contained_executable(&bundle, "Ani").is_err());
    }

    #[test]
    fn bundle_requires_identity_metadata_runtime_and_host_architecture() {
        let valid = Fixture::new();
        let bundle = valid.bundle("me.him188.ani.app.desktop", "<string>Ani</string>");
        assert_eq!(
            executable(&bundle).unwrap(),
            bundle.join("Contents/MacOS/Ani").canonicalize().unwrap()
        );
        for (id, name) in [
            ("other.app", "<string>Ani</string>"),
            ("me.him188.ani.app.desktop", "<integer>1</integer>"),
            ("me.him188.ani.app.desktop", "<string>../Ani</string>"),
        ] {
            let fixture = Fixture::new();
            assert!(executable(&fixture.bundle(id, name)).is_err());
        }
        for missing in [
            "app/Ani.cfg",
            "runtime/Contents/Home/lib/server/libjvm.dylib",
        ] {
            let fixture = Fixture::new();
            let bundle = fixture.bundle("me.him188.ani.app.desktop", "<string>Ani</string>");
            std::fs::remove_file(bundle.join("Contents").join(missing)).unwrap();
            assert!(executable(&bundle).is_err());
        }
        let invalid = Fixture::new();
        let bundle = invalid.bundle("me.him188.ani.app.desktop", "<string>Ani</string>");
        std::fs::write(
            bundle.join("Contents/MacOS/Ani"),
            b"not a Mach-O executable",
        )
        .unwrap();
        assert!(executable(&bundle).is_err());
    }

    #[test]
    fn lock_is_exclusive_released_by_drop_and_refuses_symlinks() {
        let fixture = Fixture::new();
        let first = InstanceLock::acquire(&fixture.0).unwrap();
        assert!(InstanceLock::acquire(&fixture.0).is_err());
        drop(first);
        drop(InstanceLock::acquire(&fixture.0).unwrap());
        let lock = fixture.0.join("instance.lock");
        assert_eq!(
            std::fs::metadata(&lock).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::remove_file(&lock).unwrap();
        let other = fixture.0.join("other");
        std::fs::write(&other, "untouched").unwrap();
        symlink(&other, &lock).unwrap();
        assert!(InstanceLock::acquire(&fixture.0).is_err());
        assert_eq!(std::fs::read_to_string(other).unwrap(), "untouched");
    }

    #[test]
    fn private_directory_does_not_follow_symlinks_or_change_parent_permissions() {
        let fixture = Fixture::new();
        let before = std::fs::metadata(&fixture.0).unwrap().permissions().mode();
        let data = fixture.0.join("data");
        prepare_data_directory(&data).unwrap();
        assert_eq!(
            std::fs::metadata(&data).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&fixture.0).unwrap().permissions().mode(),
            before
        );
        let link = fixture.0.join("link");
        symlink(data, &link).unwrap();
        assert!(prepare_data_directory(&link).is_err());
    }
}
