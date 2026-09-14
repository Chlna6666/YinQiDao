# YinQiDao 项目计划

更新日期：2026-09-14

本文件记录当前主线开发计划。实现顺序遵循：稳定播放与实时音频安全 > Provider/插件基础设施 > 账号与在线能力 > 推荐体验 > 生态扩展。

## 当前主线：WASM 音乐服务插件系统

目标不是把现有 `src/online/providers` 简单改成可加载脚本，而是建立完整的跨平台音乐服务层：网易云音乐、QQ 音乐及后续其他服务通过同一 ABI 接入，多个账号可以同时在线，搜索/识别/歌词/串流/歌单/推荐按能力自动路由，不要求用户反复切换“当前平台”。

当前仍处于开发阶段：`PLUGIN_ABI_VERSION = 1` / WIT `@0.1.0` 继续作为开发期 ABI。首个稳定第三方插件 ABI 发布前可以直接修正 v1 设计，不人为引入 v2 兼容层。

详细设计见 [`docs/PLUGIN_SYSTEM.md`](PLUGIN_SYSTEM.md)，组件 ABI 见 [`plugins/wit/yinqidao-plugin.wit`](../plugins/wit/yinqidao-plugin.wit)。插件包结构与当前 Host 校验见 [`plugins/README.md`](../plugins/README.md)。

### P0：协议与路由基础

- [x] 保持开发期 `PLUGIN_ABI_VERSION = 1`；所有 Provider operation 显式携带 `provider-id`，避免匿名搜索、相同 account id 等场景发生路由歧义。
- [x] Secret import 使用 `provider + optional account + key` 显式 scope；plugin identity 由 Host Store context 注入，guest 不能选择其他插件 namespace。
- [x] 定义动态 Provider capability，不再让未来插件依赖固定 `ProviderKind` 枚举。
- [x] 定义账号、认证 challenge、曲目身份、串流、推荐、识别、播放反馈等宿主协议类型。
- [x] 定义统一账号 `PluginServiceRouter`，多个平台/账号同时保持登录，不存在全局“切换平台”状态。
- [x] 为 Search / Playlist / Cloud Library / Recommendations 定义 fan-out 路由。
- [x] 为 Lyrics / Metadata / Recognition / Streaming 等单结果请求定义按账号默认值、优先级、临时 provider preference 选择的路由。
- [x] 默认策略确定为 `authenticated plugin API -> built-in provider fallback -> local fallback`。
- [x] 建立 WebAssembly Component Model WIT world，网络、Secret、时钟、日志均由 Host import 提供。
- [ ] 在有 Rust toolchain 的环境执行 `cargo fmt --all -- --check`、`cargo check --locked`、`cargo test --locked`。

### P1：Wasmtime Component Host

