# YinQiDao Spatial Engine — 实现状态

更新时间：2026-09-10

## 核心目标

`yinqidao-audio-spatial` 是 YinQiDao 自研 CPU-first 参数化空间音频核心。当前不依赖 SOFA、KEMAR、外部 HRTF 数据库或第三方空间音频 runtime；定位算法必须描述为**参数化双耳 / 虚拟声源定位**，不得宣称 measured HRTF。

实时路径长期硬约束：

- audio block 内 0 heap allocation / 0 Vec growth；
- 0 Mutex / RwLock / blocking channel / file I/O / system call；
- 不在 audio block 内 spawn thread、构建 FFT plan 或创建运行时资源；
- interleaved PCM 通过 borrowed slice + stride 直接消费；
- 多声道优先使用**可靠 codec/container speaker metadata + 与实际 PCM channel count 的精确匹配**进入 native spatial renderer；metadata 完全缺失时只允许用户显式选择且声道数精确匹配的 Source Layout Override，禁止仅按 6/8/10/12 声道数自动猜布局；
- stereo L/R 保持两个独立 source，不 collapse 为 mono 再做空间处理；
- native authored/declared speaker bed 不先 downmix 到 Stereo 再做 Scene Motion；
- LFE 保持非方向性，不参与伪方位旋转或 image-source 定位；
- CPU 优先复用 `yinqidao-audio-simd` 的 Scalar/SSE2/AVX2/AVX2+FMA/NEON dispatch；
- realtime worker pool 只有 benchmark 证明收益后才能启用；音频 GPU compute 暂缓。

## Phase 0 — CPU realtime foundation

- [x] 独立 crate：`crates/audio-spatial`。
- [x] 默认 64-frame 固定内部 block，可配置。
- [x] `SpatialEngine` / source workspace 初始化时预分配。
- [x] `Vec3` / `SourcePose` / `ListenerPose` / `RoomPose` 无分配几何 primitive。
- [x] `ListenerPose::basis()` 作为公开只读几何 API，DSP 与应用侧 GPU Debug 共用同一 listener-local basis。
- [x] `DEFAULT_HEAD_RADIUS_M = 0.0875` + `ListenerPose::ear_positions()` 作为 DSP / Debug 共用的唯一默认双耳几何基准；耳位几何保持头中心左右对称，per-ear Pinna/增益/延迟允许不对称。
- [x] `RoomPose` 提供独立 world-space 房间 position/forward/up 与 `world_to_local/local_to_world` 变换；房间坐标系不再必须绑定 Listener。
- [x] Stereo / 5.1 / 7.1 / 5.1.2 / 5.1.4 / 7.1.2 / 7.1.4 speaker scene。
- [x] `ChannelRole` + `SpeakerLayout::roles()/role()` 显式描述 authored PCM 槽位语义。
- [x] 5.1.2 contract：`FL, FR, C, LFE, SL, SR, TFL, TFR`。
- [x] 7.1.2 contract：`FL, FR, C, LFE, RL, RR, SL, SR, TFL, TFR`。
- [x] 7.1.4 contract：`FL, FR, C, LFE, RL, RR, SL, SR, TFL, TFR, TRL, TRR`，LFE index 3。
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
- [x] 动态 trajectory 生成真实 m/s velocity。
- [x] 高速 trajectory 按角速度自适应细分，单 segment 角位移约 `<= 1°`；常规 48 kHz / 0.5 Hz 仍保持 64-frame block。
- [x] rigid stereo pair 的 L/R position 与 velocity 使用一致刚体变换。
- [x] native speaker bed Scene Motion 以完整声床刚体球面变换实现；FullRange 各声道保持相对几何和 PCM 槽位，不复制/重排为中间 per-channel buffer。
- [x] Helix trajectory 使用 signed elevation，轨迹可穿过 listener 上下半球；这是直接声源几何，不是虚构 7.1.4 floor speaker。
- [x] source accumulation 复用 `yinqidao-audio-simd::mix_accumulate`。
- [x] NaN/Inf 在进入 delay/IIR/pinna persistent state 前隔离。

