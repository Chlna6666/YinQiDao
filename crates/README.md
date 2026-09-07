# YinQiDao audio codec crates

The player keeps Symphonia as the primary Rust decoder for formats it already handles well. Codecs or container features outside Symphonia are implemented as independent crates so they can be tested, fuzzed and optimized without coupling them to GPUI.

Current layout:

- `audio-codec-core`: decoder/frame/stream contracts shared by non-Symphonia codecs.
- `audio-codec-registry`: canonical routing/capability metadata. A codec appearing here does **not** mean its decoder is complete; maturity is explicit.
- `audio-simd`: runtime-dispatched kernels shared by codecs and DSP.
- `audio-codec-avs3`: pure-Rust AV3A/AVS3-P3 work. It now parses ISO-BMFF `av3a`/`dca3`, CA3 specific configuration, normative AATF synchronization/frame headers, and exposes SIMD synthesis primitives.

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

## AVS3 milestone status

Implemented in pure Rust:

1. ISO-BMFF `av3a` AudioSampleEntry probing and raw `dca3` extraction.
2. `CA3SpecificBox` parsing for `audio_codec_id=1` (lossless) and `audio_codec_id=2` (general full-rate).
3. General full-rate configuration parsing for channel, object, mixed and HOA content.
4. AATF `0xFFF` syncword, codec id, ancillary flag, NN type, coding profile, sampling-frequency signalling, CRC fields, channel/object/HOA layout fields, resolution and bitrate indices.
5. 7.1.4 / 5.1.4 / 7.1.2 / FOA / HOA channel-index mapping from the normative Annex A table.
6. Validation that the AATF coding method and known sample rate agree with `dca3` before entering the codec payload.

Still intentionally unsupported:

- `ga_co_raw_data_block()` metadata/core side-bit demultiplexing;
- range/entropy decoding and inverse quantization;
- inverse transform, TNS/BWE and post synthesis;
- stereo/multichannel reconstruction;
- object metadata rendering and HOA spatial decoding;
- `ll_raw_data_block()` lossless reconstruction;
- normative CRC verification.

The player must not report AVS3 as a complete decoder until the raw-data-block paths pass official/reference conformance and regression vectors. The temporary external AV3A backend can only be removed after that point.
