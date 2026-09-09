# YinQiDao Spatial Engine — 实现状态

更新时间：2026-09-09

## 核心目标

`yinqidao-audio-spatial` 是 YinQiDao 自研 CPU-first 参数化空间音频核心。当前不依赖 SOFA、KEMAR、外部 HRTF 数据库或第三方空间音频 runtime；定位算法必须描述为**参数化双耳 / 虚拟声源定位**，不得宣称 measured HRTF。

实时路径长期硬约束：

- audio block 内 0 heap allocation / 0 Vec growth；
- 0 Mutex / RwLock / blocking channel / file I/O / system call；
- 不在 audio block 内 spawn thread、构建 FFT plan 或创建运行时资源；
- interleaved PCM 通过 borrowed slice + stride 直接消费；
- 多声道保持 authored geometry 直接作为虚拟 source，不先破坏性 downmix；
- stereo L/R 保持两个独立 source，不 collapse 为 mono 再做 3D/360/8D；
- CPU 优先复用 `yinqidao-audio-simd` 的 Scalar/SSE2/AVX2/AVX2+FMA/NEON dispatch；
- realtime worker pool 只有 benchmark 证明收益后才能启用；音频 GPU compute 暂缓。

## Phase 0 — CPU realtime foundation

- [x] 独立 crate：`crates/audio-spatial`。
- [x] 默认 64-frame 固定内部 block，可配置。
- [x] `SpatialEngine` / source workspace 初始化时预分配。
- [x] `Vec3` / `SourcePose` / `ListenerPose` 无分配几何 primitive。
- [x] `ListenerPose::basis()` 作为公开只读几何 API，DSP 与应用侧 GPU Debug 共用同一 listener-local basis。
- [x] Stereo / 5.1 / 7.1 / 5.1.4 / 7.1.4 speaker scene。
- [x] `ChannelRole` + `SpeakerLayout::roles()/role()` 显式描述 authored PCM 槽位语义，不再只依赖数组下标。
- [x] 7.1.4 顺序与当前 AVS3 PCM 约定对齐，LFE index 3。
- [x] interleaved N-channel stride 直读，无 per-channel Vec。
- [x] stereo pair 独立虚拟 source primitive。
- [x] 4-point Lagrange fractional delay。
- [x] Woodworth spherical-head ITD。
- [x] 连续 ILD / far-ear attenuation / head-shadow 一阶低通。
- [x] near-field ear-distance correction、distance law、air absorption、front/rear/elevation 参数化线索。
- [x] LFE 独立 causal delay + 120 Hz low-pass。
- [x] end-exclusive `[n, n + frames)` 参数 ramp。
- [x] Orbit360 / FigureEight / Pendulum / FrontBack / Planetary / NearEar / Helix audio-clock trajectory。
- [x] clockwise / counter-clockwise。
- [x] 动态 trajectory 生成真实 m/s velocity，不再固定 `Vec3::ZERO`。
- [x] 高速 trajectory 按角速度自适应细分，单 segment 角位移约限制在 `<= 1°`；常规 48 kHz / 0.5 Hz 仍保持 64-frame block。
- [x] rigid stereo pair 的 L/R position 与 velocity 使用相同 Y 旋转，保持运动学一致。
- [x] source accumulation 复用 `yinqidao-audio-simd::mix_accumulate`。
- [x] NaN/Inf 在进入 delay/IIR/pinna persistent state 前隔离。

## Phase 1 — 播放器正式接入

- [x] 根播放器依赖 `yinqidao-audio-spatial`。
- [x] 10/12ch 正常播放直接进入 native spatial engine。
- [x] AVS3 5.1.4/7.1.4 在输入采样率下先 binaural render，再做 streaming stereo resample。
- [x] 正常 10/12ch 路径旁路旧 `binaural_downmix_into()`。
- [x] native multichannel 禁止再次进入 stereo spatial。
- [x] native engine 按 input sample-rate 缓存。
- [x] stereo/native 共用单一 `SpatialSettings → EnvironmentSettings` 映射；关闭 Spatial 时 native 仍保留必要 binaural/pinna，但 synthetic Early/FDN `mix=0`。
- [x] native engine 缓存最后一次 environment，只有参数变化才更新六面反射/FDN 参数，不在每个 decode chunk 重算。
- [x] seek / reopen / processing discontinuity reset EQ、resampler、native/stereo spatial、pinna、late-field 与 peak-limiter envelope。
- [x] transport generation 使用 audio-worker thread-local。
- [x] Static / Immersive3d 使用 stereo L/R front-arc virtual source。
- [x] Orbit8d / Orbit360 / Pendulum / FrontBack / Planetary / NearEar 进入自研 `Trajectory`。
- [x] dynamic stereo 以 rigid stereo pair 运动，保持 authored L/R。
- [x] crossfeed 映射为虚拟扬声器 arc 收窄，不提前混合 PCM。
- [ ] 删除 legacy `src/audio/dsp/spatial.rs` renderer；当前仍作为 failure fallback 与 settings/preset holder。