- [x] 项目实际 MSRV 升到 Rust 1.95；Wasmtime 运行时目标锁定 `wasmtime 48.0.1`。
- [ ] 在可运行 Cargo 的环境加入 `wasmtime = 48.0.1`，生成/提交新的 `Cargo.lock` 并用 `--locked` 验证，禁止手工伪造锁文件。
- [ ] 使用 Wasmtime Component `bindgen!` 从开发期 WIT v1 生成宿主与 guest binding，禁止手写易漂移 ABI。
- [x] 建立 runtime-neutral `PluginHostServices` / `PluginStoreContext`，后续 Wasmtime generated Host traits 只负责类型转换，权限/网络/Secret/熔断决策继续留在独立 Host service 层。
- [x] 建立 per-plugin/per-provider call permit：限制并发，连续错误/timeout/cancel/panic 累积失败并短时熔断对应 route，成功调用清零连续失败。
- [x] 建立统一 guest-call wrapper：普通 Host future 有 30 秒 wall-clock deadline；429 根据整数 `Retry-After` 或默认退避写入 provider route，退避时间有 Host 上限且不会被 guest “成功处理 429”意外清除。
- [x] 暴露 `PluginRouteHealthSnapshot`，包含 in-flight、连续失败、circuit 剩余时间和 rate-limit 剩余时间，为 P3 routing health 接入提供稳定数据面。
- [x] 建立 runtime-neutral `PluginEnginePolicy`：集中定义 Store memory/table/instance、route warm pool、fuel、epoch tick/deadline 和 compiled artifact 上限，并在应用启动时校验硬上限。
- [x] 插件目录：`<config>/plugins/<plugin-id>/`；`plugin.toml` v1 描述 component、manifest 与网络域名声明。
- [x] 启动时仅扫描/校验 manifest 与 component 路径，不 instantiate WASM；为后续 component 懒加载保留边界。
- [x] Component 首次使用时重新 canonicalize 路径、限制体积、读取不可变 snapshot，并基于 ABI/Wasmtime/OS/arch/内容生成 cache bucket locator。
- [x] compiled cache 信任边界已收紧：MD5 只负责目录分桶，未来 `.cwasm` 反序列化前必须存在 Host sidecar，且 `source.wasm` 与当前 snapshot 逐字节完全一致；碰撞不能获得复用资格。
- [x] 校验插件/Provider ID、ABI、重复 capability/auth method、component 路径逃逸与静态 network domain allowlist。
- [x] 插件扫描失败局部化：单个坏包记录 failure，不阻止其他合法插件进入 Catalog，也不阻止播放器启动。
- [x] 建立 Host HTTP preflight：目标必须同时命中 manifest 声明和用户 grant；首版仅允许 HTTPS，redirect 必须重新授权，并拒绝 IP literal/明显本地域名。
- [x] 建立 `<config>/plugin-permissions.json` 权限索引；用户 grant 只能等于或缩小 manifest 声明范围，插件更新不能借已有授权静默扩大网络/PlaybackEvents 权限。
- [x] 建立 Host HTTP executor：DNS 在阻塞 worker 解析，过滤 loopback/private/link-local/documentation/benchmark/NAT64/Teredo/6to4 等特殊地址后使用 `resolve_to_addrs` 固定到本次 hop；关闭自动 redirect/系统代理继承，redirect 逐 hop 重新授权和解析。
- [x] Host HTTP executor 限制 method/header/request body/response body/timeout/redirect 次数；跨 origin redirect 自动剥离 Authorization/Cookie，response 使用 bounded chunk 读取。
- [x] Host runtime 已接入 429 provider backoff；非法/过长 `Retry-After` 不可无限延长 Host route block。
- [ ] 将 Host HTTP executor 接到 generated WIT `host.http-request`，并加入连接池复用和显式 Host proxy 策略。
- [ ] 真正接入 Wasmtime `Component::new` / serialize / deserialize；compiled artifact 必须写入大小上限并使用上述 source verifier，CPU feature/engine config 变化需要纳入失效条件。
- [ ] 将 `PluginEnginePolicy` 映射到 Wasmtime `Config` / `StoreLimits` / fuel / epoch interruption / 实例池，不能只停留在声明层。
- [ ] 默认不授予 filesystem、raw socket、process、environment 权限。
- [ ] 所有 HTTP 走 host-mediated HTTP import，WASM 不持有 raw socket。
- [ ] 插件调用只允许运行在普通 async/worker 路径，严禁进入 realtime audio callback。

### P2：统一账号中心与融合登录

- [x] 建立 Host 级 `PluginHostState` 与非 Secret 的 `plugin-accounts.json` 账号索引，启动恢复全部已安装插件账号路由元数据。
- [x] 同一 `plugin_id + provider_id` 可以保存多个账号，并规范化为最多一个默认账号；不会产生全局平台切换状态。
- [x] 账号索引只保存路由元数据和状态，明确禁止 cookie/token/refresh token/device secret 进入普通配置文件。
- [x] `SecretSlot` 使用 `plugin/provider/provider-scope/key` 与 `plugin/provider/account-scope(account)/key` 两种作用域，长度前缀 + scope tag 避免碰撞。
- [x] 建立 `PluginSecretStore` 抽象与有单 Secret 大小限制、删除/替换前清零旧值的 `MemorySecretStore`，用于 Host wiring/test；它明确不是生产持久化凭据后端。
- [x] 建立 `PluginSessionCoordinator`：新进程启动时历史 Authenticated/Expired 会话进入 `PendingValidation` 恢复队列；历史 Authenticated 先从可路由状态降级，Secret/session 验证成功后才重新启用。
- [x] 建立宿主会话转换接口：fresh login 可直接注册当前进程已验证账号；restore success / restore failure / logout 分别落到 Authenticated / Expired / LoggedOut，不要求重启播放器。
- [ ] 设置页增加“音乐服务与插件”入口，显示所有插件、Provider、账号状态、待验证状态与权限。
- [ ] 支持 QR 登录、浏览器 OAuth、Device Code、Cookie Import、Host-owned Custom Form。
- [ ] 将 generated WIT `auth-poll`/refresh 结果接入 `PluginSessionCoordinator`，登录完成后立即注册，不重启应用、不重建播放器。
- [ ] 用户可以在一次具体操作中临时指定平台/账号，该偏好只作用于本次请求，不产生全局切换。
- [ ] Session/refresh token/cookie 不写入 `config.toml`；实现 OS credential store 或经过认证的 Host 加密 Secret Store，并实现 `PluginSecretStore`。
- [ ] Session 即将过期时由后台刷新；刷新失败标记 `Expired`，不阻塞其他已登录平台。
- [ ] 支持 logout 单账号、logout 单平台全部账号、撤销插件全部 secret。