## Phase 1 — 播放器正式接入

- [x] 根播放器依赖 `yinqidao-audio-spatial`。
- [x] `DecoderStream` 从 AVS3 `dca3` 保存显式 `ChannelConfiguration → ChannelLayout` hint。
- [x] AVS3 handoff 已覆盖 5.1 / 7.1 / 5.1.2 / 5.1.4 / 7.1.2 / 7.1.4；8ch 与 10ch 歧义由 metadata 语义消除，而不是靠 channel count 猜测。
- [x] DSP native path 同时校验 layout hint 与实际 PCM channel count；可靠 hint mismatch 时拒绝 native，不尝试用其他布局“纠正”。
- [x] 当前曲播放与 crossfade/preloaded first chunk 都从其同一个 `DecoderStream` 读取 layout hint 并传入 `AudioProcessor::process_into_with_layout`。
- [x] metadata 完全缺失的 discrete multichannel 默认保持保守 layout-neutral fold；`Auto` 不把 8ch/10ch/12ch 自动解释成某种 speaker bed。
- [x] Source Layout Override：metadata 缺失时，用户显式选择 5.1 / 7.1 / 5.1.2 / 5.1.4 / 7.1.2 / 7.1.4 且其 speaker count 与实际 PCM 精确一致，可让原始 N-channel PCM 直接进入 Native Speaker Bed。
- [x] Reliable metadata 始终优先于 Source Layout Override；存在 metadata 但与 PCM count 冲突时不允许 override 覆盖它。
- [x] 显式布局与 PCM count 不匹配时不冒充 native bed；保留 layout-neutral fold，并且只有用户明确配置 Virtual Bed 时才允许重新建立虚拟 speaker bed。
- [x] verified/overridden native 5.1 / 7.1 / 5.1.2 / 5.1.4 / 7.1.2 / 7.1.4 在输入采样率下先 native binaural render，再做 streaming stereo resample。
- [x] native multichannel 旁路旧 `binaural_downmix_into()`，禁止再次进入 stereo spatial motion。
- [x] native engine 按 input sample-rate 缓存。
- [x] native full-range speaker bed 与 Stereo Virtual Bed 共用 `SpatialSettings → Scene Motion` audio-clock；动态模式对整张声床做一致球面变换，LFE 保持方向无关。
- [x] stereo/native 共用单一 `SpatialSettings → EnvironmentSettings` 映射；关闭 Spatial 时 native 仍保留 direct binaural/pinna，但 synthetic Early/FDN `mix=0`。
- [x] native engine 缓存最后一次 environment，参数变化才更新六面反射/FDN。
- [x] seek / reopen / processing discontinuity reset EQ、resampler、native/stereo spatial、pinna、late-field、trajectory 与 peak-limiter envelope。
- [x] transport generation 使用 audio-worker thread-local。
- [x] Stereo Static / Immersive 使用独立 L/R 球形 virtual source；Width/Depth/Envelopment 决定方位、上下 elevation 与 externalization。
- [x] Orbit8d / Orbit360 / Pendulum / FrontBack / Planetary / NearEar / Helix 进入自研 `Trajectory`。
- [x] dynamic stereo 以 rigid stereo pair 运动，保持 authored L/R。
- [x] Stereo Virtual Bed 支持 Auto / Off / 5.1 / 7.1 / 5.1.2 / 5.1.4 / 7.1.2 / 7.1.4；动态场景 Auto 可进入完整 7.1.4 virtual bed。
- [x] crossfeed 映射为虚拟扬声器 arc 收窄，不提前混合 PCM。
- [x] 专业用户预设已扩展为 HiFi Direct / Studio / Wide / Headphone / Concert Hall / Live Concert / Cinema / Immersive / 8D / 360° / Pendulum / FrontBack / Planetary / Near Ear / Helix。
- [x] HiFi Direct 是低处理参考模式：关闭 synthetic spatial/room/motion/virtual-bed，但**不宣称 bit-perfect**；decoder、SRC、limiter 与系统输出链仍可能改变样本。
- [x] UI 已将误导性的 `3D 沉浸` / `3D Decorrelation` 分别改为 `Immersive 全景声场` / `Envelopment 空间包围度`。
- [ ] 将当前复用同一 `VirtualBedMode` selector 的 Source Layout Override 拆成独立持久化字段和独立 UI selector，避免两种语义长期耦合。
- [ ] 删除 legacy `src/audio/dsp/spatial.rs` renderer；当前仍作为 failure fallback 与 settings/preset holder。

