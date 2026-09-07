use yinqidao_codec_core::CodecError;

use crate::{BweConfig, BweSideInfo, WhiteningLevel};

const MDCT_LINES: usize = 1024;
const MID_WHITEN_RADIUS: usize = 7;
const ENVELOPE_Q_STEP: f32 = 4.24966;
const ENVELOPE_Q_OFFSET: f32 = 4.0;

/// Decoder-local pseudo-random source for HIGH BWE whitening.
///
/// The AVS3 specification requires random noise but does not fix a portable PRNG. Keeping the
/// state explicit avoids global `rand()` state, locks and cross-stream coupling.
#[derive(Clone, Copy, Debug)]
pub struct BweWhiteningRng {
    state: u64,
}

impl BweWhiteningRng {
    pub fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed },
        }
    }

    #[inline]
    fn next_u64(&mut self) -> u64 {
        let mut value = self.state;
        value ^= value >> 12;
        value ^= value << 25;
        value ^= value >> 27;
        self.state = value;
        value.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    #[inline]
    pub fn next_symmetric_f32(&mut self) -> f32 {
        let unit = ((self.next_u64() >> 40) as u32) as f32 * (1.0 / 16_777_215.0);
        unit.mul_add(2.0, -1.0)
    }
}

impl Default for BweWhiteningRng {
    fn default() -> Self {
        Self::new(0x4156_5333_4257_4552)
    }
}

/// Reusable buffers for AVS3 bandwidth-extension reconstruction.
#[derive(Debug)]
pub struct BweSynthesisWorkspace {
    replicated: [f32; MDCT_LINES],
    whitened: [f32; MDCT_LINES],
    rng: BweWhiteningRng,
}

impl BweSynthesisWorkspace {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_seed(seed: u64) -> Self {
        Self {
            replicated: [0.0; MDCT_LINES],
            whitened: [0.0; MDCT_LINES],
            rng: BweWhiteningRng::new(seed),
        }
    }
}

impl Default for BweSynthesisWorkspace {
    fn default() -> Self {
        Self {
            replicated: [0.0; MDCT_LINES],
            whitened: [0.0; MDCT_LINES],
            rng: BweWhiteningRng::default(),
        }
    }
}

fn boundary(value: Option<u16>, message: &'static str) -> Result<usize, CodecError> {
    value.map(usize::from).ok_or(CodecError::InvalidData(message))
}

fn validate_layout(config: BweConfig, side: BweSideInfo) -> Result<(usize, usize), CodecError> {
    let num_sfb = usize::from(config.num_sfb);
    let num_tiles = usize::from(config.num_tiles);
    if num_sfb == 0 || num_sfb > side.envelope_indices.len() {
        return Err(CodecError::InvalidData("BWE SFB count is outside supported tables"));
    }
    if num_tiles == 0 || num_tiles > side.whitening_levels.len() {
        return Err(CodecError::InvalidData("BWE tile count is outside supported tables"));
    }

    for index in 0..num_sfb {
        if side.envelope_indices[index].is_none() {
            return Err(CodecError::InvalidData("BWE SFB is missing its envelope index"));
        }
        let start = boundary(config.sfb_boundaries[index], "BWE SFB start is missing")?;
        let end = boundary(config.sfb_boundaries[index + 1], "BWE SFB end is missing")?;
        if start >= end || end > MDCT_LINES {
            return Err(CodecError::InvalidData("BWE SFB boundaries are invalid"));
        }
    }

    for index in 0..num_tiles {
        if side.whitening_levels[index].is_none() {
            return Err(CodecError::InvalidData("BWE tile is missing its whitening level"));
        }
        let start = boundary(config.target_tiles[index], "BWE target tile start is missing")?;
        let end = boundary(config.target_tiles[index + 1], "BWE target tile end is missing")?;
        let source = boundary(config.source_tiles[index], "BWE source tile start is missing")?;
        if start >= end || end > MDCT_LINES {
            return Err(CodecError::InvalidData("BWE target tile boundaries are invalid"));
        }
        let width = end - start;
        if source.checked_add(width).is_none_or(|source_end| source_end > MDCT_LINES) {
            return Err(CodecError::InvalidData("BWE source tile exceeds MDCT geometry"));
        }
        if matches!(side.whitening_levels[index], Some(WhiteningLevel::Mid))
            && (start < MID_WHITEN_RADIUS || end + MID_WHITEN_RADIUS > MDCT_LINES)
        {
            return Err(CodecError::InvalidData("BWE MID whitening neighborhood exceeds MDCT geometry"));
        }
    }

    let start = boundary(config.target_tiles[0], "BWE start line is missing")?;
    let stop = boundary(config.target_tiles[num_tiles], "BWE stop line is missing")?;
    let sfb_start = boundary(config.sfb_boundaries[0], "BWE first SFB boundary is missing")?;
    let sfb_stop = boundary(config.sfb_boundaries[num_sfb], "BWE final SFB boundary is missing")?;
    if start != sfb_start || stop != sfb_stop {
        return Err(CodecError::InvalidData("BWE tile and SFB extents do not match"));
    }
    Ok((start, stop))
}

