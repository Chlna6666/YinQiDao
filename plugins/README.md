# YinQiDao WASM 插件包

YinQiDao 音乐服务插件使用 WebAssembly Component Model。当前仓库处于开发阶段，ABI 仍保持 `PLUGIN_ABI_VERSION = 1` / WIT `@0.1.0`；在首个稳定第三方 ABI 发布前允许直接修正 v1 设计，不人为制造 v2 兼容层。

运行时目标为 **Rust 1.95 + Wasmtime 48.0.1**。当前工作环境无法运行 Cargo，因此真正的 Wasmtime dependency、generated bindings 与 `Cargo.lock` 更新仍需在可运行 Rust 1.95 的环境完成。

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

`plugin.toml` 使用 Host 的 `snake_case` 枚举名称；WIT world 使用 Component Model 的 kebab-case 名称。

## Provider 显式路由

一个 component 可以同时实现多个 Provider，例如同一个插件同时暴露 `netease` 与 `qqmusic`。因此开发期 v1 已直接修正为所有 Provider operation 显式携带 `provider-id`：

```text
search(provider-id, account-id?, ...)
logout(provider-id, account-id)
recommendations(provider-id, account-id, ...)
recognize(provider-id, account-id?, ...)
```

这保证匿名搜索、多账号登录、跨平台推荐以及不同 Provider 使用相同 account id 时都没有歧义。不存在隐式“当前平台”。

Host HTTP request 同样携带 `provider-id` 与可选 `account-id`。`plugin-id` 不由 guest 传入，而是在 Host 实例化 component 时绑定到 Store context，guest 无法冒充另一个插件。

## 当前 Host 校验

启动扫描拒绝以下包：

- `package_schema` 与 Host 不兼容；
- `abi_version` 与 `PLUGIN_ABI_VERSION` 不兼容；
- 插件目录名与 manifest `id` 不一致；
- 空 provider、重复 provider id、重复 capability/auth method；
- 声明登录方式但没有 `authentication` capability；
- `component` 是绝对路径、包含 `..`、不是 `.wasm`，或通过软链接逃逸插件目录；
- `network_domains` 携带 URL scheme、path、port、空白字符或非法 host label；
- 同一个插件 id 被多个目录重复声明。

Host HTTP executor 还会在 DNS 后过滤 loopback/private/link-local/documentation/benchmark/NAT64/Teredo/6to4 等特殊地址，并将已验证地址 pin 到请求；redirect 每一跳重新执行权限与 DNS 校验。

## 权限索引

Host 在配置目录保存：

```text
<config>/plugin-permissions.json
```

该文件保存用户明确授予的非 Secret 权限，例如网络域名和是否允许向对应平台回传播放行为。授权范围只能等于或小于 `plugin.toml` 的声明范围。该文件不保存 Cookie、token 或其他登录秘密。

## 账号索引

Host 在配置目录保存：

```text
<config>/plugin-accounts.json
```

该文件只保存账号的非敏感路由元数据，例如 plugin/provider/account id、display name、capability、priority、provider 内默认账号以及历史状态。

**Cookie、access token、refresh token、device secret 不允许进入该文件。**

同一个 Provider 可以保存多个账号，但 Host 会保证同一个 `plugin_id + provider_id` 最多只有一个 `is_default = true`。其他账号继续存在并参与 fan-out，不需要全局切换音乐平台。

## 跨进程会话恢复

账号索引中的 `Authenticated` 只描述上一进程最后一次确认的状态，不能作为新进程的登录凭据。

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

用户明确 `LoggedOut` 的账号不会自动重试。fresh login 在当前进程拿到有效 session 后可以立即注册为 `Authenticated`。

## Secret namespace

WIT import 使用显式 scope：

```text
secret-scope {
    provider-id,
    account-id?,
}
```

Host 注入 `plugin-id`，最终 namespace 为：

```text
plugin-id / provider-id / provider-scope / key
plugin-id / provider-id / account-scope(account-id) / key
```

Provider scope 用于账号尚未确定的 QR/OAuth/device challenge 临时凭据；Account scope 用于 refresh token、cookie 等长期账号凭据。底层 `SecretSlot` 使用开发期 v1 的长度前缀编码和显式 scope tag，Provider scope 与任意 account id 不会碰撞。

`MemorySecretStore` 只用于 Host wiring/test；真实跨进程登录必须使用 OS credential store 或经过认证的 Host 加密 Secret Store。

## Runtime Host services

`PluginHostServices` 是未来 Wasmtime generated bindings 的安全 façade：

```text
Wasmtime Store<PluginStoreContext>
        ↓
PluginHostServices
  ├─ PluginHttpExecutor
  ├─ PluginPermissionState
  ├─ PluginSecretStore
  └─ per-plugin/provider call budget + circuit breaker
```

调用 permit 在 guest export 前获取；成功清零连续失败，错误/timeout/cancel/panic 路径计入失败。超过阈值只熔断对应 plugin/provider route，不影响播放器、其他 Provider 或其他插件。

## ABI

WIT world：

```text
plugins/wit/yinqidao-plugin.wit
```

当前开发期 WIT package：

```text
yinqidao:music-plugin@0.1.0
```

设计说明：`docs/PLUGIN_SYSTEM.md`

项目实施顺序：`docs/PROJECT_PLAN.md`
