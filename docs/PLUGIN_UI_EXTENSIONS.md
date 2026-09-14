# YinQiDao 插件 UI 扩展设计

状态：规划中；开发期 ABI v1。

本设计补充 `docs/PLUGIN_SYSTEM.md`。插件系统不仅需要音乐服务能力，还需要受控的 UI 扩展能力，包括路由注册、页面、设置页、导航入口、命令/动作、Home 区块和主题贡献。

核心原则：**插件声明贡献，Host 注册、校验和渲染；WASM 不直接持有 GPUI 对象，也不能在 paint/layout/realtime audio 路径执行。**

## 1. 为什么 UI 扩展不是 Provider capability

`ProviderDescriptor.capabilities` 表示音乐服务账号能力，例如 Search、Lyrics、Streaming、Recommendations。

Route/Page/Theme 属于插件级能力，不应绑定 Provider 或账号：

- 一个 component 可以包含多个 Provider，但只需要一套设置页或关于页。
- Theme 与登录状态无关。
- 插件可以只提供 UI 扩展而不提供音乐服务 Provider。
- 插件禁用时应整体撤销 UI contribution，而不是按账号逐条撤销。

因此 UI 扩展使用独立的 plugin-level contribution descriptor，不加入 `PluginCapability`。

## 2. 注册生命周期

第一阶段采用“静态注册 + 懒执行”：

1. `plugin.toml` 声明 route/page/theme/navigation/command 元数据。
2. Host 扫描 manifest 时完成结构校验，不 instantiate WASM。
3. 插件启用后，Host 将合法 contribution 注册到自己的 Router/Navigation/Theme Registry。
4. 用户真正打开插件页面或触发 command 时，才 lazy load/instantiate Component。
5. 插件 disable/update/uninstall 时，Host 原子撤销它的 routes/navigation/commands/themes，并 invalidate Component/instance cache。

禁止插件在运行过程中任意覆盖 Host 核心 route。后续若支持动态 contributions，也必须通过 Host registry 的增量事务接口，不能直接修改 GPUI router。

## 3. Route contribution

每个 route 使用插件命名空间身份：

```text
plugin:<plugin-id>/<route-id>
```

约束：

- `route-id` 只能使用稳定、小写、受限字符集。
- 插件不能注册 Host 保留 route id，也不能覆盖其他插件 route。
- manifest 中重复 route id 直接拒绝该 contribution。
- route 可以声明导航 placement，但 placement 只表示请求位置，最终顺序由 Host 决定。
- route parameters 必须是有界、可序列化数据；禁止透传 GPUI entity/window/context。

建议静态字段：

```text
id
title
icon?
page-id
placement?       sidebar | settings | home | hidden
order?
requires-auth?   可选的 provider/account gate
```

## 4. Page contribution

插件页面第一阶段采用 Host-owned declarative UI schema，不开放任意 native view。

建议支持的基础节点：

- text / heading
- stack / row / column
- section / card
- list / grid
- image
- button
- input / textarea
- select
- toggle
- progress
- badge
- divider / spacer
- table（有界行列）

Host 必须限制：

- 最大 UI tree depth
- 最大 node 数量
- 最大字符串长度
- 最大 list/table item 数
- 最大图片尺寸/解码内存
- action payload 大小

插件返回的是 view model，不是 GPUI element。Host 将 schema 转成统一 GPUI 组件，从而保持主题、无障碍、DPI、输入法、焦点和窗口生命周期一致。

## 5. 页面状态和 Action

Page render 与用户 action 都走普通 async/control path：

```text
navigate
  -> Host router
  -> lazy plugin call
  -> page model
  -> Host GPUI render

button/input action
  -> namespaced action id
  -> async guest call
  -> state/result patch
  -> Host 更新 view model
```

要求：

- UI thread 不同步等待 guest。
- guest timeout/trap 只让对应 page/action 失败，不能冻结窗口。
- action id 必须 namespaced。
- 不允许插件通过高频 action 驱动每帧重渲染。
- 页面关闭后取消/忽略过期异步结果。

## 6. Navigation extension points

第一阶段提供固定 extension points，而不是允许插件任意改主界面：

- sidebar
- settings
- home
- library secondary navigation
- track context actions
- playlist context actions
- command palette

Host 可以拒绝当前版本不支持的 placement。

插件不能：

- 删除/重排 Host 核心导航项。
- 覆盖核心页面。
- 隐藏系统设置、安全提示或权限 UI。

## 7. Settings page

插件设置使用 Host-owned form schema。

设置值必须存储在插件自己的配置 namespace：

```text
plugin-settings/<plugin-id>/...
```

普通设置不得存 token/cookie/refresh token；Secret 仍必须走 `PluginSecretStore`。

设置表单支持：

- boolean
- string
- integer/float bounded range
- enum/select
- path picker（只有未来显式文件权限存在时）
- action button

