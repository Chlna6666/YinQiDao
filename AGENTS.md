# Repository Guidelines

## Project Structure & Module Organization

`src/main.rs` is the desktop entry point. GPUI UI code is under `src/ui/`, including `shell.rs`, page modules, and reusable `components/`. Playback and DSP live in `src/audio/`; library scanning and metadata indexing live in `src/library.rs` and `src/library/metadata.rs`. Lyrics, artwork, online providers, settings, and OS media controls are separate modules under `src/`. `crates/lucide-gpui/` contains local icon assets, while `vendor/gpu-allocator/` is a patched dependency. Tests are colocated with modules, including `src/audio/engine_tests.rs`; there is no standalone `tests/` directory.

## Build, Test, and Development Commands

Use the locked dependency graph:

```powershell
cargo run
cargo check --locked
cargo test --locked
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
```

`cargo run` launches the player (Windows is the primary development target). `cargo check` is the fast compile gate; `cargo test` runs unit and module tests; `fmt` and `clippy` enforce formatting and lint quality. For a focused regression, use a specific test path, for example `cargo test audio::dsp::tests -- --nocapture`.

## Coding Style & Naming

This is a Rust 2024 project requiring Rust 1.89 or newer. Run rustfmt and keep imports idiomatic. Use `snake_case` for modules and functions, `UpperCamelCase` for types, and `SCREAMING_SNAKE_CASE` for constants. Prefer borrowed inputs, explicit `Result`/`Option`, small responsibility-focused modules, and no unnecessary cloning, allocation, blocking I/O, or unbounded queues. Keep UI work on the GPUI thread; move scanning, metadata, artwork, and network work to background tasks. Preserve the existing provider priority and GPUI image-cache usage.

## Testing Guidelines

Add regression tests beside the changed module and name them by behavior, such as `rapid_track_switch_keeps_latest_command`. Cover cancellation/coalescing, file-format and error paths, and bilingual lyrics when relevant. Avoid real network calls in unit tests; use deterministic fixtures and temporary files. A desktop session may be required for media-control tests.

## Commit & Pull Request Guidelines

Use focused Conventional Commits, such as `fix(ui): prevent artwork placeholder flicker`, `perf(audio): reuse decode buffers`, or `feat(online): add provider fallback`. PRs should explain the user-visible change, list validation commands, call out platform-specific limitations, and include a screenshot or reproduction note for UI changes. Do not commit credentials; document Spotify environment variables without hard-coding values.

## Configuration & Security

Keep credentials in `YINQIDAO_SPOTIFY_CLIENT_ID` and `YINQIDAO_SPOTIFY_CLIENT_SECRET`. Online endpoints may change, so handle provider failures gracefully. Do not add absolute machine paths or commit generated local caches.

## Commit Conventions (Cocogitto)

提交信息使用 Angular 风格的 Conventional Commits，并由 Rust 编写的 Cocogitto (`cog`) 校验。格式为 `type(scope): description`，`scope` 可省略；提交主题建议不超过 50 个字符，中文和英文均可。允许的类型为：`feat`、`fix`、`docs`、`style`、`refactor`、`perf`、`test`、`build`、`ci`、`chore`、`revert`。

首次开发环境安装并启用 hook：

```powershell
cargo install cocogitto --locked
cog install-hook commit-msg
```

之后可以使用 `cog commit fix "修复关于页面渲染后端显示" "渲染"`，也可以使用 `git commit`；已安装的 `commit-msg` hook 会执行 `cog verify` 和 `cog check`。详细规则见 [docs/COMMIT_CONVENTIONS.md](docs/COMMIT_CONVENTIONS.md) 与根目录 [cog.toml](cog.toml)。Cocogitto 仅管理提交、校验和 CHANGELOG，不参与 Rust 应用构建或运行。
