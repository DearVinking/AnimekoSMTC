# Animeko SMTC

为 [Animeko](https://github.com/open-ani/animeko) 提供 Windows SMTC 和 macOS Now Playing 集成，显示番剧、剧集、封面、播放状态和进度，支持系统播放/暂停。

## Windows 使用

1. 完整解压便携包，保留同目录的 `AnimekoSMTC.exe` 和 `animeko_probe.dll`。
2. 完全退出 Animeko（包括托盘），双击 `AnimekoSMTC.exe`，选择安装目录里的 `Ani.exe`。
3. Animeko 自动启动，开始播放后即可显示媒体信息。安装路径会自动记住。

```powershell
.\AnimekoSMTC.exe --animeko 'D:\Ani'
.\AnimekoSMTC.exe --choose
```

## macOS 使用

1. 选择与 Animeko 相同架构的包：Apple Silicon 使用 `arm64`，Intel 使用 `x86_64`。解压后将完整的 `AnimekoSMTC.app` 放到固定位置。
2. 完全退出已运行的 Animeko，双击 `AnimekoSMTC.app`，首次选择 `Ani.app`。

更换安装位置时可重新选择：

```sh
'/Applications/AnimekoSMTC.app/Contents/MacOS/AnimekoSMTC' --choose
'/Applications/AnimekoSMTC.app/Contents/MacOS/AnimekoSMTC' --animeko '/Applications/Ani.app'
```

macOS 最低部署目标为 11.0。不同系统面板显示的字段、封面布局和进度控件由系统决定；目前提供播放、暂停、切换播放状态，不支持从系统面板拖动进度或切集。暂停、缓冲时进度停止推进，重新播放后按 Animeko 的实际倍速更新。

## 排错

配置、连接状态 `status.txt` 和启动日志 `animeko-startup.log` 位于：

- Windows：`%LOCALAPPDATA%\AnimekoSMTC\`
- macOS：`~/Library/Application Support/AnimekoSMTC/`

## 从源码构建

需要 Rust（MSRV 1.85）、完整 JDK 21；macOS 还需要 Xcode Command Line Tools，Windows 需要 MSVC 和 Windows SDK。`JAVA_HOME` 必须指向含 `include/jvmti.h` 的 JDK。

```sh
cargo build --workspace --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
python3 tests/probe_integration.py
```

macOS 生成应用包（Intel 将 target 改成 `x86_64-apple-darwin`）：

```sh
export MACOSX_DEPLOYMENT_TARGET=11.0
cargo build --workspace --release --locked --target aarch64-apple-darwin
python3 scripts/package-macos.py --target aarch64-apple-darwin --profile release
```

输出位于 `dist/`：完整 `.app`、按架构命名的 ZIP 和 `.sha256`。打包脚本只包装已构建的二进制，检查架构并验证签名，不会发布发行版。

普通测试不会注册系统媒体会话。在有桌面的 macOS 上，可显式运行原生字典、控制、封面及生命周期检查；该测试会暂时发布测试媒体信息：

```sh
ANIMEKO_RUN_NATIVE_TESTS=1 cargo test -p animeko-smtc --test macos_session --locked
```

CI 配置覆盖 Windows、Linux 和 macOS 两种架构，并检查 stable / Rust 1.85、真实 JVM 探针及依赖审计；系统控制中心的实际显示仍需桌面验收。

## 许可

Rust 源码采用 MIT。Animeko 图标采用 AGPL-3.0，来源和许可见 `THIRD-PARTY-NOTICES.txt` 与 `licenses/`。
