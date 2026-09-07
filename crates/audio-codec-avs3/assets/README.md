# AVS3 normative interoperability tables

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

`avs3a_mcr_rotations.bin` is the normative MCR angle-VQ interoperability table
preconverted to little-endian `(cos(theta), sin(theta))` `f32` pairs. The 512-entry
long/transition-window codebook comes first, followed by the 256-entry short-window
codebook; each entry has three angle dimensions. Keeping rotations rather than raw
angles removes per-frame trigonometry from the <=32-kb/s stereo hot path.

MCR rotation fingerprints:

- size: `18,432` bytes
- SHA-256: `9fe0ece1f78509f66847b9b31c60efed6a2185b5ee533e0b17e66cf3b5df61bc`
- FNV-1a 64: `5b62aa9a6b23145a`

The Rust tests verify byte geometry, fingerprints and representative rotation
values. MCR indexing and inverse-rotation control flow are implemented independently
in `mcr_synthesis.rs`; the asset is static interoperability data rather than imported
reference-decoder control flow.

`avs3a_hoa_spatial_tables.bin` contains the 1,343 fixed-angle HOA basis index pairs
followed by the 257-entry binary32 sine table used by spatial basis recovery. Angle
indices are serialized as little-endian signed 16-bit integers; sine values are
little-endian `f32`. The final three fixed-angle rows are explicit zero rows, matching
the implicit zero initialization in the declared 1,343-row reference table.

HOA spatial-table fingerprints:

- size: `6,400` bytes
- SHA-256: `641e93f65c86376815560119d6704064d33528ecddd331bab133f189164aec50`
- FNV-1a 64: `91a0296fd4def1af`

`hoa_synthesis.rs` uses the table read-only through `include_bytes!`. Sine/cosine
values are resolved by table lookup and quadrant mapping; the nine spherical-harmonic
normalization constants are fixed binary32 constants. No table decoding, allocation,
trigonometry or square-root operation occurs in the per-frame basis-recovery hot path.

These files contain interoperability data. For terms and notices applying to the
public AVS3 reference material used for cross-checking, consult its accompanying
UWA Code Sharing Policy documentation.