### P3：Provider Router 接管现有在线链路

- [ ] 将现有 `ProviderKind` 视为 built-in compatibility backend，而不是未来唯一 Provider 模型。
- [ ] `OnlineServices` 前置统一 Service Router，并把 `PluginSessionCoordinator` 作为路由资格门：PendingValidation/Expired/LoggedOut 都不得产生 authenticated route。
- [ ] 已登录插件具备 Metadata/Search/Lyrics/Artwork 能力时优先走插件官方/登录 API。
- [ ] 插件无结果、异常、超时或置信度不足时才进入现有匿名 provider chain。
- [ ] 识别流程改为：Host 计算一次 fingerprint -> 已登录插件 Recognition fan-out/priority -> built-in remote recognition -> AcoustID/local fallback。
- [ ] 保持“元数据 Provider 与歌词 Provider 解耦”：QQ 元数据命中仍可选择网易云 YRC/TTML 等更高质量 authored word timing。
- [ ] artwork 同样独立选择最匹配来源，不能因 metadata winner 强制绑定封面来源。
- [ ] 将现有 `PluginRouteHealthSnapshot` 接入 routing：circuit/rate-limit route 暂时不参与选择；连续 5xx 的 guest/provider 策略在 generated binding 接入后补齐。

### P4：统一在线曲库与播放

- [ ] 新增 `CanonicalTrackId` / canonical identity 层，把本地 Track、网易云 ID、QQ mid、ISRC、MBID、fingerprint 映射到同一逻辑曲目。
- [ ] 本地歌曲和远程歌曲可同时出现在队列、播放历史、收藏、歌单与推荐结果中。
- [ ] 搜索并发查询：本地 Library + 所有已登录 Search provider，统一去重、排序和分组。
- [ ] Cloud Library 页面聚合各平台“我喜欢/收藏/购买/云盘”等能力，不要求切换平台页面。
- [ ] 统一歌单允许混合本地与多平台曲目；同步到单平台时做 identity resolve 与缺失项报告。
- [ ] Stream 插件只返回 `URL + headers + expiry + codec hints`，真正网络读取、缓存、解码、输出继续由 Host 管理。
- [ ] 临近 URL 过期时提前刷新，避免播放中 401/403。
- [ ] 预加载下一曲时允许提前 resolve stream，但不把插件调用放进 audio callback。
- [ ] DRM/设备绑定能力单独声明；Host 无法合法解密的服务必须明确标记 unsupported，不做静默绕过。

### P5：人性化本地推荐 + 多平台推荐融合

- [ ] Library SQLite 增加 append-only/低写放大的行为信号：play started/completed、skip、seek、like、unlike、dislike、manual replay、queue source、推荐 impression/click。
- [ ] 所有行为写入普通后台路径，绝不从 realtime callback 写数据库。
- [ ] 建立本地 profile：最近播放、长期偏好、artist/album/genre affinity、时间段偏好、重复疲劳、skip penalty。
- [ ] 冷启动推荐优先使用本地 Library 的多样化组合，不要求登录任何平台。
- [ ] 登录平台后并发请求各平台 Personalized Recommendations，再与本地候选融合。
- [ ] 去重以 canonical identity 为准，避免同一首歌在网易云/QQ/本地文件重复出现三次。
- [ ] 排序加入 freshness、多样性、熟悉/探索比例、连续艺术家惩罚、近期播放惩罚、用户明确 dislike hard filter。
- [ ] 默认不把所有本地行为上传给所有平台；只有插件明确声明 PlaybackEvents 且用户允许时才发送对应平台所需事件。
- [ ] 推荐解释保持简洁：例如“来自 QQ 每日推荐”“因为最近常听 X”“本地收藏中很久没播放”。
- [ ] Home 使用 section 化推荐而不是无限随机流：继续听、每日混合、最近喜欢、相似歌曲、跨平台发现、本地遗珠。

