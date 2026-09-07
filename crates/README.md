# YinQiDao audio codec crates

The player keeps Symphonia as the primary Rust decoder for formats it already handles well. Codecs or container features outside Symphonia are implemented as independent crates so they can be tested, fuzzed and optimized without coupling them to GPUI.

Current layout:

- `audio-codec-core`: decoder/frame/stream contracts shared by non-Symphonia codecs.
- `audio-codec-registry`: canonical routing/capability metadata. A codec appearing here does **not** mean its decoder is complete; maturity is explicit.
- `audio-simd`: runtime-dispatched kernels shared by codecs and DSP.
- `audio-codec-avs3`: pure-Rust AV3A/AVS3-P3 work, including ISO-BMFF `av3a`/`dca3` probing, bit reading, decoder state and SIMD synthesis primitives.

## CPU dispatch policy

Portable release binaries must not require `-C target-cpu=native`. Hot kernels select the fastest implementation that is legal for the running CPU.

| Target family | Baseline | Accelerated path |
| --- | --- | --- |
| Windows/Linux/macOS x86_64 | generic x86_64 | AVX2+FMA → AVX2 → SSE2 → scalar |
| Windows/Linux x86 | generic x86 | AVX2+FMA → AVX2 → SSE2 → scalar |
| Windows ARM64 | AArch64 | NEON |
| Linux ARM64 | AArch64 | NEON |
| Android ARM64 | AArch64 | NEON |
| iOS/iPadOS ARM64 | AArch64 | NEON |
| macOS Apple Silicon | AArch64 | NEON |
| Android ARMv7 / other Rust targets | portable scalar | scalar until a stable Rust 1.89-safe runtime NEON path is available |

Shared SIMD kernels currently cover:

- gain + PCM safety clamp;
- overlap/add and channel/object accumulation;
- transform/window vector multiply;
- dot products used by transform, prediction and filter-bank code.

The intent is to keep architecture-specific `unsafe` code inside `audio-simd`; codec crates remain mostly safe Rust and call stable slice-based kernels.

## Non-Symphonia codec roadmap

The registry currently tracks:

- AVS3-P3 / AV3A / Audio Vivid;
- Monkey's Audio / APE;
- WavPack;
- Opus;
- Musepack;
- AC-3;
- E-AC-3;
- DTS Core.

New decoder crates should be created only when an actual parser/decoder milestone is implemented and tested. Do not add empty crates merely to make the format list look complete.

AVS3 status: the crate currently implements the container/config boundary and low-level infrastructure. The normative AVS3-P3 entropy, transform and immersive reconstruction tools must be implemented from the specification and verified against conformance vectors before the player switches away from the transitional external AV3A backend. Do not report the crate as a complete AVS3 decoder until those tests pass.
