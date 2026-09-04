# 音栖岛 · YinQiDao

音栖岛是一款使用 Rust 和 GPUI 构建的桌面音乐播放器。

本项目采用 [GNU GPL v3 或更高版本](LICENSE) 开源。

当前版本是以本地播放为主、带可选在线信息增强的可交互音乐播放器 v1：

- 中文品牌名固定为“音栖岛”，Cargo 包名为 `yin_qi_dao`。
- UI 使用 [Better-Minecraft-Bedrock-Launcher](https://github.com/Chlna6666/Better-Minecraft-Bedrock-Launcher) 的 `master` 分支中的 `crates/gpui`。
- 图标使用 BMCBL 的 `crates/lucide-gpui`，通过 GPUI `AssetSource` 注册 SVG 资源，不使用 Unicode 字符充当控件图标。
- 异步运行时通过同一仓库的 `crates/gpui_tokio` 初始化，承载目录选择、歌库扫描、元数据和播放控制任务。
- Symphonia 启用全格式、全编解码器和元数据支持；CPAL 负责跨平台音频输出，播放线程与 CPAL 回调线程分离。
- 首页、歌曲库、专辑、艺术家、歌单、设置和独立沉浸式播放页均可交互；支持搜索、队列、播放控制、循环/随机和键盘快捷键。
- 音乐目录由 SQLite 索引并使用 `notify` 监听增量变化；Lofty 读取音频元数据，封面后台解码并缓存为缩略图。扫描采用增量索引、有限计算线程和事件合并，不会把扫描任务直接堆到 UI 或播放线程。
- 在线信息采用网易云音乐、QQ 音乐、Spotify、咪咕音乐、千千音乐、酷狗音乐的优先级链；命中后补充标题、艺术家、专辑、发行年份、封面和歌词，并在平台不可用时回退到 LRCLIB/MusicBrainz。
- 歌词支持同步 LRC、纯文本和原文/翻译双语时间轴；Spotify Web API 需要设置 `YINQIDAO_SPOTIFY_CLIENT_ID` 与 `YINQIDAO_SPOTIFY_CLIENT_SECRET` 后才会参与搜索。
- 设置页支持输出设备、音量、10 段 EQ、预设、低延迟立体声空间化和动态模糊开关。
- 播放页采用“背景 → 模糊层 → 前景内容”的 GPUI 绘制顺序；入场、按压和环境光动画遵循 Apple 风格的短时、低幅度、可关闭原则。
- `vendor/gpu-allocator` 是项目内本地补丁，不依赖开发机的绝对路径。

## 运行

需要 Rust 1.89 或更高版本。在 Windows 上执行：

```powershell
cargo run
```

当前不包含账号登录、DRM、付费资源下载和 FFmpeg；在线平台接口均为公开可访问的非官方接口，可能受地区、频率和服务变更影响。遇到 Symphonia 不支持或损坏的文件时，歌库会保留可理解的失败原因并继续扫描其他文件。