### P6：收藏、喜欢与歌单融合

- [ ] “喜欢”首先写入 YinQiDao canonical library，保证离线可用。
- [ ] 用户可配置每个平台是否同步 Like，不默认跨所有平台自动写入。
- [ ] 同步失败保留 pending op，并用幂等 operation id 重试；UI 展示本地已喜欢，不因为远端失败回滚用户操作。
- [ ] 跨平台歌单复制采用 preview -> resolve -> conflict report -> apply 四阶段，避免错误匹配批量污染歌单。
- [ ] 支持 provider-specific playlist metadata，但 UI 模型保持统一。

### P7：插件管理与开发者生态

- [ ] 插件安装、启用、禁用、更新、权限变更、健康状态、日志查看。
- [ ] 签名插件显示发布者身份；未签名插件默认需要显式开发者模式确认。
- [ ] 插件权限更新必须重新授权，不允许新版本静默扩大 network/secret 能力。
- [ ] 提供 Rust SDK；WIT 保证未来可以生成 TypeScript/Go/C# 等 guest bindings。
- [ ] 提供 reference plugin：本地 mock provider，不访问真实音乐服务，用于测试当前开发期 ABI。
- [ ] 提供插件 conformance tests：manifest、Provider 显式路由、超时、分页、auth state、provider/account Secret scope、URL expiry、错误映射。
- [ ] 插件 UI 第一阶段只允许 Host-owned schema/form/action；不向插件暴露 GPUI 对象或任意 native window。

### P8：稳定性、性能和安全收口

- [ ] Plugin calls tracing：provider、operation、latency、cache hit、timeout、circuit state；日志必须脱敏 token/cookie/header。
- [ ] 每个 service 独立 timeout 与 concurrency budget，推荐/搜索不可抢占播放 URL refresh。
- [ ] 大响应使用 bounded body；封面/音频本体不跨 WIT 大块复制，优先 URL/host blob handle。
- [ ] 组件实例与编译结果复用，禁止每次搜索重新 compile/instantiate。
- [ ] fuzz WIT boundary decoder 与 manifest parser。
- [ ] 恶意插件测试：无限循环、内存膨胀、redirect escape、secret probing、超大 JSON、压缩炸弹。
- [ ] Windows/Linux/macOS 都使用同一 WIT ABI，平台差异只留在 Host capability implementation。

## 关键行为准则

1. **不需要切换平台。** 登录网易云和 QQ 后，两者一直在线；每个功能按 capability 自动选择或 fan-out。
2. **登录后优先使用登录 API。** 对搜索、元数据、识别、歌词、推荐、云歌单等，存在健康且当前进程已验证的 authenticated plugin route 时先走插件；本地/匿名链路只是容错。
3. **本地文件仍是本地文件。** 路径、codec、sample rate、channels 等技术信息永远由 Host/本地扫描负责；插件只增强音乐身份与在线能力。
4. **播放核心不插件化。** 解码、DSP、空间音频、输出和 realtime callback 始终由 Host 管理，WASM 插件绝不能进入 realtime audio block。
5. **身份统一而来源独立。** 同一 canonical track 可以分别从 QQ 获得 metadata、网易云获得 YRC、本地获得无损文件、另一个平台获得推荐理由。
6. **失败局部化。** 一个插件超时不能卡 UI、不能阻塞其他 provider、不能影响已经在播放的 PCM。
7. **权限最小化。** 网络、Secret、文件、UI 能力都由 Host 显式授权；默认没有 raw OS 权限。
8. **会话跨进程 fail-closed。** 上一进程保存的 Authenticated 只代表历史状态；新进程必须重新验证 Secret/session 后才能获得 authenticated route。
9. **Provider 身份必须显式。** 一个 component 暴露多个 Provider 时，guest/Host 调用都不能依赖 account id 或隐式“当前平台”推断 Provider。