## Phase 2 — CPU kernel / 声学质量

- [x] block 起点一次生成参数 step，sample loop 只做加法推进。
- [x] reflection step 显式固定为 `[RenderParameterStep; EARLY_REFLECTION_TAP_COUNT]`，避免 `std::array::from_fn` const generic 推断歧义。
- [x] source pose/listener 参数 cache；固定 5.1.4/7.1.4 避免重复 pose solve。
- [x] LFE 跳过方向性 pose solve。
- [x] `CubicDelayLine::read_pair` 共用 ring cursor；整数 delay direct-read fast path。
- [x] stereo L/R stride 直读，无 mono scratch。
- [x] conservative near-field / air / front-back / elevation cues。
- [x] FullRange source 同一 delay ring 同时服务 direct ITD 与 first-order reflection arrivals。
- [x] delay history 约 80 ms reflection budget + ITD margin；初始化后固定容量。
- [x] reflection filter/parameter/cache 均为每 source 固定数组。
- [x] environment `mix=0` 完全跳过 reflection geometry/read/filter 与 late-field 热循环。
- [x] environment 改变只 invalidates reflection cache/filter；direct ITD 与 pinna pose cache 独立保留。
- [x] direct path 双级参数化 pinna externalization：方向 notch + broad spectral shoulder/peak，并保留轻微 per-ear spectral asymmetry。
- [x] notch/shoulder 系数按 block 端点求值并 sample-ramp；静态 speaker pose 命中 cache，sample loop 不做三角函数/`powf`。
- [x] 每耳两级 TDF2 biquad 固定状态、0 allocation；异常样本清状态并输出 0，denormal 主动归零。
- [x] linked-stereo zero-lookahead peak safety limiter：L/R 共用 gain envelope、instantaneous attack、约 90 ms release、默认 ceiling `-0.30 dBFS`、0 added latency / 0 allocation。
- [x] 用户音量 gain 直接进入 limiter detector/application；最终 SIMD hard clamp 仅保留为异常 invariant guard，不再承担正常 limiter 职责。
- [x] channel-order conformance vectors：5.1.4 / 7.1.4 固化显式 `ChannelRole` 顺序、LFE index 3、rear/side/top 几何类别与稳定短名。
- [ ] reflection arrival 是否加入 pinna spectral cue：先以 benchmark/听感证明收益，避免 `6 taps × N sources` 无依据增负载。
- [ ] 更多 hot kernel 下沉 `audio-simd`。
- [ ] Lagrange vs Thiran fractional delay 质量/成本对比。
- [ ] 更完整的多段 pinna bank / 参数标定；当前已完成双级 notch + shoulder，但仍需听感与测量校准。
- [ ] 通用 parameter smoothing/crossfade 自动化测试。
- [ ] limiter ceiling/release/headroom 的实际节目素材标定与 true-peak 校验。

## Phase 3 — 自适应 CPU 多线程

- [ ] 独立 realtime worker pool；不用通用 Rayon pool 作为热路径 scheduler。
- [ ] worker 初始化阶段预创建；audio block 内禁止 spawn。
- [ ] worker-local fixed scratch / partial L-R mix。
- [ ] deterministic reduction。
- [ ] serial SIMD vs parallel cost model。
- [ ] profiling threshold 以下强制 serial。

**当前仍没有实际 benchmark 数据，因此没有并行阈值，也没有默认 worker pool。**

## Phase 4 — 专业空间场 / 对象音频