## 8. Commands 和上下文动作

插件可声明 namespaced command，例如：

```text
plugin:<plugin-id>/command/import-playlist
plugin:<plugin-id>/command/refresh-library
```

可挂载到：

- command palette
- track context menu
- playlist context menu
- page-local actions

Host 负责检查当前 selection/context，并只传必要的 canonical ids/descriptor，不把内部 GPUI entity 或裸数据库连接交给 guest。

## 9. Theme contribution

Theme 是静态设计资源，不是 guest 每帧计算逻辑。

第一阶段只接受 semantic design tokens + 受控 package assets，例如：

```text
background
surface
surface-elevated
text-primary
text-secondary
accent
border
success
warning
error
radius-small/medium/large
spacing-scale
```

主题规则：

- Theme 由用户显式选择，插件不能自动激活自己的主题。
- 插件 disable/uninstall 时自动回退到有效 Host theme。
- 禁止任意 CSS、native shader、脚本化 paint callback。
- paint/layout 时绝不调用 WASM。
- theme 文件和 assets 在启用时一次校验/解析，转换成 Host theme snapshot 后缓存。
- 图片等 package assets 由 Host bounded decode；不允许主题通过远程 URL 在渲染阶段取资源。
- Host 保留关键安全/错误状态颜色的最低可辨识约束。

后续可增加：字体 token、播放器背景 preset、歌词主题 preset，但仍然由 Host renderer 实现。

## 10. Home/Widget contribution

允许插件贡献 Home section，但不允许任意持续运行 widget runtime。

建议模型：

```text
HomeContribution
  id
  title
  priority
  refresh-policy
  page/action target
```

数据加载由普通 async guest call 完成，Host 做缓存、刷新节流和 viewport 生命周期管理。

推荐、最近播放等高频数据不得从 WASM paint callback 拉取。

## 11. 权限边界

UI contribution 本身不自动获得任何额外权限。

例如：

- 注册页面 ≠ 网络权限。
- 注册设置页 ≠ Secret 权限扩张。
- 注册 Theme ≠ filesystem/raw asset 任意访问。
- 注册 command ≠ process spawn。

页面 action 需要网络/Secret 时仍进入现有 Host permission/runtime/security façade。

## 12. 模块边界

计划结构：

```text
src/plugin/
  abi.rs
  client.rs
  frontend.rs

  ui/
    mod.rs
    manifest.rs      # contribution metadata + validation
    registry.rs      # routes/navigation/commands/themes
    schema.rs        # declarative page model
    actions.rs       # async guest action bridge
    theme.rs         # semantic token validation/cache

  component/
    wasmtime.rs      # generated bindings adapter only

src/ui/
  plugin_page.rs     # Host GPUI renderer for declarative schema
```

依赖规则：

```text
plugin::ui::schema/registry
        ↓
Host GPUI adapter

Wasmtime generated types
        ↓ only inside
plugin::component
        ↓ semantic conversion
plugin::client/frontend
```

`src/ui` 不允许直接引用 Wasmtime generated types；`plugin::component` 不允许直接创建 GPUI element。

## 13. WIT 演进计划

开发期 v1 可以直接增加独立 UI interface，不人为创建 ABI v2。

计划接口职责：

- `ui-describe`：返回/确认动态可用状态，不负责核心静态 route 注册。
- `ui-render-page(page-id, params)`：返回有界 declarative page model。
- `ui-handle-action(page-id, action-id, payload)`：处理用户动作。
- `ui-load-section(section-id, cursor?)`：可选 Home section 数据。

静态 route/navigation/theme metadata 优先放 `plugin.toml`，这样 Host 启动扫描不需要 instantiate Component。

## 14. 性能规则

- route/theme 注册只在安装、启动、enable/update 生命周期执行。
- theme token 在 paint 前预解析，不从 WASM 查询颜色。
- 页面 guest call 不在 GPUI render method 内执行。
- 列表使用 Host virtual list；guest 返回分页数据，而不是一次返回无限节点。
- 图片使用 Host image cache/bounded decode。
- update/uninstall 必须使 page state、route registry、Component lifecycle generation 一起失效。

## 15. 首阶段验收

UI 扩展第一阶段完成条件：

1. mock plugin 可以从 `plugin.toml` 注册一个 sidebar route 和一个 settings route。
2. 打开 route 后 lazy instantiate Component，Host 渲染声明式 page。
3. button/action 异步调用 guest，timeout 不冻结 UI。
4. disable plugin 后 route/navigation 立即撤销。
5. mock plugin 可以提供一个静态 Theme，并由用户显式启用/回退。
6. Theme/page 不存在直接 GPUI/Window/Wasmtime 类型泄漏。
7. route/page/theme 注册都有大小、重复 id、namespace 和资源限制测试。
