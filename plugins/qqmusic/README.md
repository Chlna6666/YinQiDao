# QQ 音乐 Provider 插件

基于 QQ 音乐 Web 接口（musicu.fcg 模块化网关 + 老 fcgi）的 YinQiDao 插件，与网易云插件共享同一份 `yinqidao:music-plugin` WIT 契约——宿主对两者一视同仁，无任何 NetEase/QQ 写死的分支。

## 能力矩阵（与其他插件共存时的行为约定）

| 能力 | QQ音乐（本插件） | 与网易云同时启用时 |
|---|---|---|
| authentication | Cookie 导入（music.y.qq.com 登录后复制 Cookie；最小集 `uin + qm_keyst`） | 各自独立登录、独立账户、互不干扰 |
| search | `music.search.SearchCgiService`（特殊信封：key=module、无 comm） | 搜索扇出同时发给所有健康来源，结果按 provider 去重合并 |
| streaming | `vkey.GetVkeyServer/CgiGetVkey`（游客 M500 / 登录 M800 / 绿钻 F000 探测） | 各曲目按其 `source.provider_id` 回源，不跨源解析 |
| lyrics | 老 fcgi LRC + 翻译（base64） | 曲目各自的来源提供歌词 |
| playlists | 创建/收藏列表 + “我喜欢”（dirid 201） | 侧栏「创建的歌单/收藏的歌单」混合展示，多来源时带来源标识 |
| media_collections | 歌单/专辑/歌手详情与曲目；歌手→专辑 | 通用入口；Album/Artist 无“我的列表”语义（返回空页） |
| recommendations | 猜你喜欢（匿名可用）/ 新歌速递 / 推荐歌单广场 | 发现页与首页推荐按路由优先级取源 |
| user_profile | 主页 fcg（昵称/头像/等级） | 设置页按插件分行展示 |
| like_sync / cloud_library / recognition / playback_events | **未声明**（红心写入依赖 ag-1 加密通道，待后续版本） | 宿主按能力声明隐藏入口，不会对不可用能力发起调用 |

**可以同时使用**：登录态、搜索、播放、歌单浏览/读取、推荐——全部多来源安全（每条数据自带 `source.provider_id`，写操作只回到来源路由）。
**不可以/暂不支持**：跨来源解析（QQ 歌目不会回退到网易云 vkey，反之亦然）；红心与云盘同步（QQ 侧能力未实现，网易云侧不受影响）。

## 接口说明

- 网关：`POST https://u.y.qq.com/cgi-bin/musicu.fcg`，必须带 `Referer: https://y.qq.com/`（缺失 → code -1310）。
- 游客态 `comm.uin="0"、g_tk=5381`；登录态 `comm.uin=<uin>`、`comm.authst=<qm_keyst>` + Cookie（uin/qm_keyst/euin）；g_tk = hash33(qm_keyst, 5381)。
- 搜索模块特殊信封：请求 key = module 名、不带顶层 comm；`code 2001` 瞬时错误自动重试一次。
- Cookie 约 3 天过期（社区经验）；失效表现为 `code 1000/104401/104400`，重新导入即可。
- 红心写入需要 `zzc` 签名 + `ag-1` AES-GCM 加密通道（`u6.y.qq.com/musics.fcg`），实现留待后续版本；届时以独立 `crypto/sign.rs` 落地。

参考资料：`luren-dc/QQMusicApi`（接口形状权威）、`jixunmoe/qmweb-sign`（签名/加密）、`WaveForge/simple-music` 文档（2026-08~10 实测可用性）。

## 构建

```powershell
cargo check --manifest-path plugins/qqmusic/Cargo.toml --locked
cargo test  --manifest-path plugins/qqmusic/Cargo.toml --locked
cargo check --manifest-path plugins/qqmusic/Cargo.toml --target wasm32-wasip2 --locked
# release 构建（CI 同款）
cargo build --manifest-path plugins/qqmusic/Cargo.toml --target wasm32-wasip2 --release --locked
```

构建产物：`plugins/qqmusic/target/wasm32-wasip2/release/yinqidao_qqmusic_plugin.wasm`。

`wasm32-wasip2` 目标通常直接生成 WebAssembly Component，无需再运行
`wasm-tools component new`。在「插件中心 → 导入插件」中直接选择该文件即可；
导入器从上级 `plugins/qqmusic/plugin.toml` 读取元数据，并临时将所选 WASM
规范化为清单中的 `provider.wasm`，不会修改构建输出或复制整个 `target/`。

手动打包时使用：

```text
io.yinqidao.qqmusic/
├── plugin.toml
└── provider.wasm
```

`plugin.toml` 复制自 `plugins/qqmusic/plugin.toml`，将编译产物复制并命名为
`provider.wasm`。注意 QQ 与网易云插件的 `plugin.toml`、WASM 不能混用。
