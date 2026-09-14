# YinQiDao WASM 插件包

YinQiDao 音乐服务插件使用 WebAssembly Component Model。当前仓库已经定义 WIT ABI 与宿主侧插件目录、账号、会话和权限路由基础；真正的 Wasmtime Component 实例化将在 P1 后续阶段接入。

## 安装目录

每个插件使用独立目录：

```text
<config>/plugins/<plugin-id>/
├─ plugin.toml
└─ provider.wasm
```

`<plugin-id>` 必须与 `plugin.toml` 中的 `id` 完全一致。Host 启动时只解析清单并校验 component 路径，不会为了扫描插件而立即实例化 WASM。

## plugin.toml v1

```toml
package_schema = 1
component = "provider.wasm"

id = "com.example.netease"
name = "NetEase Music Provider"
version = "0.1.0"
abi_version = 1
description = "Example music-service plugin"
homepage = "https://example.com"
network_domains = [
  "music.163.com",
  "interface.music.163.com",
]

[[providers]]
id = "netease"
display_name = "网易云音乐"
capabilities = [
  "authentication",
  "search",
  "metadata",
  "lyrics",
  "artwork",
  "streaming",
  "playlists",
  "cloud_library",
  "recommendations",
  "recognition",
  "user_profile",
  "like_sync",
  "playback_events",
]
auth_methods = ["qr_code", "cookie_import"]
```

`plugin.toml` 使用 Host 的 `snake_case` 枚举名称；WIT world 使用 Component Model 的 kebab-case 名称。后续 SDK 会生成两侧 binding，插件作者不应自行维护两套 ABI 字符串。

## 当前 Host 校验

启动扫描已经拒绝以下包：

- `package_schema` 与 Host 不兼容；
- `abi_version` 与 `PLUGIN_ABI_VERSION` 不兼容；
- 插件目录名与 manifest `id` 不一致；
- 空 provider、重复 provider id、重复 capability/auth method；
- 声明登录方式但没有 `authentication` capability；
- `component` 是绝对路径、包含 `..`、不是 `.wasm`，或通过软链接逃逸插件目录；
- `network_domains` 携带 URL scheme、path、port、空白字符或非法 host label；
- 同一个插件 id 被多个目录重复声明。

这里的 domain allowlist 只是静态请求声明。Host HTTP preflight 还要求目标同时命中用户授权；redirect 必须重新授权。后续 HTTP executor 仍需要在 DNS 解析后拒绝 loopback/private/link-local/特殊用途地址，静态 manifest 与 URL 检查不能替代完整 SSRF 防护。

## 权限索引

Host 在配置目录保存：

```text
<config>/plugin-permissions.json
```

该文件保存用户明确授予的非 Secret 权限，例如网络域名和是否允许向对应平台回传播放行为。授权范围必须是 `plugin.toml` 声明范围的子集：

```text
manifest: *.example.com
user grant: api.example.com       ✓
user grant: *.api.example.com     ✓
user grant: *.example.com         ✓
user grant: example.com           ✗
user grant: *.com                 ✗
```

因此插件更新、损坏的权限文件或 UI bug 都不能利用历史 grant 静默扩大网络访问。`playback_events = true` 也只有插件实际声明 `playback_events` capability 时才有效。

该文件不保存 Cookie、token 或其他登录秘密。

## 账号索引

Host 在配置目录保存：

```text
<config>/plugin-accounts.json
```

该文件只保存账号的非敏感路由元数据，例如：

- plugin/provider/account id；
- display name / avatar URL；
- capability；
- priority；
- provider 内默认账号；
- authenticated/expired/logged-out 历史状态。

**Cookie、access token、refresh token、device secret 不允许进入该文件。** Secret Store 会作为独立 Host capability 实现，并按插件/Provider/账号 namespace 隔离。

同一个 Provider 可以保存多个账号，但 Host 会保证同一个 `plugin_id + provider_id` 最多只有一个 `is_default = true`。其他账号继续存在并可在验证成功后参与 fan-out 服务，不需要全局切换音乐平台。

## 跨进程会话恢复

账号索引中的 `Authenticated` 只描述上一进程最后一次确认的状态，不能作为新进程的登录凭据。

启动顺序为：

```text
plugin-accounts.json
        ↓
PluginSessionCoordinator
        ↓
PendingValidation
        ↓ Secret/session refresh 成功
Authenticated → PluginServiceRouter 可路由
        ↓ 失败
Expired → built-in/local fallback
```

Host 启动时会先把历史 `Authenticated` 从可路由状态降级，并放入 `PendingValidation` 恢复队列。历史 `Expired` 也可以进入恢复队列，因为 refresh token 仍可能有效；用户明确 `LoggedOut` 的账号不会自动重试。

fresh login 是例外：当前进程刚完成登录并拿到有效 session 后，可以直接通过会话协调器注册 `Authenticated`，无需重启播放器。

未来 Wasmtime `auth-poll` / refresh 流程必须通过这个协调器改变路由资格，不能直接信任磁盘状态。

## Secret namespace

Host-owned `SecretSlot` 使用：

```text
plugin-id / provider-id / account-id / key
```

逻辑 namespace。底层 storage key 使用长度前缀编码，账号 ID 中即使包含 `/` 等分隔字符，也不能逃逸到其他账号或插件的 Secret namespace。真正的系统 credential store / Host 加密 Secret Store 仍在后续阶段实现。

## ABI

WIT world：

```text
plugins/wit/yinqidao-plugin.wit
```

设计说明：

```text
docs/PLUGIN_SYSTEM.md
```

项目实施顺序：

```text
docs/PROJECT_PLAN.md
```
