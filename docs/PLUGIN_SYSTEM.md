# YinQiDao WASM 插件系统设计

状态：设计已确定，P0 协议/路由基础实现中。

## 1. 设计目标

YinQiDao 的插件系统用于接入音乐服务能力，而不是把播放引擎本身脚本化。插件可以提供网易云音乐、QQ 音乐、Spotify 或后续其他平台的登录、搜索、元数据、歌词、封面、播放 URL、云歌单、收藏、推荐、识别等能力；Host 保留本地 Library、网络策略、音频下载/解码、DSP、空间音频、输出、缓存、Secret 与 UI 控制权。

核心用户体验：

- 用户可以同时登录多个音乐平台和多个账号。
- 不存在全局“当前音乐平台”切换开关。
- Search / Cloud Library / Playlist / Recommendations 可以并行聚合多个账号。
- Lyrics / Metadata / Recognition / Streaming 等单结果操作根据 capability、账号默认值、请求临时偏好、provider 健康度自动选择。
- 登录插件可用时优先使用该平台的 authenticated API；现有匿名 Provider 和本地识别作为 fallback。
- 同一歌曲允许不同能力来自不同 Provider，不把“歌曲来源”错误等同于“所有信息都必须来自同一平台”。

## 2. 与现有代码的关系

当前在线架构已经具备两个有价值的特性：

1. 多 Provider 并行搜索并做全局候选评分。
2. Metadata Provider 与 Lyrics Provider 已解耦，能够在 QQ 等 Provider 命中元数据时继续获取网易云 authored word timing。

需要替换的是固定的 `ProviderKind` 注册模型，而不是丢弃现有匹配质量逻辑。迁移完成后：

```text
                        ┌──────────────────────────┐
Local Library ─────────▶│ Canonical Identity Layer │
                        └────────────┬─────────────┘
                                     │
              ┌──────────────────────▼──────────────────────┐
              │              Service Router                 │
              │ capability + account + health + preference  │
              └───────────┬──────────────────┬──────────────┘
                          │                  │
              ┌───────────▼──────────┐  ┌────▼──────────────────┐
              │ Authenticated Plugins │  │ Built-in compatibility │
              │   WASM Components     │  │ src/online/providers   │
              └───────────┬──────────┘  └────┬──────────────────┘
                          │                  │
                          └──────────┬───────┘
                                     │
                            ┌────────▼────────┐
                            │ Local fallbacks │
                            │ AcoustID / tags │
                            └─────────────────┘
```

## 3. Component Model 选择

插件 ABI 使用 WebAssembly Component Model + WIT，而不是自行设计线性内存指针 ABI。

原因：

- WIT 能表达 record / variant / option / result / list，适合账号和音乐服务协议。
- Host 和 Guest binding 可以生成，避免手写 `ptr + len` ABI 的生命周期、编码和版本问题。
- Component Model 允许以后提供 Rust、JavaScript/TypeScript、Go 等不同语言 SDK。
- Host 可以只暴露明确 imports，而不是给 WASM 任意 OS 能力。

首版 world 位于：

`plugins/wit/yinqidao-plugin.wit`

首版 ABI：

`PLUGIN_ABI_VERSION = 1`

ABI 的 breaking change 必须升级 major ABI；新增 optional capability、optional field 或独立 interface 应优先保持兼容。

## 4. 权限模型

### 4.1 默认权限

默认插件只有：

- Host logger
- monotonic/current time abstraction
- 自己 namespace 下的 Secret Store
- 经授权的 Host HTTP

默认**没有**：

- arbitrary filesystem
- raw TCP/UDP socket
- process spawn
- environment variables
- microphone
- native window / GPUI object
- audio callback

### 4.2 HTTP

WASM 组件不直接拥有 raw network socket。插件发出 Host HTTP request，Host 负责：

- manifest `network_domains` allowlist
- 用户授权
- DNS/redirect 后再次校验目标域名
- timeout
- 最大 body/response
- connection pooling
- proxy
- TLS
- per-plugin/per-provider concurrency
- 429 backoff
- tracing 与脱敏

这样网易云/QQ 插件仍然可以在组件内部计算签名、构造 query/body/header，但最终 socket 由 YinQiDao 控制。

### 4.3 Secret

Token、cookie、refresh token、device secret 不允许写入普通插件配置或 `config.toml`。

Secret key 在 Host 侧强制 namespace：

```text
<plugin-id>/<provider-id>/<account-id>/<key>
```

插件只能看到自己的 namespace。后续 Host 实现优先使用系统 credential store；没有可靠平台 keystore 时使用带版本的加密文件并明确标记安全级别。

## 5. 登录模型

认证不是插件自己绘制网页/UI，而是插件返回 `AuthChallenge`，Host 负责统一用户体验。

