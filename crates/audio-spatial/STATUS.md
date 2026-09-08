# YinQiDao Spatial Engine — 实现状态

更新时间：2026-09-08

## 目标与不可退化约束

`yinqidao-audio-spatial` 是音栖岛自研的 CPU 优先空间音频核心。不依赖 SOFA、KEMAR、外部 HRTF 数据库或第三方空间音频 runtime。

实时渲染路径长期约束：

- 音频 block 内 **0 heap allocation / 0 Vec growth**。
- **0 Mutex / 0 RwLock / 0 blocking channel / 0 system call / 0 file I/O**。
- 不在音频 block 内创建线程、FFT plan、设备或其他运行时资源。
- PCM 输入优先以只读 slice/stride 直接消费；禁止先复制为每声道 `Vec`。
- 多声道输入直接作为多个虚拟声源渲染，不先做破坏空间信息的 stereo downmix。
- 输出使用调用方提供的缓冲区；workspace 在 `SpatialEngine::new` 时一次性预分配。
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
- [x] 四点 Lagrange fractional delay。
- [x] Woodworth spherical-head ITD 参数模型。
- [x] 连续 ILD / far-ear attenuation。
- [x] head-shadow 一阶低通基础模型。
- [x] front/rear、elevation、distance 参数化线索基础。
- [x] LFE 独立低通并对称注入双耳。
- [x] block 首尾参数插值。
- [x] sample-clock 轨迹：Orbit360 / FigureEight / Pendulum / FrontBack / Planetary / NearEar / Helix。
- [x] 每个运动 block 只在轨迹端点计算三角函数。
- [x] 初始化后无 allocation 的 early-reflection ring network。
- [x] source accumulation 使用 `yinqidao-audio-simd::mix_accumulate`。
- [x] reset 复用已分配缓冲区。
- [x] 单元测试源码覆盖 listener basis、7.1.4 layout、fractional delay、方向性、sample-clock 连续性和 7.1.4 基础渲染。

## 尚未完成 — 不得提前标记

### Phase 1 — 播放器正式接入

- [ ] 根播放器依赖 `yinqidao-audio-spatial`。
- [ ] `AudioProcessor` 的 10/12ch 路径直接调用新 engine。
- [ ] 删除/旁路旧 `binaural_downmix_into()` 对原生 5.1.4/7.1.4 的近似折叠。
- [ ] stereo 3D/360/8D presets 迁移到新 `Trajectory`。
- [ ] 旧 `src/audio/dsp/spatial.rs` 在功能完全迁移后删除。
- [ ] seek / track switch / output-rate change 正确 reset/recreate 空间状态。

### Phase 2 — CPU kernel 与质量

- [ ] interleave/deinterleave、双耳 source gain、双耳 accumulation、filter bank 等热点下沉到 `audio-simd`。
- [ ] 4-point Lagrange 与 Thiran fractional delay 质量/成本对比。
- [ ] 更完整 front/back、elevation、near-field、air absorption。
- [ ] 参数 crossfade/smoothing 自动化测试。
- [ ] 5.1/7.1/5.1.4/7.1.4 channel-order conformance vectors。
- [ ] NaN/Inf 隔离。
- [ ] 峰值/能量 headroom 与 limiter 重新标定。

### Phase 3 — 自适应 CPU 多线程

- [ ] 独立 realtime worker pool，不用通用 Rayon pool 作为热路径调度器。
- [ ] worker 在播放/设备初始化阶段预创建，block 内禁止 spawn。
- [ ] 每 worker 固定 scratch/partial L/R mix。
- [ ] deterministic reduction。
- [ ] 串行 SIMD / 并行 source workload cost model。
- [ ] 小工作量强制串行，跨过 profiling threshold 才并行。
- [ ] deadline miss 时允许安全回退，不阻塞设备 callback。

### Phase 4 — 环境与对象音频

- [ ] 多 tap 几何 early reflections。
- [ ] diffuse late field / 低成本 FDN room。
- [ ] object-based AVS3 Audio Vivid source metadata 接入。
- [ ] HOA 参数化 binaural 路径。
- [ ] 大量 objects source culling / audibility budget。

### Phase 5 — GPU（暂缓）

- [ ] 仅在长卷积、大量 object 或高阶 HOA 等 break-even 明确时评估。
- [ ] persistent GPU buffers/pipeline，禁止 per-block resource creation。
- [ ] 禁止 CPU↔GPU 每个小 DSP stage 往返。
- [ ] 必须以 end-to-end callback latency/CPU time 证明收益后才能默认启用。

## 性能基准计划

至少记录 stereo trajectory、5.1、7.1、5.1.4、AV3A 7.1.4 12ch、16/32/64 objects；覆盖 Scalar/SSE2/AVX2/AVX2+FMA/NEON 和 block 32/64/128；记录平均、p95、p99、worst block、CPU 占用、额外延迟、allocation count。

没有 benchmark 数据前，不把“线程更多”“block 更大”或“GPU”视为优化。

## 当前验证状态

当前会话环境没有可用 `cargo` / `rustc`：

- **尚未执行 `cargo check`**。
- **尚未执行 `cargo test`**。
- **尚未执行 benchmark**。
- workspace 变更后的 `Cargo.lock` 尚未通过 Cargo 重新生成/校验。

后续不得把这些项目描述为已经通过。具备 Rust 1.89+ toolchain 后优先执行：

```text
cargo check -p yinqidao-audio-spatial
cargo test -p yinqidao-audio-spatial
```

## 下一笔建议

优先把 `src/audio/dsp/mod.rs` 的原生 10/12 声道路径接到本 crate，让 AVS3 7.1.4 从 decoder 输出直接进入 12-source CPU renderer；随后迁移 stereo 360/8D presets。这样先删除当前最明显的空间信息损失点，不需要等待 GPU 或多线程。
