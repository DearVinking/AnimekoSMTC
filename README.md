# Animeko SMTC

让 [Animeko](https://github.com/open-ani/animeko) 支持使用 SMTC 功能显示正在播放的番剧、剧集、封面和播放进度。

## 使用

1. 解压便携包，将 `AnimekoSMTC.exe` 和 `animeko_probe.dll` 放在同一目录，完全退出 Animeko。
2. 双击 `AnimekoSMTC.exe`，选择 Animeko 安装目录里的 `Ani.exe`。
3. Animeko 自动启动，开始播放后即可显示媒体信息。安装路径会自动记住。

也可以直接指定安装目录：

```powershell
.\AnimekoSMTC.exe --animeko 'D:\Ani'
```

更换安装位置时运行 `AnimekoSMTC.exe --choose`。

## 注意

- 每次需要通过 Animeko SMTC 启动 Animeko。

## 排错

连接状态和错误详情保存在 `%LOCALAPPDATA%\AnimekoSMTC\status.txt`，启动日志保存在同目录的 `animeko-startup.log`。

## 许可

MIT