支持：

- QR Code
- Browser OAuth
- Device Code
- Cookie Import
- Custom Form

流程：

```text
Host -> auth-begin(provider, method)
Plugin -> AuthChallenge
Host -> 展示二维码/打开浏览器/显示表单
Host -> auth-poll(challenge)
Plugin -> pending | authenticated(account) | expired | denied
Host -> 注册 AccountSession -> ServiceRouter
```

登录完成后不需要重启，不需要切换“网易云模式/QQ 模式”。

### 5.1 多账号

`plugin_id + provider_id + account_id` 构成 session 唯一键。

每个平台可以存在多个账号：

- `is_default` 只决定该 provider 内优先账号。
- `priority` 用于同能力的稳定选择。
- Search/Recommendations 等 fan-out 服务可以同时使用多个账号。
- 一次操作可临时指定 provider/account，但不会让其他账号退出或失活。

## 6. Service Router

Rust 侧 P0 已定义 `PluginServiceRouter`。

### 6.1 Fan-out 服务

默认 fan-out：

- Search
- Playlists
- Cloud Library
- Recommendations

这些服务需要聚合所有健康、已认证、声明对应 capability 的 Provider。

### 6.2 Single-route 服务

默认选择单个 route：

- Metadata
- Lyrics
- Artwork
- Streaming
- Recognition
- User Profile
- Like Sync
- Playback Events

单 route 的排序：

1. 本次操作显式 `preferred_provider`
2. provider 默认账号
3. account priority
4. 稳定 provider/account id 顺序

后续加入 runtime health 后，在第 1/2 项之间加入 circuit/latency/429 health score。

### 6.3 默认 fallback

```text
Authenticated Plugin API
        ↓ unavailable / error / timeout / low-confidence
Built-in Provider Compatibility Backend
        ↓ unavailable / error / timeout / low-confidence
Local Fallback
```

“登录后用 API”不是删除本地能力，而是改变优先级。这样平台故障、网络断开或插件被禁用时，本地音乐仍然可用。

## 7. 识别

识别不应该让每个插件重新读文件和解码音频。

Host：

1. 本地文件正常扫描技术 metadata。
2. 需要识别时统一计算一次 fingerprint。
3. 把 `algorithm + fingerprint bytes + duration` 传给 Recognition Provider。
4. 多平台识别可以并发，但需按账号 capability 和 budget 限制。
5. 结果进入 canonical identity resolver。
6. 无 authenticated recognition route 时才调用 built-in/AcoustID fallback。

首版默认不把 raw PCM 传给插件，减少复制、隐私和内存压力。未来确有平台只接受音频片段时，使用 Host blob handle/stream import，而不是把几十 MiB PCM 放进 WIT list。

## 8. Canonical Identity

插件化之后，`TrackId = i64` 只能继续表示本地 Library row，不能作为全局歌曲身份。

需要增加逻辑身份：

```text
CanonicalTrack
  local_track_id?
  title / artists / album / duration
  ISRC?
  MusicBrainz recording id?
  fingerprint id?
  source refs[]
      netease:<id>
      qqmusic:<mid>
      spotify:<id>
      ...
```

匹配顺序建议：

1. confirmed source mapping
2. ISRC exact
3. MBID exact
4. fingerprint exact/high confidence
5. title + artists + duration + version flags
6. ambiguous -> 不自动合并

现有 provider candidate score/version compatibility 可迁移到最后一层。

## 9. 远程播放

插件不把音频数据直接送入 audio engine。

插件返回 `StreamDescriptor`：

- URL
- request headers
- codec hint
- bitrate
- sample rate/channels hint
- expiry

Host 继续负责：

- URL download/range/read
- cache
- decoder
- seek
- preload
- crossfade
- DSP
- spatial
- device output

这样 realtime 路径完全不认识 WASM。

URL 有 expiry 时，播放层维护 refresh deadline；下一曲 preload 可以提前 resolve，但刷新和插件调用只能发生在 async control path。

## 10. 歌词、封面和元数据

Provider 不做“一次命中包办所有字段”。

推荐策略：

- Metadata：identity confidence 优先。
- Lyrics：authored word timing > synchronized translation > line sync > plain。
- Artwork：identity compatible + quality + cache policy。
- Stream：可播放性 + 用户 quality preference + expiry health。

因此可以出现：

```text
identity / metadata : QQ Music
lyrics              : Netease YRC
artwork              : QQ Music
stream               : local FLAC
recommendation       : QQ + Netease + local model
```

这属于正常状态，不需要“统一来源”。

## 11. 推荐系统

### 11.1 本地推荐始终存在

即使没有任何插件或网络，YinQiDao 也应产生有用推荐。

行为信号：