- [x] listener-local rectangular image-source room；所有 source 共享同一房间几何，不再按 source 单独撑大房间。
- [x] 正式播放路径使用 per-source first-order image-source reflections。
- [x] Left / Right / Front / Rear / **Floor / Ceiling 六面一阶反射**。
- [x] 每条反射按 image arrival direction 进入同一套 binaural ITD/ILD/head-shadow/distance/air 模型。
- [x] reflection 只增加 excess path delay，不给整个音乐节目加入绝对传播延迟。
- [x] 极端越界 source 只在 reflection solver 内夹到房间边界内；direct source pose 不修改。
- [x] LFE 不做伪方向化 image-source reflection。
- [x] 已删除退出正式链的旧全局 `EarlyReflectionNetwork` API/实现。
- [x] direct path 参数化 pinna / externalization spectral layer；当前明确为 generic parametric cue，不宣称 measured HRTF。
- [x] 全局 8-line FDN late diffuse field：8 条预分配 delay、正交 Hadamard feedback、per-line damping、固定容量。
- [x] FDN 只在所有 source 汇合后的最终 stereo block 运行一次；native layout 先 normalization 再激励尾场。
- [x] FDN 使用独立 L/R 正交 injection vectors，保留纯 Side/反相 stereo 的 late-field 激励，不先 collapse 为 mono。
- [x] `environment.mix=0` 为 late-field bit-exact bypass；room-size 改变/reset 清理旧 tail history。
- [x] native 5.1.4/7.1.4 在 Spatial 启用时使用同一六面 Early + FDN room，保持 authored speaker geometry，不启动 stereo motion。
- [ ] absolute room transform / listener-room position；当前仍为 listener-centered room。
- [ ] Audio Vivid object metadata 接入。
- [ ] HOA 参数化 binaural path。
- [ ] object source culling / audibility budget。
- [ ] listener pose / head orientation runtime 接口。
- [ ] source directivity / spread 高级模型。

## Phase 5 — Spatial Debug / 发烧级可视化

- [x] allocation-free 固定尺寸 `SpatialDebugSnapshot`：listener、最多 32 source。
- [x] reflection Debug：`12 source × 6 tap = 72` 固定矩阵，包含 image/bounce/full+excess path/arrival/LR delay+gain。
- [x] Debug 默认关闭；开启时复用固定数组。
- [x] 根播放器原子槽 + odd/even seqlock 发布，无 Mutex/channel/heap publication。
- [x] scene publish 约 30 Hz。
- [x] A/B/C：Original / Post-EQ / Post-Spatial 工程指标。
- [x] GPU 3D Debug 使用 BMCBL GPUI `GpuMesh3d` / WGSL / depth；不修改 GPUI core。
- [x] Debug UI 已从根 `src/` 收拢到 `src/ui/audio_debug_window.rs`、`src/ui/audio_spatial_debug_3d.rs`、`src/ui/audio_spatial_debug_3d.wgsl`；删除旧 `audio_debug_window.rs` / `audio_debug_window_v2.rs` 与根级 3D 文件，不再保留 V2 过渡命名。
- [x] GPUI stage accent 的 `Rgba → Hsla` 显式转换已修复。
- [x] GPU Debug 直接调用公开 `ListenerPose::basis()`，不复制 listener basis 公式。
- [x] GPU 3D scene：listener、heading、authored sources、velocity、真实 6-wall room wireframe/grid、source→bounce→listener paths。
- [x] Floor/Ceiling 使用与 DSP 相同的 room height 与真实 bounce path，可在 3D 中直接观察上下反射。
- [x] mesh id 稳定，以 generation 刷新 GPU cache；UI 约 30 Hz 更新。
- [x] 3D Camera yaw/pitch/zoom/reset。
- [x] Pinna telemetry 复用 realtime cue generator：notch center/Q/depth/cue strength/L-R notch；运行时现已扩展到双级 notch + shoulder。
- [x] FDN telemetry 复用 realtime parameter derivation：wet/feedback/damping cutoff/delay range。
- [x] GPU 3D 明确区分 Early/Late：离散彩色折线路径代表六面 image-source Early，监听者周围半透明多层 volume 代表 FDN Late。
- [ ] Debug channel label 直接改用公开 `ChannelRole`，移除 UI 自己维护的重复 5.1.4/7.1.4 名称数组。
- [ ] object ID / Audio Vivid metadata 可视化。

## Phase 6 — 音频 GPU Compute（暂缓）

仅指音频 DSP 计算；Debug GPU 3D 已启用，不属于 realtime audio compute。