## Phase 2 — CPU kernel / 声学质量

- [x] block 起点一次生成参数 step，sample loop 只做加法推进。
- [x] reflection step 显式固定为 `[RenderParameterStep; EARLY_REFLECTION_TAP_COUNT]`，避免 `std::array::from_fn` const generic 推断歧义。
- [x] source pose/listener 参数 cache；固定 native speaker pose 避免重复 solve。
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
- [x] notch/shoulder 系数按 block 端点求值并 sample-ramp；静态 speaker pose 命中 cache。
- [x] 每耳两级 TDF2 biquad 固定状态、0 allocation；异常样本清状态并输出 0，denormal 主动归零。
- [x] linked-stereo zero-lookahead peak safety limiter：L/R 共用 gain envelope、instantaneous attack、约 90 ms release、默认 ceiling `-0.30 dBFS`、0 added latency / 0 allocation。
- [x] 用户音量 gain 进入 limiter detector/application；最终 SIMD hard clamp 只保留为异常 invariant guard。
- [x] channel-order conformance vectors 已覆盖 5.1 / 7.1 / 5.1.2 / 5.1.4 / 7.1.2 / 7.1.4 的 `ChannelRole` 语义、LFE index 3、rear/side/top 几何类别与稳定短名。
- [x] codec→spatial handoff contract tests 已覆盖显式 layout、layout/channel-count mismatch、8ch/10ch 歧义、metadata-less Source Layout Override 与可靠 metadata 优先级。
- [x] native 5.1/7.1/.2/.4 与 Stereo Virtual Bed Scene Motion 的 sample-clock regression tests 已写入源码。
- [ ] reflection arrival 是否加入 pinna spectral cue：先以 benchmark/听感证明收益，避免 `6 taps × N sources` 无依据增负载。
- [ ] 更多 hot kernel 下沉 `audio-simd`。
- [ ] Lagrange vs Thiran fractional delay 质量/成本对比。
- [ ] 更完整的多段 pinna bank / 参数标定。
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

- [x] world-fixed 默认 rectangular image-source room；所有 source 共享同一房间几何，Listener 转头/移动不再旋转或平移默认墙面。
- [x] `RoomPose` + 显式 arbitrary-room image-source solver：source/listener 在 room-local 求解，image/bounce 再回到 world-space。
- [x] 正式播放路径使用 per-source first-order image-source reflections。
- [x] Left / Right / Front / Rear / Floor / Ceiling 六面一阶反射。
- [x] 每条反射按 image arrival direction 进入同一套 binaural ITD/ILD/head-shadow/distance/air 模型。
- [x] reflection 只增加 excess path delay，不给整个音乐节目加入绝对传播延迟。
- [x] 极端越界 source/listener 只在 reflection solver 内夹到房间边界内；direct source/listener pose 不修改。
- [x] LFE 不做伪方向化 image-source reflection。
- [x] 已删除退出正式链的旧全局 `EarlyReflectionNetwork` API/实现。
- [x] direct path 参数化 pinna / externalization spectral layer；明确为 generic parametric cue，不宣称 measured HRTF。
- [x] 全局 8-line FDN late diffuse field：8 条预分配 delay、正交 Hadamard feedback、per-line damping、固定容量。
- [x] FDN 只在所有 source 汇合后的最终 stereo block 运行一次；native layout 先 normalization 再激励尾场。
- [x] FDN 使用独立 L/R 正交 injection vectors，保留纯 Side/反相 stereo 的 late-field 激励。
- [x] `environment.mix=0` 为 late-field bit-exact bypass；room-size 改变/reset 清理旧 tail history。
- [x] native 5.1 / 7.1 / 5.1.2 / 5.1.4 / 7.1.2 / 7.1.4 在 Spatial 启用时使用同一六面 Early + FDN room；静态模式保持 authored geometry，动态模式仅对 FullRange speaker bed 施加统一刚体球面 Scene Motion，不经过 Stereo 二次运动。
- [x] Concert Hall / Live Concert / Cinema / Immersive 已作为不同参数化场景公开；这些是算法场景预设，不描述为真实场馆 measured IR。
- [x] Helix 通过 signed elevation 提供上方/下方直接声像运动；Floor/Ceiling 仍分别属于真实 room reflection surface，不将“下方声像”错误描述为 floor speaker。
- [ ] `SpatialEngine` runtime custom `RoomPose` setter + Debug room pose publication；当前正式默认 room 已 world-fixed，但任意运行时房间 transform 尚未贯通 renderer/cache/debug schema。
- [ ] Audio Vivid object metadata 接入。
- [ ] HOA 参数化 binaural path。
- [ ] object source culling / audibility budget。
- [ ] listener pose / head orientation 从播放器命令/UI 到 `AudioProcessor → stereo/native SpatialEngine` 的 runtime 接口；底层 `SpatialEngine::set_listener` 已存在，但尚未向上贯通。
- [ ] source directivity / spread 高级模型。