- playback_started
- playback_completed
- skipped
- seek-heavy
- replayed
- liked / unliked / disliked
- manual queue add
- recommendation impression/click
- hour-of-day/day-of-week（只在本地使用）

本地 rank feature：

- recency
- play frequency
- completion ratio
- skip penalty
- explicit like/dislike
- artist/album/genre affinity
- recently overplayed penalty
- artist repetition penalty
- novelty/exploration score

推荐不是单一随机 shuffle。Home 建议以多个 section 展示：

- 继续听
- 最近喜欢
- 你的每日混合
- 相似歌曲
- 本地遗珠
- 跨平台发现
- 某艺术家电台

### 11.2 登录后的推荐融合

对所有具有 Recommendations capability 的已登录账号并发请求：

```text
Netease recommendations ┐
QQ recommendations      ├─ canonical dedupe ─ local rerank ─ sections
Spotify recommendations ┤
Local candidate model   ┘
```

远端 score 不能直接跨 Provider 比较，需要在 Provider 内归一化，再由 Host 加入本地 freshness/diversity/fatigue 约束。

推荐结果必须记录 impression，否则无法区分“用户没看到”与“用户看到了但没点”。

## 12. 收藏与歌单

YinQiDao 自己的 canonical like 是 UI 真相来源。远端同步是附加状态。

好处：

- 离线喜欢立即生效。
- QQ 写入失败不会让爱心瞬间回滚。
- 用户可以选择只同步网易云，不同步 QQ。
- 后续可以把同一个 canonical track 同步到多个服务。

远端 mutation 使用 operation id，确保重试幂等。

跨平台歌单复制不允许直接按 title 批量搜索然后写入，必须：

1. preview
2. canonical resolve
3. ambiguity/missing report
4. 用户确认
5. apply

## 13. UI 原则

### 13.1 Music Services 页面

每个 Provider card 展示：

- 插件名称/版本/发布者
- Provider 名称
- 已登录账号
- capability
- health
- permissions
- login/add account/logout

不提供“把整个应用切到网易云”的开关。

### 13.2 Host-owned 登录 UI

插件只给 challenge/schema，Host 用统一 GPUI 组件显示。

禁止插件直接拿 GPUI context 或 native HWND。后续若需要插件 UI，采用声明式 panel/action schema，仍由 Host render。

## 14. 性能约束

- 插件不进入 realtime audio callback。
- Search/Recommendation/Library sync 使用 async task；不要 `cx.notify()` 高频轮询。
- WIT 不传大封面/音频 PCM，优先 descriptor/URL/blob handle。
- Component compile cache 持久化。
- 实例池按 plugin/provider 控制，不为每个 search item 新建实例。
- HTTP client 由 Host 复用 connection pool。
- 所有 queue bounded。
- Recommendation fetch 可以低优先级，不能与 stream refresh 抢占唯一 worker。

## 15. Wasmtime Host 预期实现

Host crate/module 后续至少需要：

```text
PluginCatalog
  scan/install/update/enable/disable

ComponentCache
  compile/deserialize/version invalidation

PluginInstancePool
  instantiate/reuse/limits/epoch interruption

PluginHttpHost
  allowlist/proxy/timeout/rate limit/redaction

PluginSecretStore
  namespace/encrypt/revoke

PluginAccountStore
  account/session metadata

PluginServiceRouter
  capability + account + health routing

CanonicalIdentityStore
  local/remote id mapping

RecommendationEngine
  local model + provider merge
```

Wasmtime runtime版本必须在真实 Rust 环境中验证项目的 Rust 1.89 MSRV 与 `Cargo.lock --locked` 后再 pin，不在无 toolchain 环境盲目修改依赖图。

## 16. 安全与故障模型

必须覆盖：

- 无限循环 -> epoch/fuel interrupt
- 内存膨胀 -> store/resource limiter
- 大响应 -> body limit
- redirect 到未授权域 -> 拒绝
- secret cross-namespace probing -> 拒绝
- token/header log -> redact
- plugin panic/trap -> provider isolated failure
- repeated timeout/5xx -> circuit breaker
- rate limit -> Retry-After/backoff
- plugin update permissions expansion -> re-consent
- ABI mismatch -> disable with clear diagnostic

## 17. 首批 reference plugins

建议顺序：

1. `mock-provider`：本地 fixture，覆盖所有 ABI，不访问网络。
2. `netease`：QR/Cookie、Search、Metadata、YRC、Artwork、Cloud Library、Recommendations、Recognition（若平台接口可稳定实现）。
3. `qqmusic`：QR/Cookie、Search、Metadata、QRC、Artwork、Cloud Library、Recommendations。
4. 再迁移 Spotify/咪咕/酷狗等。

真实 Provider 插件应该独立于 Host 发布周期；Host 只维护稳定 ABI 和策略层。
