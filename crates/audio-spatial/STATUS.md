# YinQiDao Spatial Engine — 实现状态

更新时间：2026-09-09

## 目标与不可退化约束

`yinqidao-audio-spatial` 是音栖岛自研的 CPU 优先空间音频核心。不依赖 SOFA、KEMAR、外部 HRTF 数据库或第三方空间音频 runtime。

实时渲染路径长期约束：

- 音频 block 内 **0 heap allocation / 0 Vec growth**。
- **0 Mutex / 0 RwLock / 0 blocking channel / 0 system call / 0 file I/O**。
- 不在音频 block 内创建线程、FFT plan、设备或其他运行时资源。
- PCM 输入优先以只读 slice/stride 直接消费；禁止先复制为每声道 `Vec`。
- 多声道输入直接作为多个虚拟声源渲染，不先做破坏空间信息的 stereo downmix。
- 普通 stereo 的 L/R 也必须保留为两个独立虚拟声源，不允许先 collapse 成 mono 再做 3D/360/8D。
- 输出使用调用方提供的缓冲区；workspace 在 `SpatialEngine::new` / bridge 初始化时一次性预分配。
- CPU 优先复用 `yinqidao-audio-simd` 的 Scalar/SSE2/AVX2/AVX2+FMA/NEON runtime dispatch。
- 多线程只有在 benchmark 证明总成本低于串行 SIMD 时才启用；禁止给 64-frame 小任务增加调度负担。
- GPU 暂缓，在 CPU 路径有稳定性能基线前不引入 GPU 同步/拷贝复杂度。
- 当前定位算法属于**参数化双耳/虚拟声源定位**，不得宣称为测量型 HRTF。

## 已完成 — Phase 0 / CPU realtime foundation

- [x] 独立 crate：`crates/audio-spatial`。
- [x] 固定内部 block，默认 64 frames，可配置。
- [x] `SpatialEngine` 预分配左右 mix buffer 与 source workspace。
- [x] `SourcePose` / `ListenerPose` / `Vec3`。
- [x] Stereo / 5.1 / 7.1 / 5.1.4 / 7.1.4 speaker scene。
- [x] 7.1.4 顺序与当前 AVS3 PCM 输出约定对齐，LFE 为 index 3。
- [x] interleaved N-channel 通过 stride 直接读取，无 per-channel PCM 副本。
- [x] 每个原生输入声道映射为独立虚拟 source。
- [x] stereo L/R 独立虚拟声源渲染 primitive：`render_interleaved_stereo_pair`。
- [x] 四点 Lagrange fractional delay。
- [x] Woodworth spherical-head ITD 参数模型。
- [x] 连续 ILD / far-ear attenuation。
- [x] head-shadow 一阶低通基础模型。
- [x] front/rear、elevation、distance 参数化线索基础。
- [x] LFE 独立低通并对称注入双耳。
- [x] block 参数使用 end-exclusive `[n, n + frames)` 时钟语义；修复 64-frame 轨迹约 `64/63` 的速度漂移。
- [x] sample-clock 轨迹：Orbit360 / FigureEight / Pendulum / FrontBack / Planetary / NearEar / Helix。
- [x] 轨迹支持 clockwise / counter-clockwise。
- [x] 每个运动 block 只在轨迹端点计算三角函数。
- [x] 初始化后无 allocation 的 early-reflection ring network。
- [x] source accumulation 使用 `yinqidao-audio-simd::mix_accumulate`。
- [x] reset 复用已分配缓冲区。
- [x] 单元测试源码覆盖 listener basis、7.1.4 layout、fractional delay、方向性、sample-clock 连续性、stereo pair 与 7.1.4 基础渲染。

## Phase 1 — 播放器正式接入

