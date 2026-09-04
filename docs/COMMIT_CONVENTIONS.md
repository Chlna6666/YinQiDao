# Commit Conventions

本项目采用 Angular 风格的 Conventional Commits，由 Rust 编写的 Cocogitto (`cog`) 统一校验提交、生成版本变更记录并管理 CHANGELOG。

## 格式

```text
type(scope): description
```

`scope` 可选，不限制固定范围，建议使用清晰的 crate、GPUI 模块、音频模块或 CI 范围。提交主题建议不超过 50 个字符，描述可以使用中文或英文。

允许的类型：

| 类型 | 用途 |
| --- | --- |
| `feat` | 新增功能 |
| `fix` | 修复问题 |
| `docs` | 文档变更 |
| `style` | 格式或样式调整 |
| `refactor` | 重构，不改变外部行为 |
| `perf` | 性能优化 |
| `test` | 测试变更 |
| `build` | 构建或依赖变更 |
| `ci` | 持续集成变更 |
| `chore` | 其他维护工作 |
| `revert` | 回滚提交 |

示例：

```text
fix(渲染): 修复关于页面渲染后端名称显示
docs(规范): 补充提交信息与本地校验说明
perf(audio): 复用播放解码缓冲区
```

## 安装与校验

首次使用执行：

```powershell
cargo install cocogitto --locked
cog install-hook commit-msg
```

创建提交可以使用：

```powershell
cog commit fix "修复关于页面渲染后端显示" "渲染"
```

也可以使用 `git commit`。安装后的 `commit-msg` hook 会执行 `cog verify --file <message-file>`；仓库已有提交历史时还会执行带范围的 `cog check`，阻止新增的不符合规范提交。首次提交尚无 `HEAD`，因此只执行单条提交校验，避免 hook 自身阻止仓库初始化。手动校验可运行 `cog verify --file commit_message.txt` 或 `cog check`。

远端仓库已有一个历史提交 `Initial commit`，它不属于本次新增历史且未被改写。hook 和 CI 只检查远端基线之后的新提交；如需检查完整历史，直接运行 `cog check`。

## CHANGELOG

提交类型和 CHANGELOG 分组配置位于根目录 `cog.toml`。需要生成变更记录或版本时使用 `cog changelog`、`cog bump --auto`；Cocogitto 是开发工具，不参与 Rust 应用构建和运行。
