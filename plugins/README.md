# YinQiDao WASM 插件包

YinQiDao 音乐服务插件使用 WebAssembly Component Model。当前仓库已经定义 WIT ABI 与宿主侧插件目录/账号路由基础；真正的 Wasmtime Component 实例化将在 P1 后续阶段接入。

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

这里的 domain allowlist 只是静态请求声明。后续 Host-mediated HTTP 仍会在每次请求、重定向和 DNS 解析后执行运行时安全检查，静态 manifest 校验不能替代网络沙箱。

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
- authenticated/expired/logged-out 状态。

**Cookie、access token、refresh token、device secret 不允许进入该文件。** Secret Store 会作为独立 Host capability 实现，并按插件/Provider/账号 namespace 隔离。

同一个 Provider 可以保存多个账号，但 Host 会保证同一个 `plugin_id + provider_id` 最多只有一个 `is_default = true`。其他账号继续保持登录并参与 fan-out 服务，不需要全局切换音乐平台。

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
