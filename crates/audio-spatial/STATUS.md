# YinQiDao Spatial Engine — 实现状态

更新时间：2026-09-09

## 核心目标

`yinqidao-audio-spatial` 是 YinQiDao 自研的 CPU-first 参数化空间音频核心。当前不依赖 SOFA、KEMAR、外部 HRTF 数据库或第三方空间音频 runtime；当前定位算法必须描述为**参数化双耳 / 虚拟声源定位**，不得宣称 measured HRTF。

实时路径长期硬约束：

- audio block 内 0 heap allocation / 0 Vec growth；
- 0 Mutex / RwLock / blocking channel / file I/O / system call；
- 不在 audio block 内 spawn thread、构建 FFT plan 或创建运行时资源；
- interleaved PCM 通过 borrowed slice + stride 直接消费；
- 多声道在保持 authored channel geometry 的前提下直接作为虚拟 source 渲染，不先破坏性 downmix；
- stereo L/R 保持两个独立 source，不 collapse 为 mono 再做 3D/360/8D；
- CPU 优先复用 `yinqidao-audio-simd` 的 Scalar/SSE2/AVX2/AVX2+FMA/NEON dispatch；
- realtime worker pool 只有 benchmark 证明收益后才能启用；音频 GPU compute 暂缓。

## Phase 0 — CPU realtime foundation

- [x] 独立 crate：`crates/audio-spatial`。
- [x] 默认 64-frame 固定内部 block，可配置。
- [x] `SpatialEngine` / source workspace 初始化时预分配。
- [x] `Vec3` / `SourcePose` / `ListenerPose`，具备无分配 `Add/Sub/Mul<f32>` 几何 primitive。
- [x] Stereo / 5.1 / 7.1 / 5.1.4 / 7.1.4 speaker scene。
- [x] 7.1.4 顺序与当前 AVS3 PCM 约定对齐，LFE index 3。
- [x] interleaved N-channel stride 直读，无 per-channel Vec。
- [x] stereo pair 独立虚拟 source primitive。
- [x] 4-point Lagrange fractional delay。
- [x] Woodworth spherical-head ITD。
- [x] 连续 ILD / far-ear attenuation / head-shadow 一阶低通。
- [x] near-field ear-distance correction、distance law、air absorption、front/rear/elevation 参数化线索。
- [x] LFE 独立 causal delay + 120 Hz low-pass。
- [x] end-exclusive `[n, n + frames)` 参数 ramp，修复 64-frame trajectory `64/63` 时钟漂移。
- [x] Orbit360 / FigureEight / Pendulum / FrontBack / Planetary / NearEar / Helix audio-clock trajectory。
- [x] clockwise / counter-clockwise。
- [x] source accumulation 复用 `yinqidao-audio-simd::mix_accumulate`。
- [x] NaN/Inf 在进入 delay/IIR persistent state 前隔离。

## Phase 1 — 播放器正式接入

- [x] 根播放器依赖 `yinqidao-audio-spatial`。
- [x] 10/12ch 正常播放直接进入 native spatial engine。
- [x] AVS3 5.1.4/7.1.4 在输入采样率下先 binaural render，再做 streaming stereo resample。
- [x] 正常 10/12ch 路径旁路旧 `binaural_downmix_into()`；旧实现仅保留异常 fallback。
- [x] native multichannel 禁止再次进入 stereo spatial，避免 double-spatialize。
- [x] native engine 按 input sample-rate 缓存，不逐 chunk 构造。
- [x] seek / reopen / processing discontinuity reset EQ、resampler、native/stereo spatial delay/filter/trajectory state。
- [x] transport generation 使用 audio-worker thread-local。
- [x] Static / Immersive3d 使用 stereo L/R front-arc virtual source。
- [x] Orbit8d / Orbit360 / Pendulum / FrontBack / Planetary / NearEar 进入自研 `Trajectory`。
- [x] dynamic stereo 以 rigid stereo pair 运动，保持 authored L/R。
- [x] stereo bridge scratch 固定尺寸，长 chunk 分块。
- [x] crossfeed 映射为虚拟扬声器 arc 收窄，不提前混合 PCM。
- [ ] 删除 legacy `src/audio/dsp/spatial.rs` renderer；当前仍作为 construction/render failure fallback 与 settings/preset holder。

## Phase 2 — CPU kernel / 声学质量

- [x] block 起点一次生成参数 step，sample loop 只做加法推进。
- [x] source pose/listener 参数 cache；固定 5.1.4/7.1.4 首 block 后避免重复昂贵 pose solve。
- [x] LFE 跳过方向性 pose solve。
- [x] `CubicDelayLine::read_pair` 共用 ring cursor；整数 delay direct-read fast path。
- [x] stereo L/R stride 直读，无 mono scratch。
- [x] conservative near-field / air / front-back / elevation cues。
- [x] FullRange source 的同一条 delay ring 同时服务 direct ITD 与 4 条 first-order reflection arrival，不复制 PCM、不创建额外 source object。
- [x] delay history 扩到约 80 ms reflection budget + ITD margin；初始化后固定容量。
- [x] reflection filter state 为每 source 固定 `[f32; 4]` 左/右数组；reflection parameters/cache 也是固定数组。
- [x] environment `mix=0` 时完全跳过 reflection geometry/read/filter 热循环。
- [x] environment 改变只 invalidates reflection cache/filter，不清 direct ITD/history。
- [ ] 更多 hot kernel 下沉 `audio-simd`。
- [ ] Lagrange vs Thiran fractional delay 质量/成本对比。
- [ ] 专业外化：参数化 pinna front/back/elevation notch bank。
- [ ] parameter smoothing/crossfade 自动化测试。
- [ ] 5.1/7.1/5.1.4/7.1.4 channel-order conformance vectors。
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

