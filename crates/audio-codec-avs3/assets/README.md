# AVS3 normative frequency-domain tables

`avs3_fd_tables_0.bin` through `avs3_fd_tables_8.bin` are contiguous chunks of one
43,968-byte little-endian asset containing 10,992 binary32 values used by AVS3
frequency-domain LSF inverse quantization.

Concatenate the files in numeric order to recover the canonical asset. The first
eight chunks are 5,400 bytes each and the final chunk is 768 bytes. The split is
repository-transport only; `fd_lsf_tables.rs` reconstructs the native `f32` table
at compile time, so frame decoding performs no chunk dispatch, byte conversion or
allocation.

Table order is:

1. `mean_lsf` (16 values)
2. HBR stage-1 CB1 (256 x 9)
3. HBR stage-1 CB2 (256 x 7)
4. HBR stage-2 CB1 (128 x 3)
5. HBR stage-2 CB2 (128 x 3)
6. HBR stage-2 CB3 (64 x 3)
7. HBR stage-2 CB4 (32 x 3)
8. HBR stage-2 CB5 (32 x 4)
9. LBR stage-1 CB1 (256 x 9)
10. LBR stage-1 CB2 (256 x 7)
11. LBR stage-2 CB1 (128 x 5)
12. LBR stage-2 CB2 (128 x 4)
13. LBR stage-2 CB3 (64 x 7)

The numerical data corresponds to the GY/T 363-2023 Annex-B LSF tables and was
cross-checked against the publicly available AVS3-P3 reference-derived asset in
`1254qwer/avs3a-rust` (`assets/avs3a_fd_tables.bin`, Git blob
`d5bc9a84a46a0d57888ac1d22b54d120e357809f`).

Canonical concatenated fingerprints:

- size: `43,968` bytes
- SHA-256: `6b8e25a332edf722c81c494c85ab57d90f145d1524fd808e01333e1c9a6d39d5`
- FNV-1a 64: `9ce264f019b75cc4`

The Rust tests verify byte geometry, FNV-1a, finite values, all split-VQ table
lengths, and bit-for-bit equality between the embedded mean vector and the
existing Annex-B.46 `LSF_MEAN` constants.

These files contain interoperability data rather than imported reference-decoder
control flow. For terms and notices applying to the public reference material,
consult its accompanying UWA Code Sharing Policy documentation.
