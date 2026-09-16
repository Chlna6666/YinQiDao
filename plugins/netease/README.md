# YinQiDao 网易云音乐插件

`io.yinqidao.netease` 是 YinQiDao 仓库内维护的 WebAssembly Component Model 参考 Provider。

## 已实现能力

- Cookie 导入登录、Host Secret 持久化与账号恢复；
- 歌曲搜索与 metadata resolve；
- LRC 歌词与翻译歌词合并；
- 封面 URL；
- 受 Host 网络权限约束的在线播放地址；
- 用户歌单、歌单分页曲目、创建歌单、添加/删除曲目；
- 网易云“我的音乐云盘” Cloud Library；
- “我喜欢的音乐” Liked Tracks；
- 喜欢/取消喜欢；
- NCM 容器流式解密模块（MP3/FLAC/M4A/WAV/Ogg 原格式输出，不转码）。

插件不会直接使用 WASI socket。所有网易云 HTTP 请求均经过 YinQiDao `host.http-request`，Cookie 仅通过 Host Secret API 保存和读取。

## NCM 解码边界

当前 `yinqidao:music-plugin@0.1.0` WIT 是音乐服务 Provider ABI，还没有通用本地音频 decoder export。因此 `src/decoder.rs` 是独立、可复用的 NCM 解密实现，不伪造一个 Host 无法调用的 capability。后续若 Host 增加统一 decoder ABI，可以直接把该模块接到新的字节流接口，不需要重新实现 NCM 算法。

## 构建

需要 Rust 1.95：

```bash
rustup target add wasm32-wasip2
cargo build --manifest-path plugins/netease/Cargo.toml --target wasm32-wasip2 --release
```

产物：

```text
plugins/netease/target/wasm32-wasip2/release/yinqidao_netease_plugin.wasm
```

安装时将其命名为 `provider.wasm`，与 `plugin.toml` 一起放进：

```text
<config>/plugins/io.yinqidao.netease/
├─ plugin.toml
└─ provider.wasm
```

仓库 CI 会为该独立 guest crate 生成锁文件、执行格式/检查/测试/Release Component 构建，并上传可安装插件包 artifact。仓库不会手工伪造 `Cargo.lock`。
