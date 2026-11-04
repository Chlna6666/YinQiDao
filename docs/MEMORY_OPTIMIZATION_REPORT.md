# 内存占用优化报告（2026-11-04）

## 概述

两轮系统性优化：第一轮针对**无界滞留内存**（只增不减的缓存）与**分配器回收节奏**；第二轮按「先测量归因、再动手」的规范，基于 10s 粒度时间序列收紧**插件 GC 保留策略**与 **GPUI 图像管线设计预算**。全部改动零新增依赖，`Cargo.toml` / `Cargo.lock` 未变。

## 测量归因（10s 粒度时间序列 + 日志时间戳对齐）

- **启动后 5 秒内提交内存即达 ~1071MB**（启动突发），随后被 mimalloc purge 与 GC 逐步回落；
- 旧策略（warm TTL 120s）下 T180→T300 出现 ~78MB 回落台阶，与插件 GC sweep 丢 warm 实例的时点吻合 → **warm Store/Instance 线性内存+async 栈确实滞留数十 MB**；
- GPUI fork 图像管线默认**设计内滞留 ~192MB**（`idle_image_bytes=128MB` 解码图空闲缓存 + `bitmap_pool_bytes=64MB` 空闲位图复用池），远超播放器可见工作集；
- **测量噪声警示**：同配置两次运行的稳态提交内存相差 ±50–60MB（网络/封面/驱动状态差异），稳态单点对比不具决定性；启动峰值与策略边界是稳健指标。

## 前后对比（同机、同协议：启动后 60/180/240s 采样）

| 指标 | 基线(HEAD) | 第一轮 | 第二轮 |
|---|---|---|---|
| 二进制体积 | 40,778,752 B | ≈持平 | ≈持平 |
| 提交内存启动峰值 | 1011.1 MB (T60) | 945.2 MB (T60) | 962 MB (T30，含突发未完全回落) |
| 提交内存稳态 | 956.9 MB | 944.2 MB | 958.1 MB（曲线全程平坦） |
| 插件 warm 实例保留 | 120s TTL / 全局 32 | 同左 | **60s TTL / 全局 8 / 编译缓存 8 份·300s** |
| GPUI 解码图空闲预算 | 128 MB + 位图池 64 MB | 同左 | **48 MB + 16 MB** |

> 注：第一轮/第二轮稳态差异在运行间噪声（±50–60MB）之内；第二轮的收益是**结构性**的——GC 边界收紧 4 倍、图像空闲预算收紧 ~3.3 倍，上限保证在任何会话形态下成立，不依赖单次采样。

**结构性收益（与采样无关）**：

- 本地封面位图（png+blur）原本整库常驻（1 万首 ≈ GB 级），现封顶 256 条 ≈ 200–300 MB，超出部分按 FIFO 驱逐、可见时经磁盘缓存按需补载；
- 远端图片内存缓存：磁盘 image-cache 已积累 882 张 / 318 MB，旧实现下长期浏览会把它们全部滞留 RAM，现封顶 256 条（≈ 92 MB 上界）；
- 在线歌单视图缓存封顶 16 份完整歌单数据。

## 改动清单

| 文件 | 内容 |
|---|---|
| `src/allocator.rs`（新增） | mimalloc `purge_delay` 1000ms→200ms；自声明最小 FFI（`mi_option_get/set`），以出厂默认值 1000 作运行时锚点校验，序号漂移则不调优。零新依赖（静态库经 mimalloc crate 已链入） |
| `src/main.rs` | 启动最早期调用回收调优；注册 `mod allocator`；**GPUI 图像管线预算收紧**（`idle_image_bytes` 128→48MB、`bitmap_pool_bytes` 64→16MB；刻意保留 `trim_memory_on_hidden=false` 防封面闪烁决策不动） |
| `src/plugin/component/gc.rs` | **插件 GC 保留策略系统性收紧**：warm Store/Instance 空闲 TTL 120→60s、全局 warm 上限 32→8（单 route 2 不变）、进程内编译 Component 缓存 16→8 份且空闲 TTL 600→300s；磁盘缓存策略不变。启动日志已确认新策略生效 |
| `src/ui/shell.rs` | ① 封面位图（png+blur）驻留上限 256 条 + FIFO 驱逐（当前播放曲受保护，调色板刻意保留）；② `artwork_ready` 集合终止整库批量喂入，杜绝「上限驱逐↔无限重载」循环；③ 渲染期 miss 记录（`ARTWORK_WANTED`）+ 事件驱动补载器（无轮询定时器），替代原递归式 `request_library_artworks`；④ `online_playlist_cache` 封顶 16；⑤ `track_index_by_id`/`online_daily_tracks` 转 `Arc`（克隆降为引用计数） |
| `src/ui/image_cache.rs` | 远端图片 `MEM_CACHE` 由无界 `HashMap` 改为带 256 条 FIFO 上限的 `MemCache`，被驱逐项可从磁盘缓存秒回 |
| `src/ui/library.rs` | 队列抽屉渲染复用 `track_index_by_id`（原来每次渲染重建 O(全库) HashMap）；5 处封面读取改走 `artwork_display_bytes`（miss 自动记录补载） |
| `src/ui/home.rs` | 首页渲染不再深克隆 `Vec<RemoteTrack>`；专辑卡封面读取接入按需补载 |
| `src/ui/plugin/service_accounts.rs` | 注销路径的字段清理适配 Arc 化 |

## 验证

- `cargo check --locked` ✓
- `cargo test --locked`：**482 通过，1 失败** —— `ui::stage_lyrics::tests::sustained_scale_uses_full_attack_release_without_early_clipping` 经 `git stash` 在干净 HEAD 上复测同样失败，为**本次改动前即存在的失败**（动画常量断言，与内存优化无关，建议单独排查）。
- `cargo fmt --all -- --check` ✓
- `cargo clippy --locked --all-targets --all-features -- -D warnings` ✓

新增回归测试：`artwork_map_eviction_respects_cap_and_protects_current`、`fifo_map_eviction_caps_entries`（shell.rs）、`mem_cache_evicts_oldest_beyond_cap`（image_cache.rs）。

## 后续建议（本轮刻意不做）

1. **启动突发（T5s ≈ 1071MB）的成分分解**：需要 VMMap/ETW 才能归因（wasmtime Store/工件 mmap、GPUI 图集与解码图、tokio 栈、D3D 提交资源）。这是下一个大头，值得单独立项。
2. `LyricsDocument` 原文三重持有（raw + 逐行 + 逐字）——`Arc<str>` 化或缓存条目在持久化后丢弃 raw（中风险，涉及持久化路径）。
3. 曲库扫描流式入库（`parsed_results` 全量驻留 + 路径串 ~4 份重复，首扫大库峰值 RSS 明显）。
4. `TrackData` 字段 `Arc<str>` 化 + codec 内化（中收益，触碰序列化与多处构造点）。
5. 建议手工做一次 1 分钟目检：打开曲库页确认封面正常渐次出现、滚动后封面可补载；插件功能（搜索/播放/歌词）正常（GC TTL 收紧后首次调用可能多一次冷实例化，属预期）。