- [x] 根播放器依赖 `yinqidao-audio-spatial`。
- [x] `AudioProcessor` 的 10/12ch 正常播放路径直接调用新 engine。
- [x] AVS3 5.1.4/7.1.4 在输入采样率下先完成双耳渲染，再进入 streaming stereo resampler。
- [x] 正常 10/12ch 路径旁路旧 `binaural_downmix_into()` 的近似折叠；旧表仅保留为异常恢复 fallback。
- [x] 原生多声道禁止再次进入 stereo effect renderer，避免 double-spatialize。
- [x] native spatial engine 按输入 sample rate 缓存；采样率变化才重建，不逐 chunk 构造。
- [x] seek / track reopen / processing timeline discontinuity 会 reset EQ、resampler、native spatial、stereo spatial delay/filter/trajectory state。
- [x] seek transport generation 使用 audio-worker thread-local，避免多个播放器/预加载线程互相触发 reset。
- [x] stereo Static/Immersive3d 进入 L/R 双虚拟声源 front-arc renderer。
- [x] stereo Orbit8d / Orbit360 / Pendulum / FrontBack / Planetary / NearEar 进入 audio-clock `Trajectory`。
- [x] 动态 stereo 保留 L/R 两个 source，整组 stereo pair 作为刚性声场运动，不再只移动 mono centre。
- [x] stereo bridge scratch 固定为内部 block 大小，长 chunk 分块处理，运行时不因 chunk 长度增长 workspace。
- [x] `crossfeed` 在新 stereo 路径中转为虚拟扬声器 arc 收窄，不提前互混 PCM，相位关系保留到 binaural renderer。
- [x] environment 参数仅在设置变化时更新，不在正常 audio block 重算 delay/filter 配置。
- [ ] 删除旧 `src/audio/dsp/spatial.rs` 的渲染实现；目前只作为新 renderer 构造/运行失败的兼容 fallback 和 preset/settings holder。

## Phase 2 — CPU kernel 与声学质量

- [x] FullRange 双耳参数在 block 起点一次生成六字段增量，sample 热循环只递增参数。
- [x] LFE gain ramp 改为 block 级增量；strided PCM 读取使用递增 cursor。
- [x] 每 source 缓存最后一次 `SourcePose + ListenerPose -> RenderParameters`；固定布局首 block 后不重复昂贵 pose solve。
- [x] LFE direction-independent 路径跳过双耳 pose 参数求解。
- [x] `CubicDelayLine::read_pair` 共享双耳 cursor/length；整数 delay 走直接历史读取。
- [x] stereo L/R pair 直接读取 interleaved source stride，无 mono scratch / per-channel Vec。
- [x] 近场使用左右耳几何距离修正 ILD，并小比例混合 point-to-ear 几何到 Woodworth ITD；近场不额外放大整体增益，避免 headroom 负担。
- [x] 距离大于 1 m 后加入平滑的高频空气吸收参数，normal music distance 下保持保守。
- [x] front/rear/elevation 高频线索按连续参数调整；后方、下方与远距离逐步降低 upper-band 能量，不使用外部 HRTF 数据。
- [x] 输入 PCM、source gain/spread/position 的 NaN/Inf 在进入持久 delay/IIR 状态前隔离。
- [ ] interleave/deinterleave、双耳 source gain、双耳 accumulation、filter bank 等热点继续下沉到 `audio-simd`。
- [ ] 4-point Lagrange 与 Thiran fractional delay 质量/成本对比。
- [ ] 更完整 front/back pinna-like notch / spectral cue（仍保持参数化，不伪称 measured HRTF）。
- [ ] 参数 crossfade/smoothing 自动化测试。
- [ ] 5.1/7.1/5.1.4/7.1.4 channel-order conformance vectors。
- [ ] 峰值/能量 headroom 与 limiter 重新标定。

## Phase 3 — 自适应 CPU 多线程

- [ ] 独立 realtime worker pool，不用通用 Rayon pool 作为热路径调度器。
- [ ] worker 在播放/设备初始化阶段预创建，block 内禁止 spawn。
- [ ] 每 worker 固定 scratch/partial L/R mix。
- [ ] deterministic reduction。
- [ ] 串行 SIMD / 并行 source workload cost model。
- [ ] 小工作量强制串行，跨过 profiling threshold 才并行。
- [ ] deadline miss 时允许安全回退，不阻塞设备 callback。

**重要：截至当前没有 benchmark 数据，因此没有并行阈值，也没有默认启用 worker pool。禁止凭经验猜 `12 sources / 64 frames` 应该并行。**

## Phase 4 — 专业空间场 / 对象音频

- [ ] 多 tap 几何 early reflections。
- [ ] diffuse late field / 低成本 FDN room。
- [ ] object-based AVS3 Audio Vivid source metadata 接入。
- [ ] HOA 参数化 binaural 路径。
- [ ] 大量 objects source culling / audibility budget。
- [ ] listener pose / head orientation runtime 接口。
- [ ] source directivity / spread 更完整模型。

## Phase 5 — Spatial Debug / 发烧级可视化

目标是同时观察**原版输入、EQ 后、空间后、最终输出以及整个虚拟声场本身**，而不是只有频谱。

