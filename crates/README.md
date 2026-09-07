# YinQiDao audio codec crates

The player keeps Symphonia as the primary Rust decoder for formats it already handles well. Codecs or container features outside Symphonia are implemented as independent pure-Rust crates so they can be tested, fuzzed and optimized without coupling them to GPUI.

Current layout:

- `audio-codec-core`: decoder/frame/stream contracts shared by non-Symphonia codecs.
- `audio-codec-registry`: canonical routing/capability metadata. A codec appearing here does **not** mean its decoder is complete; maturity is explicit.
- `audio-simd`: runtime-dispatched kernels shared by codecs and DSP.
- `audio-codec-avs3`: pure-Rust AV3A/AVS3-P3 work from ISO-BMFF probing through AVS3 frame/QC parsing and range-decoder infrastructure.

## CPU dispatch policy

Portable release binaries must not require `-C target-cpu=native`. Hot kernels select the fastest implementation legal for the running CPU.

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

Shared SIMD kernels cover gain/clamp, overlap-add/channel accumulation, vector/window multiplication and dot products. Architecture-specific `unsafe` stays inside `audio-simd`; codec crates remain mostly safe Rust.

## Non-Symphonia codec roadmap

The registry tracks AVS3-P3/AV3A/Audio Vivid, Monkey's Audio, WavPack, Opus, Musepack, AC-3, E-AC-3 and DTS Core. New decoder crates are created only when an actual parser/decoder milestone exists and is tested; the repository does not add empty crates merely to advertise a format.

## AVS3 milestone status

Implemented in pure Rust:

1. ISO-BMFF `av3a` AudioSampleEntry probing and raw `dca3` extraction.
2. `CA3SpecificBox` parsing for lossless and general-full-rate AVS3-P3.
3. AATF synchronization/header parsing and cross-validation against `dca3`.
4. Channel/object/mixed/HOA profile routing and normative channel-layout resolution including 7.1.4.
5. Bit-accurate metadata boundaries plus complete dynamic Audio Vivid L1/L2 object metadata parsing.
6. Core side information: transform/window type, FD shaping VQ, complete two-filter TNS Huffman decode and reflection-coefficient lookup.
7. BWE enable/configuration tables and BWE side-information parsing for mono/stereo/multichannel modes.
8. Spectrum GroupBits, multichannel pair/ILD/silence/ratio side information and frame-major channel ordering.
9. Multichannel bit allocation through the published safe-channel/LFE/Q6-ratio steps, with byte-conservation checks and without inventing the unpublished final per-channel cap.
10. Complete `DecodeQcBits()` parsing into zero-copy context/base bit ranges for Basic and Low-Complexity NN modes.
11. A 32-bit AVS3 range-decoder engine with 16-bit renormalization, 16-bit CDF precision, 4-bit signed overflow extension and zero-extension of omitted trailing bytes.
12. Zero-copy range-byte windows that consume byte-counted QC payloads starting at arbitrary packet bit offsets.
13. Normative Basic/Low-Complexity feature-scale and noise-filling parameter dequantization helpers.
14. Cross-platform SIMD synthesis primitives in `audio-simd` for overlap-add, transform windows and filter/prediction dot products.

Still intentionally unsupported for production playback:

- full Basic static metadata (`BasicL1()` / VR extension) bodies;
- normative B.1/B.8/B.9 probability/standard-deviation tables wired into context/base latent range decoding;
- context/base decoding neural-network weights and inverse transforms;
- complete inverse quantization/noise-filling reconstruction into MDCT spectra;
- stereo inverse M/S and multichannel MCAC reconstruction;
- post synthesis: inverse TNS/BWE/FD shaping, degrouping and IMDCT;
- HOA side information, HOA byte splitting and spatial reconstruction;
- `ll_raw_data_block()` lossless reconstruction;
- normative CRC verification.

The range engine is deliberately model-agnostic. CDF/model data must come from the normative AVS3 tables or properly licensed project-owned data; code from public mirrors without an explicit compatible license is not copied into this repository.

AVS3 remains `InDevelopment` in the codec registry until real AV3A samples and official/reference conformance vectors pass end-to-end PCM regression tests. The transitional external AV3A backend must not be removed before that point.