fn prepare_replicated(
    config: BweConfig,
    spectrum: &[f32],
    workspace: &mut BweSynthesisWorkspace,
    start: usize,
) -> Result<(), CodecError> {
    workspace.replicated.fill(0.0);
    workspace.replicated[..start].copy_from_slice(&spectrum[..start]);

    for tile in 0..usize::from(config.num_tiles) {
        let target_start = boundary(config.target_tiles[tile], "BWE target tile start is missing")?;
        let target_end = boundary(config.target_tiles[tile + 1], "BWE target tile end is missing")?;
        let source_start = boundary(config.source_tiles[tile], "BWE source tile start is missing")?;
        let width = target_end - target_start;
        let source_end = source_start + width;
        workspace.replicated[target_start..target_end]
            .copy_from_slice(&spectrum[source_start..source_end]);
    }
    Ok(())
}

fn apply_whitening(
    config: BweConfig,
    side: BweSideInfo,
    workspace: &mut BweSynthesisWorkspace,
) -> Result<(), CodecError> {
    workspace.whitened.fill(0.0);

    for tile in 0..usize::from(config.num_tiles) {
        let start = boundary(config.target_tiles[tile], "BWE target tile start is missing")?;
        let stop = boundary(config.target_tiles[tile + 1], "BWE target tile end is missing")?;
        match side.whitening_levels[tile].ok_or(CodecError::InvalidData(
            "BWE tile is missing its whitening level",
        ))? {
            WhiteningLevel::Off => {
                workspace.whitened[start..stop]
                    .copy_from_slice(&workspace.replicated[start..stop]);
            }
            WhiteningLevel::Mid => {
                for line in start..stop {
                    let mut energy = 0.0_f32;
                    for value in &workspace.replicated
                        [line - MID_WHITEN_RADIUS..=line + MID_WHITEN_RADIUS]
                    {
                        energy = value.mul_add(*value, energy);
                    }
                    let rms = (energy / (2 * MID_WHITEN_RADIUS + 1) as f32).sqrt();
                    workspace.whitened[line] = if rms == 0.0 {
                        workspace.replicated[line]
                    } else {
                        workspace.replicated[line] / rms
                    };
                }
            }
            WhiteningLevel::High => {
                let nonzero = workspace.replicated[start..stop]
                    .iter()
                    .any(|value| *value != 0.0);
                if nonzero {
                    for value in &mut workspace.whitened[start..stop] {
                        *value = workspace.rng.next_symmetric_f32();
                    }
                }
            }
        }
    }
    Ok(())
}