- [ ] 仅在长卷积、大量 object 或高阶 HOA break-even 明确时评估。
- [ ] persistent GPU buffers/pipeline。
- [ ] 禁止 CPU↔GPU 每个小 DSP stage 往返。
- [ ] 必须以 end-to-end callback latency / CPU time 证明收益。

## Serial SIMD benchmark

```text
cargo run --release -p yinqidao-audio-spatial --example cpu_bench
```

现有源码 case：32 / 64 / 128 frames 的 stereo static、Orbit360、FigureEight/Orbit8d、5.1.4、7.1.4。

已加入用于当前决策的 64-frame 对照：

- dry baseline：stereo / Orbit360 / 5.1.4 / 7.1.4；
- `environment.mix=0.10`：stereo / Orbit360 / 7.1.4；
- `environment.mix=0.30`：stereo / 7.1.4；
- Debug off/on：stereo 与 7.1.4；
- 每个 case 输出 avg/p50/p95/p99/worst 与 audio deadline budget；
- direct 双级 pinna 始终属于 FullRange baseline，room case 增加六面 Early + 全局 FDN。

仍需后续增加：

- 可切 pinna off/on 的精确边际成本（当前 pinna 属于 baseline）；
- limiter off/on 边际成本与 gain-reduction 统计；
- reflection-pinna prototype cost；
- 16/32/64 future objects；
- AVS3 7.1.4 end-to-end。

## 2026-09-09 本地编译反馈与修复

用户本地构建已实际暴露以下源码/API 问题；对应修复已提交，但**尚未收到修复后的完整 `cargo check` 成功结果**：

1. `renderer.rs`：`std::array::from_fn` 无法推断 `reflection_steps` const generic 长度（E0284）
   - 已显式标注 `[RenderParameterStep; EARLY_REFLECTION_TAP_COUNT]`。
2. `audio_debug_window_v2.rs`：`stage_card` 需要 `Hsla`，`rgb(...)` 返回 `Rgba`（E0308）
   - 已在新 `src/ui/audio_debug_window.rs` 三处 accent 调用显式 `.into()`。
3. `audio_spatial_debug_3d.rs`：应用 crate 无法调用 `pub(crate) ListenerPose::basis()`（E0624）
   - 已把 `basis()` 提升为公开只读几何 API，并保持算法实现唯一来源。
4. Debug 文件结构
   - 已删除根 `src/` 的旧窗口/V2/3D 文件，统一收拢到 `src/ui/`。

## 当前验证状态

- **本助手环境尚未执行 `cargo check`**；
- **本助手环境尚未执行 `cargo test`**；
- **尚未执行 `cpu_bench`**；
- 用户本地编译已推进到并反馈上述编译错误，但修复后是否完整通过仍待下一次本地构建确认；
- 新增 channel-order conformance tests 已写入源码，但尚未执行；
- **尚未得到 serial/parallel break-even**；
- `Cargo.lock` 尚未通过当前环境中的 Cargo 重新生成/校验。

不得把以上未确认项目描述为已经通过。

具备 toolchain 后优先：

```text
cargo check -p yinqidao-audio-spatial
cargo test -p yinqidao-audio-spatial
cargo check -p yin_qi_dao
cargo run --release -p yinqidao-audio-spatial --example cpu_bench
```

## 下一步

1. 用户本地重新执行 `cargo check`，继续消除剩余 GPUI 3D / pinna / limiter / native-room type/API 问题，直到根包完整通过。
2. 让 Audio Laboratory 的 channel label 改为读取公开 `ChannelRole`，并增加 codec→spatial handoff bridge test，避免 Debug 名称表和真实 layout 分叉。
3. 实际跑 serial benchmark，分别测 dry / 双级 pinna / 六面 Early / FDN / limiter / Debug 的边际成本。
4. 用真实音乐与峰值测试素材标定 limiter ceiling/release、post-spatial headroom 与 true-peak 风险；不依赖 hard clamp 塑形。
5. 只有 benchmark + 听感同时证明收益时才尝试 reflection-pinna / tap audibility budget / realtime worker pool。
6. 编译与性能稳定后删除 legacy `src/audio/dsp/spatial.rs` renderer/fallback。
7. 再推进 absolute room transform、listener runtime orientation、source directivity 与 Audio Vivid object metadata。
