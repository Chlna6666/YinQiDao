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
- [x] Stereo / 5.1 / 7.1 / 5.1.4 / 7.1.4 speaker scene。
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
- [x] source accumulation 复用 `yinqidao-audio-simd::mix_accumulate`。
- [x] NaN/Inf 在进入 delay/IIR persistent state 前隔离。

## Phase 1 — 播放器正式接入

- [x] 根播放器依赖 `yinqidao-audio-spatial`。
- [x] 10/12ch 正常播放直接进入 native spatial engine。
- [x] AVS3 5.1.4/7.1.4 在输入采样率下先 binaural render，再做 streaming stereo resample。
- [x] 正常 10/12ch 路径旁路旧 `binaural_downmix_into()`。
- [x] native multichannel 禁止再次进入 stereo spatial。
- [x] native engine 按 input sample-rate 缓存。
- [x] seek / reopen / processing discontinuity reset EQ、resampler、native/stereo spatial state。
- [x] transport generation 使用 audio-worker thread-local。
- [x] Static / Immersive3d 使用 stereo L/R front-arc virtual source。
- [x] Orbit8d / Orbit360 / Pendulum / FrontBack / Planetary / NearEar 进入自研 `Trajectory`。
- [x] dynamic stereo 以 rigid stereo pair 运动，保持 authored L/R。
- [x] crossfeed 映射为虚拟扬声器 arc 收窄，不提前混合 PCM。
- [ ] 删除 legacy `src/audio/dsp/spatial.rs` renderer；当前仍作为 failure fallback 与 settings/preset holder。

## Phase 2 — CPU kernel / 声学质量

- [x] block 起点一次生成参数 step，sample loop 只做加法推进。
- [x] source pose/listener 参数 cache；固定 5.1.4/7.1.4 避免重复 pose solve。
- [x] LFE 跳过方向性 pose solve。
- [x] `CubicDelayLine::read_pair` 共用 ring cursor；整数 delay direct-read fast path。
- [x] stereo L/R stride 直读，无 mono scratch。
- [x] conservative near-field / air / front-back / elevation cues。
- [x] FullRange source 同一 delay ring 同时服务 direct ITD 与 first-order reflection arrivals。
- [x] delay history 约 80 ms reflection budget + ITD margin；初始化后固定容量。
- [x] reflection filter/parameter/cache 均为每 source 固定数组。
- [x] environment `mix=0` 完全跳过 reflection geometry/read/filter 热循环。
- [x] environment 改变只 invalidates reflection cache/filter，不清 direct ITD/history。
- [ ] 更多 hot kernel 下沉 `audio-simd`。
- [ ] Lagrange vs Thiran fractional delay 质量/成本对比。
- [ ] 专业外化：参数化 pinna front/back/elevation notch bank。
- [ ] parameter smoothing/crossfade 自动化测试。
- [ ] channel-order conformance vectors。
- [ ] headroom / limiter 重新标定。

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
- [ ] absolute room transform / listener-room position；当前仍为 listener-centered room。
- [ ] diffuse late field / 低成本 8-line FDN room。
- [ ] 参数化 pinna / externalization spectral layer。
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
- [x] GPU 3D Debug V2 使用 BMCBL GPUI `GpuMesh3d` / WGSL / depth；不修改 GPUI core。
- [x] GPU 3D scene：listener、heading、authored sources、velocity、真实 6-wall room wireframe/grid、source→bounce→listener paths。
- [x] Floor/Ceiling 使用与 DSP 相同的 room height 与真实 bounce path，可在 3D 中直接观察上下反射。
- [x] mesh id 稳定，以 generation 刷新 GPU cache；UI 约 30 Hz 更新。
- [x] 3D Camera yaw/pitch/zoom/reset。
- [ ] object ID / Audio Vivid metadata 可视化。
- [ ] externalization / late-field / pinna cue telemetry。

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

下一批必须增加：environment mix=0/0.10/0.30、4-wall vs 6-wall cost、Debug off/on、16/32/64 objects、AVS3 7.1.4 end-to-end。

## 当前验证状态

- **尚未执行 `cargo check`**；
- **尚未执行 `cargo test`**；
- **尚未执行 `cpu_bench`**；
- **尚未得到 serial/parallel break-even**；
- `Cargo.lock` 尚未通过 Cargo 重新生成/校验。

不得把以上项目描述为已经通过。

具备 toolchain 后优先：

```text
cargo check -p yinqidao-audio-spatial
cargo test -p yinqidao-audio-spatial
cargo check -p yin_qi_dao
cargo run --release -p yinqidao-audio-spatial --example cpu_bench
```

## 下一步

1. 实际 `cargo check/test`，优先修复 GPUI 3D V2 / renderer type/API 问题。
2. 实现参数化 pinna externalization notch bank，重点解决前后/上下与头外定位不足。
3. 实现低成本固定容量 FDN late diffuse field，补充包围与房间空气感而不做糊化型普通混响。
4. Debug 3D 增加 pinna/externalization/late-field telemetry。
5. 完成 channel-order conformance + headroom/limiter 标定后删除 legacy stereo renderer。
6. 只有 serial baseline 数据证明收益后才实现 realtime worker pool；音频 GPU compute 继续暂缓。