## Phase 5 — Spatial Debug / 发烧级可视化

**硬约束：GPU 3D 是 Audio Laboratory 的新增能力，不允许替代或删除原有工程分析视图。**

- [x] allocation-free 固定尺寸 `SpatialDebugSnapshot`：listener、最多 32 source。
- [x] reflection Debug：`12 source × 6 tap = 72` 固定矩阵，包含 image/bounce/full+excess path/arrival/LR delay+gain。
- [x] Debug 默认关闭；开启时复用固定数组。
- [x] 根播放器原子槽 + odd/even seqlock 发布，无 Mutex/channel/heap publication。
- [x] scene publish 约 30 Hz。
- [x] 每 virtual/authored source 的实际 PCM Peak/RMS 仅在 Debug-enabled 路径计算；固定 `SourceActivity[32]` 复用，无 render-time Vec；结果直接绑定进 `SpatialDebugSource.input_peak/input_rms` 并随同一 scene seqlock 发布。
- [x] GPU speaker glyph 大小/亮度、direct binaural path、reflection 可见度与 Spatial Telemetry 的 dBFS inspector 均消费实时 Peak/RMS；静音声道不再以固定强度伪装为活跃。
- [x] 完整 Audio Laboratory 工程分析恢复并独立放在 `src/ui/audio_debug_analysis.rs`：SOURCE / POST-EQ / POST-SPATIAL、Peak/True Peak/RMS/Crest/DR/Clip、LUFS-M/S/I、Correlation、S/M。
- [x] A/B/C Spectrum、Transfer ΔdB、M/S Spectrum、Phase History、Crest/Dynamic History、Spectrogram、Waveform Overlay、Stereo Vectorscope 全部保留。
- [x] GPU 3D Debug 使用 BMCBL GPUI `GpuMesh3d` / WGSL / depth；不修改 GPUI core。
- [x] Debug UI 位于 `src/ui/audio_debug_window.rs`、`audio_debug_analysis.rs`、`audio_spatial_debug_3d.rs`、`audio_spatial_debug_3d.wgsl`；不再保留 V2 命名。
- [x] GPUI stage accent `Rgba → Hsla` 显式转换已修复。
- [x] GPU Debug 直接调用公开 `ListenerPose::basis()` 与 `ListenerPose::ear_positions()`；左右耳 marker 与 DSP 的默认 ±head-radius 几何保持同一真值来源。
- [x] Listener 不再只画中心点：3D 中显示低模 humanoid/head shell、明确左耳（蓝）/右耳（红）、面向方向与 interaural axis。
- [x] Stereo / 5.1 / 7.1 / 5.1.2 / 5.1.4 / 7.1.2 / 7.1.4 authored/virtual source 在 snapshot 中保持独立 source，GPU 3D 以独立 virtual-speaker glyph 显示，而不是合并成一个点。
- [x] 每个 active source 同时绘制 `source → left ear` 与 `source → right ear` 两条 direct binaural path；线宽/透明度由实际 `left_gain/right_gain` 驱动，直观看到 off-center source 的双耳不对称响应。
- [x] 六面 Early reflection 由 `source → bounce` 后再分别进入 left/right ear；不再把 reflection 终点画成单一 Listener 中心。
- [x] Floor/Ceiling 使用与 DSP 相同的 room height 与真实 bounce path。
- [x] mesh id 稳定，以 generation 刷新 GPU cache；UI 约 30 Hz 更新。
- [x] 3D Camera 主交互为**左键拖拽 orbit + 滚轮 zoom + 双击 reset**，并新增显式**“重置视角”**按钮；滚轮在 3D 区阻止向父级滚动传播。
- [x] Pinna telemetry 复用 realtime cue generator。
- [x] FDN telemetry 复用 realtime parameter derivation。
- [x] GPU 3D 明确区分 Early/Late：离散折线路径代表六面 image-source Early，监听者周围半透明 volume 代表 FDN Late。
- [x] Debug channel label 直接消费公开 `ChannelRole` / `SpeakerLayout::role()`，不维护重复 channel-role 名称数组。
- [x] 已建立指定 CC0 `Humanoid Low Poly Mesh (With Basic Face)` 的离线导入链：`tools/audio_debug/export_humanoid_cc0.py` 使用 Blender 将 `.blend` 一次性烘焙成 `src/ui/audio_debug_humanoid_generated.rs` 的静态 `GpuMesh3d` 顶点/索引；运行时无需 Blender、OBJ/GLTF parser 或文件 I/O。
- [ ] 当前仓库的 `audio_debug_humanoid_generated.rs` 仍是 `HUMANOID_ASSET_READY=false` 的 build-safe placeholder；需要本地取得 `HumanoidBaseMesh_new.blend` 后运行 exporter，才能把**指定 CC0 真模型**烘焙进仓库。此项完成前不得宣称实际渲染的就是原始 CC0 mesh。
- [ ] GPU room wireframe / Floor / Ceiling grid 还需要从固定 world room 投影到 listener-view；DSP reflection 已 world-fixed，但在 runtime Listener orientation 向上层开放前必须先完成这项，避免声音墙面与 Debug 房间框语义不一致。
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

