# YinQiDao audio codec crates

The player keeps Symphonia as the primary Rust decoder for formats it already handles well. Codecs or container features outside Symphonia are implemented as independent crates so they can be tested, fuzzed and optimized without coupling them to GPUI.

Current layout:

- `audio-codec-core`: decoder/frame/stream contracts shared by non-Symphonia codecs.
- `audio-codec-registry`: canonical routing/capability metadata. A codec appearing here does **not** mean its decoder is complete; maturity is explicit.
- `audio-simd`: runtime-dispatched kernels shared by codecs and DSP.
- `audio-codec-avs3`: pure-Rust AV3A/AVS3-P3 work. It now parses ISO-BMFF `av3a`/`dca3`, CA3 specific configuration, normative AATF synchronization/frame headers, GA codec routing, metadata boundaries and the fixed core-side transform selector, and exposes SIMD synthesis primitives.

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

Shared SIMD kernels currently cover gain/clamp, overlap-add/channel accumulation, vector/window multiplication and dot products. Architecture-specific `unsafe` stays inside `audio-simd`; codec crates remain mostly safe Rust.

## Non-Symphonia codec roadmap

The registry currently tracks AVS3-P3/AV3A/Audio Vivid, Monkey's Audio, WavPack, Opus, Musepack, AC-3, E-AC-3 and DTS Core. New decoder crates are created only when an actual parser/decoder milestone exists and is tested; the repository does not add empty crates merely to advertise a format.

## AVS3 milestone status

Implemented in pure Rust:

1. ISO-BMFF `av3a` AudioSampleEntry probing and raw `dca3` extraction.
2. `CA3SpecificBox` parsing for `audio_codec_id=1` (lossless) and `audio_codec_id=2` (general full-rate).
3. General full-rate configuration parsing for channel, object, mixed and HOA content.
4. AATF `0xFFF` syncword, codec id, ancillary flag, NN type, coding profile, sampling-frequency signalling, CRC fields, channel/object/HOA layout fields, resolution and bitrate indices.
5. 7.1.4 / 5.1.4 / 7.1.2 / FOA / HOA channel-index mapping from the normative Annex A table.
6. Validation that AATF coding method, sample rate, content profile, channel index and object counts agree with `dca3` where both are available.
7. General full-rate `codecFormat` routing without touching compressed spectral data: mono, stereo, multichannel and HOA are selected from AATF profile/layout fields; mixed bed+objects always enters the multichannel path.
8. Safe extraction of the raw coded block after the AATF header/CRC/alignment boundary.
9. Bit-accurate `Avs3MetadataDec()` boundary handling for the fixed flags. When both flags are zero, the first core-side field begins at bit offset 2; no byte-alignment assumption is made. If static/dynamic metadata is present the decoder stops before its variable payload rather than guessing a length.
10. Arbitrary-bit-position reading and the fixed 2-bit `transformType` prefix of `DecodeCoreSideBits()` with long, short, cut-in and cut-out window modes.

Still intentionally unsupported:

- `Avs3SmDec()` / `Avs3DmDec()` static and dynamic metadata payload bodies;
- the variable FdShaping/TNS/BWE portions of `DecodeCoreSideBits()`;
- `DecodeGroupBits()` / stereo / multichannel / HOA side information;
- range/entropy decoding and inverse quantization;
- inverse transform, TNS/BWE and post synthesis;
- stereo/multichannel reconstruction;
- object metadata rendering and HOA spatial decoding;
- `ll_raw_data_block()` lossless reconstruction;
- normative CRC verification.

The player must not report AVS3 as a complete decoder until the raw-data-block paths pass official/reference conformance and regression vectors. The temporary external AV3A backend can only be removed after that point.