- [x] crate 提供 allocation-free 固定尺寸 `SpatialDebugSnapshot`：listener、最多 32 个 source pose、azimuth/elevation/distance、ITD、ILD、near-field、head-shadow、air absorption、direct/environment contribution。
- [x] `SpatialEngine` Debug 默认关闭；开启时复用内置固定数组，不创建 Vec、不向 UI 持锁。
- [x] 根播放器通过固定原子槽 + odd/even seqlock 发布空间场景；最多 32 source，每 source 24 个固定 word，无 `Mutex` / channel / heap publication。
- [x] `AudioProcessor` 仅在 Audio Laboratory 开启时启用 scene capture，并按 `sample_rate / 30` 帧间隔将发布频率限制到约 30 Hz；关闭后清理 published scene。
- [x] GPUI Top View：listener、朝向、距离环、stereo L/R 与 native 5.1.4/7.1.4 当前 source position；高度声道通过 marker 与 elevation telemetry 区分，不错误投影为平面距离。
- [x] GPUI Source Telemetry：最多展开 12 source，显示 azimuth / elevation / distance / ITD / ILD / L-R gain / near-field / head-shadow / air / direct contribution。
- [ ] trajectory 历史尾迹 / 速度矢量可视化；当前快照只表示最新动态位置。
- [ ] early-reflection 几何路径可视化；当前仅发布 environment contribution，不伪造反射几何。
- [ ] GPUI Front/Elevation View。
- [x] Original / Post-EQ / Post-Spatial A/B/C reference。
- [x] Vectorscope / stereo correlation。
- [x] Mid/Side spectrum、S/M energy、peak/RMS/crest/LUFS 工程分析。
- [x] Original vs Processed transfer ΔdB / Mid-Side / spectrogram / waveform overlay。
- [ ] native 5.1.4/7.1.4 channel-name/order inspector；当前 Top View 与 telemetry 已显示全部当前 source index/position，但尚未给每个 index 标注规范声道名。

## Phase 6 — GPU（暂缓）

- [ ] 仅在长卷积、大量 object 或高阶 HOA 等 break-even 明确时评估。
- [ ] persistent GPU buffers/pipeline，禁止 per-block resource creation。
- [ ] 禁止 CPU↔GPU 每个小 DSP stage 往返。
- [ ] 必须以 end-to-end callback latency/CPU time 证明收益后才能默认启用。

## Serial SIMD benchmark 基线

已加入稳定版 Rust 可运行的基准入口：

```text
cargo run --release -p yinqidao-audio-spatial --example cpu_bench
```

当前源码基准覆盖以下 workload，全部提供 32 / 64 / 128 frames：

- stereo static virtual pair；
- stereo Orbit360；
- stereo FigureEight / Orbit8d 核心轨迹；
- 5.1.4；
- 7.1.4。

输出 current SIMD backend、average / p50 / p95 / p99 / worst 和 realtime deadline 百分比。**这些 case 已加入源码，但本会话尚未实际运行 benchmark。**

后续必须补：5.1、7.1、AV3A 7.1.4 end-to-end、16/32/64 objects，以及 Debug off/on 对比。worker-pool 版本必须把 dispatch/wakeup/reduction 全部计入，不允许只测 worker kernel。

## 当前验证状态

当前会话环境没有可用的本地 Rust workspace/toolchain 验证路径：

- **尚未执行 `cargo check`**。
- **尚未执行 `cargo test`**。
- **尚未执行 `cpu_bench`**。
- **尚未得到 serial/parallel break-even 数据**。
- workspace 变更后的 `Cargo.lock` 尚未通过 Cargo 重新生成/校验。

后续不得把这些项目描述为已经通过。具备 Rust toolchain 后优先执行：

```text
cargo check -p yinqidao-audio-spatial
cargo test -p yinqidao-audio-spatial
cargo check -p yin_qi_dao
cargo run --release -p yinqidao-audio-spatial --example cpu_bench
```

## 下一笔建议

1. 首先在可用 Rust toolchain 上执行 `cargo check/test`，修复任何类型/借用/GPUI API 问题；随后运行新增 stereo + native serial baseline，并对比 Debug off/on 成本。
2. 给 Spatial Debug 增加固定容量 trajectory history 与 Front/Elevation View，再增加 early-reflection 几何调试数据；历史缓冲仍必须固定容量、无音频线程分配。
3. 给 native 5.1.4/7.1.4 source index 加明确 channel-name/order inspector 和 conformance vectors。
4. 完成 headroom / limiter 标定与 channel-order conformance 后，再删除 legacy stereo renderer。
5. 有 serial baseline 后才设计 realtime worker pool 与 parallel threshold；GPU 继续暂缓。