已加入 64-frame 对照：

- dry baseline：stereo / Orbit360 / 5.1.4 / 7.1.4；
- `environment.mix=0.10`：stereo / Orbit360 / 7.1.4；
- `environment.mix=0.30`：stereo / 7.1.4；
- Debug off/on：stereo 与 7.1.4；
- 每个 case 输出 avg/p50/p95/p99/worst 与 audio deadline budget；
- direct 双级 pinna 属于 FullRange baseline，room case 增加六面 Early + 全局 FDN。

仍需后续增加：

- 5.1 / 7.1 / 5.1.2 / 7.1.2 native bed 与 Helix Scene Motion benchmark case；
- 可切 pinna off/on 的精确边际成本；
- limiter off/on 边际成本与 gain-reduction 统计；
- reflection-pinna prototype cost；
- 16/32/64 future objects；
- AVS3 native multichannel end-to-end。

## 2026-09-09 本地编译反馈与修复

用户本地构建已实际暴露以下源码/API 问题；对应修复已提交，但**尚未收到修复后的完整 `cargo check` 成功结果**：

1. `renderer.rs`：`std::array::from_fn` 无法推断 `reflection_steps` const generic 长度（E0284）
   - 已显式标注 `[RenderParameterStep; EARLY_REFLECTION_TAP_COUNT]`。
2. Debug stage card：`stage_card` 需要 `Hsla`，`rgb(...)` 返回 `Rgba`（E0308）
   - 已显式 `.into()`。
3. GPU Debug：应用 crate 无法调用 `pub(crate) ListenerPose::basis()`（E0624）
   - 已把 `basis()` 提升为公开只读几何 API。