- [x] `image_source.rs`：listener-local rectangular room 的 first-order image-source 几何。
- [x] 正式播放路径使用 per-source Left/Right/Front/Rear first-order reflections。
- [x] 每条反射按 image arrival direction 再经过同一套 binaural ITD/ILD/head-shadow/distance/air 模型。
- [x] reflection 只增加 excess path delay，不给整个音乐节目加入绝对传播延迟。
- [x] LFE 不做伪方向化 image-source reflection。
- [ ] ceiling / floor first-order reflections。
- [ ] absolute room transform / listener-room position，而不是当前 listener-centric scalar room。
- [ ] diffuse late field / 低成本 8-line FDN room。
- [ ] 参数化 pinna / externalization spectral layer。
- [ ] Audio Vivid object metadata 接入。
- [ ] HOA 参数化 binaural path。
- [ ] object source culling / audibility budget。
- [ ] listener pose / head orientation runtime 接口。
- [ ] source directivity / spread 高级模型。

## Phase 5 — Spatial Debug / 发烧级可视化

目标：同时观察原版输入、EQ 后、空间后、最终输出，以及整个虚拟场景与空间 DSP 内部 cue。

- [x] allocation-free 固定尺寸 `SpatialDebugSnapshot`：listener、最多 32 source、ITD/ILD/distance/near-field/head-shadow/air/direct/environment。
- [x] reflection Debug 已扩展为 `12 source × 4 tap = 48` 固定矩阵；每条含 source index、image/bounce position、full/excess path、arrival az/el、L/R delay/gain。
- [x] Debug 默认关闭；开启时复用 engine 内固定数组。
- [x] 根播放器固定原子槽 + odd/even seqlock 发布，无 Mutex/channel/heap publication。
- [x] scene publish 限制到约 30 Hz，关闭 Audio Laboratory 后清理 published scene。
- [x] A/B/C：Original / Post-EQ / Post-Spatial 工程指标与波形参考。
- [x] 旧 Top/Front 工程视图已完成，可作为辅助分析口径。
- [x] 新 GPU 3D Debug V2：直接使用 BMCBL GPUI `GpuMesh3d` / WGSL / depth；不修改 GPUI core。
- [x] GPU 3D scene 包含 listener、heading、authored sources、velocity vectors、room wireframe/grid、48 条 source→bounce→listener reflection paths。
- [x] 3D mesh 保持稳定 mesh id，以 generation 刷新 GPU cache；UI 约 30 Hz 更新，不影响 audio realtime path。
- [x] 3D Camera 提供 yaw/pitch/zoom/reset 调试控制。
- [ ] floor/ceiling 音频模型完成后，让 3D room 高度和上下反射使用真正 DSP room geometry，而不是当前调试高度估计。
- [ ] object ID / Audio Vivid metadata 可视化。
- [ ] externalization / late-field / pinna cue 的内部 telemetry。

## Phase 6 — 音频 GPU Compute（暂缓）

这里仅指**音频 DSP 计算**。Phase 5 的 Debug 3D 可视化已经使用 GPU，不属于 audio realtime compute。

- [ ] 仅在长卷积、大量 object 或高阶 HOA break-even 明确时评估。
- [ ] persistent GPU buffers/pipeline。
- [ ] 禁止 CPU↔GPU 每个小 DSP stage 往返。
- [ ] 必须以 end-to-end callback latency / CPU time 证明收益。

## Serial SIMD benchmark

运行入口：

```text
cargo run --release -p yinqidao-audio-spatial --example cpu_bench
```

源码 case 已覆盖 32 / 64 / 128 frames：stereo static、Orbit360、FigureEight/Orbit8d、5.1.4、7.1.4。

下一批 benchmark 必须增加：environment mix=0/0.10/0.30、stereo/native image-source、Debug off/on、16/32/64 objects、AVS3 7.1.4 end-to-end。

## 当前验证状态

本会话环境仍没有可用本地 Rust workspace/toolchain 验证路径：

- **尚未执行 `cargo check`**；
- **尚未执行 `cargo test`**；
- **尚未执行 `cpu_bench`**；
- **尚未得到 serial/parallel break-even**；
- `Cargo.lock` 尚未通过 Cargo 重新生成/校验。

不得把以上项目描述为已经通过。具备 toolchain 后优先：

```text
cargo check -p yinqidao-audio-spatial
cargo test -p yinqidao-audio-spatial
cargo check -p yin_qi_dao
cargo run --release -p yinqidao-audio-spatial --example cpu_bench
```

## 下一步

1. 实际 `cargo check/test`，优先修复 GPUI 3D V2 / renderer 的 type/API 问题。
2. 声音本体优先补 ceiling/floor + 参数化 pinna externalization + 低成本 FDN late diffuse field，解决“有左右移动但不够头外/不够包围”的问题。
3. 让 Debug 3D 直接显示真实 room height、floor/ceiling reflection、late-field energy、externalization telemetry。
4. 完成 channel-order conformance + headroom/limiter 标定后删除 legacy stereo renderer。
5. 只有 serial baseline 数据证明收益后才实现 realtime worker pool；音频 GPU compute 继续暂缓。