/// Apply AVS3-P3 bandwidth-extension reconstruction to one already inverse-grouped 1024-line MDCT
/// spectrum.
///
/// Processing order follows 7.9.3: source-to-target spectrum replication, tile whitening, then SFB
/// envelope energy matching. Frequencies at and above the configured BWE stop line are cleared.
/// The low/core band below the BWE start line is preserved bit-for-bit.
pub fn apply_bwe_synthesis(
    config: BweConfig,
    side: BweSideInfo,
    spectrum: &mut [f32],
    workspace: &mut BweSynthesisWorkspace,
) -> Result<(), CodecError> {
    if spectrum.len() != MDCT_LINES {
        return Err(CodecError::InvalidData("BWE synthesis requires a 1024-line MDCT spectrum"));
    }
    if spectrum.iter().any(|value| !value.is_finite()) {
        return Err(CodecError::InvalidData("BWE input spectrum contains non-finite values"));
    }

    let (bwe_start, bwe_stop) = validate_layout(config, side)?;
    prepare_replicated(config, spectrum, workspace, bwe_start)?;
    apply_whitening(config, side, workspace)?;

    for sfb in 0..usize::from(config.num_sfb) {
        let start = boundary(config.sfb_boundaries[sfb], "BWE SFB start is missing")?;
        let stop = boundary(config.sfb_boundaries[sfb + 1], "BWE SFB end is missing")?;
        let width = stop - start;
        let mut current_energy = 0.0_f32;
        for value in &workspace.whitened[start..stop] {
            current_energy = value.mul_add(*value, current_energy);
        }
        current_energy /= width as f32;

        let envelope_index = side.envelope_indices[sfb].ok_or(CodecError::InvalidData(
            "BWE SFB is missing its envelope index",
        ))?;
        let target_energy = 2.0_f32.powf(envelope_index as f32 / ENVELOPE_Q_STEP - ENVELOPE_Q_OFFSET);
        if !target_energy.is_finite() || target_energy < 0.0 {
            return Err(CodecError::InvalidData("BWE envelope produced invalid target energy"));
        }
        let gain = if current_energy != 0.0 {
            (target_energy / current_energy).sqrt()
        } else {
            1.0
        };
        for (source, output) in workspace.whitened[start..stop]
            .iter()
            .zip(&mut spectrum[start..stop])
        {
            *output = *source * gain;
        }
    }

    spectrum[bwe_stop..].fill(0.0);
    debug_assert!(spectrum[..bwe_start].iter().all(|value| value.is_finite()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BweMode;

    fn off_side(config: BweConfig, env: u8) -> BweSideInfo {
        let mut envelope_indices = [None; 6];
        for slot in envelope_indices.iter_mut().take(usize::from(config.num_sfb)) {
            *slot = Some(env);
        }
        let mut whitening_levels = [None; 3];
        for slot in whitening_levels.iter_mut().take(usize::from(config.num_tiles)) {
            *slot = Some(WhiteningLevel::Off);
        }
        BweSideInfo {
            envelope_indices,
            whitening_levels,
            next_bit_offset: 0,
        }
    }

    #[test]
    fn off_whitening_preserves_core_and_matches_target_sfb_energy() {
        let config = BweConfig::for_bitrate(BweMode::Mono, 96).unwrap().unwrap();
        let side = off_side(config, 34);
        let mut spectrum = [0.0_f32; MDCT_LINES];
        for (index, value) in spectrum.iter_mut().enumerate().take(672) {
            *value = if index & 1 == 0 { 2.0 } else { -2.0 };
        }
        let core_before = spectrum[..672].to_vec();
        let mut workspace = BweSynthesisWorkspace::new();
        apply_bwe_synthesis(config, side, &mut spectrum, &mut workspace).unwrap();
        assert_eq!(&spectrum[..672], core_before.as_slice());

        let target = 2.0_f32.powf(34.0 / ENVELOPE_Q_STEP - ENVELOPE_Q_OFFSET);
        let energy = spectrum[672..736].iter().map(|value| value * value).sum::<f32>() / 64.0;
        assert!((energy - target).abs() <= target.max(1.0) * 1.0e-5);
        assert!(spectrum[832..].iter().all(|&value| value == 0.0));
    }

    #[test]
    fn high_whitening_is_deterministic_for_equal_decoder_seed() {
        let config = BweConfig::for_bitrate(BweMode::Mono, 96).unwrap().unwrap();
        let mut side = off_side(config, 20);
        side.whitening_levels[0] = Some(WhiteningLevel::High);
        let input: [f32; MDCT_LINES] = std::array::from_fn(|i| if i < 672 { (i as f32 + 1.0) * 0.01 } else { 0.0 });
        let mut a = input;
        let mut b = input;
        let mut wa = BweSynthesisWorkspace::with_seed(1234);
        let mut wb = BweSynthesisWorkspace::with_seed(1234);
        apply_bwe_synthesis(config, side, &mut a, &mut wa).unwrap();
        apply_bwe_synthesis(config, side, &mut b, &mut wb).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn missing_side_information_is_rejected() {
        let config = BweConfig::for_bitrate(BweMode::Mono, 96).unwrap().unwrap();
        let side = BweSideInfo {
            envelope_indices: [None; 6],
            whitening_levels: [None; 3],
            next_bit_offset: 0,
        };
        let mut spectrum = [0.0_f32; MDCT_LINES];
        let mut workspace = BweSynthesisWorkspace::new();
        assert!(apply_bwe_synthesis(config, side, &mut spectrum, &mut workspace).is_err());
    }
}
