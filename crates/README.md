# YinQiDao audio codec crates

The player keeps Symphonia as the primary Rust decoder for formats it already handles well. Codecs or container features outside Symphonia are implemented as independent crates so they can be tested, fuzzed and optimized without coupling them to GPUI.

Current layout:

- `audio-codec-core`: decoder/frame/stream contracts shared by non-Symphonia codecs.
- `audio-simd`: runtime-dispatched audio kernels. x86/x86_64 selects AVX2, then SSE2, then scalar; AArch64 uses NEON; unsupported architectures remain on scalar code.
- `audio-codec-avs3`: pure-Rust AV3A/AVS3-P3 work, including ISO-BMFF `av3a`/`dca3` probing, bit reading, decoder state and SIMD synthesis primitives.

Target policy:

| Target family | Baseline | Accelerated path |
| --- | --- | --- |
| Windows/Linux x86_64 | generic x86_64 | AVX2 runtime dispatch, SSE2 fallback |
| Windows/Linux x86 | generic x86 | AVX2/SSE2 when detected |
| Windows/Linux ARM64 | AArch64 | NEON |
| Android ARM64 | AArch64 | NEON |
| iOS ARM64 | AArch64 | NEON |
| Other Rust targets | portable scalar | scalar |

Release builds must not require `-C target-cpu=native`; portable binaries choose the fastest safe backend at runtime.

AVS3 status: the crate currently implements the container/config boundary and low-level infrastructure. The normative AVS3-P3 entropy, transform and immersive reconstruction tools must be implemented from the specification and verified against conformance vectors before the player switches away from the transitional external AV3A backend. Do not report the crate as a complete AVS3 decoder until those tests pass.