4. Debug 文件结构
   - 已统一收拢到 `src/ui/`，并恢复完整 Audio Laboratory 分析模块。

## 2026-09-10 专业场景 / Scene Motion / Source Layout Override

- [x] Native Speaker Bed Scene Motion：完整 FullRange speaker bed 由单一 audio-clock trajectory 驱动，保持声道相对几何，不先 fold 到 Stereo。
- [x] Native layout 扩展到 5.1 / 7.1 / 5.1.2 / 5.1.4 / 7.1.2 / 7.1.4，并保持 metadata + channel-count 的显式 contract。
- [x] Helix 正式接入 `SpatialMotionMode → TrajectoryKind`，提供上下半球 direct-source 运动；LFE 仍不方向化。
- [x] metadata-less/discrete multichannel 支持 exact-match Source Layout Override，可靠 metadata 永远优先。
- [x] Stereo/Mono Virtual Bed 与 native bed 共用球形 binaural renderer；Auto/显式布局均不改变 native authored metadata。
- [x] 新增 HiFi Direct / Concert Hall / Live Concert / Helix Sphere 专业预设。
- [x] 设置界面已暴露上述预设，并将 `3D 沉浸` 改名为 `Immersive 全景声场`、`3D Decorrelation` 改名为 `Envelopment 空间包围度`。
- [x] 设置界面明确说明“下方直接声像”来自 signed-elevation trajectory，而不是 7.1.4 floor speaker。
- [ ] 当前 Source Layout Override 暂时复用 `VirtualBedMode` 的显式 layout 值；下一阶段应拆成独立设置，避免 Stereo Virtual Bed 与源布局声明共享一个状态。

## 当前验证状态

- **本助手环境尚未执行 `cargo check`**；
- **本助手环境尚未执行 `cargo test`**；
- **尚未执行 `cpu_bench`**；
- 用户本地编译已推进到并反馈 2026-09-09 一轮编译错误，但最新主线是否完整通过仍待下一次本地构建确认；
- 新增 channel-order / codec→spatial handoff / Source Layout Override / Scene Motion / Helix / ear-position / source-activity / RoomPose / world-room image-source tests 已写入源码，但尚未执行；
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

1. 把 Source Layout Override 从 `VirtualBedMode` 拆成独立持久化设置与独立 selector；旧配置通过 serde default 安全迁移，避免 Stereo Virtual Bed 选择意外声明 metadata-less multichannel 布局。
2. 用户本地重新执行 `cargo check`，继续消除剩余 GPUI 3D / pinna / limiter / native-room / handoff type/API 问题，直到根包完整通过。
3. 让 GPU room wireframe / Floor / Ceiling grid 使用固定 world room → listener-view 投影；完成前不向播放器/UI 开放 runtime Listener orientation。
4. 在 `AudioProcessor → StereoSpatializer/native SpatialEngine` 贯通 Listener pose，再接播放器命令/UI/head-tracking 输入；验证转头只改变 binaural cues，不旋转 world room。
5. 本地获取 `HumanoidBaseMesh_new.blend` 后运行 `tools/audio_debug/export_humanoid_cc0.py`，把指定 CC0 真模型烘焙进 `audio_debug_humanoid_generated.rs`，并核对模型朝向/头中心与 DSP ear markers 对齐。
6. 实际跑 serial benchmark，补齐 5.1/7.1/.2/.4 与 Helix case，并分别测 dry / 双级 pinna / 六面 Early / FDN / limiter / Debug 的边际成本。
7. 用真实音乐与峰值测试素材标定 HiFi Direct 的低处理链、Concert Hall/Live Concert 场景参数、limiter ceiling/release、post-spatial headroom 与 true-peak 风险。
8. 只有 benchmark + 听感同时证明收益时才尝试 reflection-pinna / tap audibility budget / realtime worker pool。
9. 编译与性能稳定后删除 legacy `src/audio/dsp/spatial.rs` renderer/fallback。
10. 再推进 runtime custom `RoomPose`、source directivity、Audio Vivid object metadata 与 object/HOA 路径。
