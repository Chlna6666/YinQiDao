use yinqidao_codec_core::CodecError;

use crate::hoa_transform::{HOA_TRANSFORM_BINS, HOA_TRANSFORM_LEN, HoaTransformWorkspace};
use crate::{
    BASE_OUTPUT_POSITIONS, GaHoaFrameSideInfo, HoaConfig, HoaSideInfo,
    HoaTransportSynthesisWorkspace, MAX_HOA_BASIS, NeuralNetworkType, decode_hoa_transport_frame,
};

pub const HOA_FRAME_SAMPLES: usize = BASE_OUTPUT_POSITIONS;
pub const HOA_OVERLAP_SIZE: usize = HOA_FRAME_SAMPLES / 2;
pub const HOA_SPATIAL_TABLE_BYTES_LEN: usize = 6_400;
pub const HOA_SPATIAL_TABLE_FNV1A: u64 = 0x91a0_296f_d4de_f1af;
const HOA_MAX_OUTPUT_CHANNELS: usize = 16;
const HOA_BASIS_TABLE_LEN_LOCAL: usize = crate::HOA_BASIS_TABLE_LEN;
const HOA_ANGLE_TABLE_BYTES: usize = HOA_BASIS_TABLE_LEN_LOCAL * 2 * core::mem::size_of::<i16>();
const HOA_SINE_TABLE_VALUES: usize = 257;
const HOA_QUARTER_TURN: usize = 256;
const HOA_HALF_TURN: usize = 512;
const HOA_THREE_QUARTER_TURN: usize = 768;
const HOA_FULL_TURN: usize = 1_024;
const HOA_BASIS_DELAY_FRAMES: usize = 2;

const HOA_SPATIAL_TABLE_BYTES: &[u8; HOA_SPATIAL_TABLE_BYTES_LEN] =
    include_bytes!("../assets/avs3a_hoa_spatial_tables.bin");

pub fn hoa_spatial_table_bytes() -> &'static [u8; HOA_SPATIAL_TABLE_BYTES_LEN] {
    HOA_SPATIAL_TABLE_BYTES
}

#[inline]
fn table_i16(byte_offset: usize) -> i16 {
    i16::from_le_bytes([
        HOA_SPATIAL_TABLE_BYTES[byte_offset],
        HOA_SPATIAL_TABLE_BYTES[byte_offset + 1],
    ])
}

fn hoa_angle_pair(index: usize) -> Result<[usize; 2], CodecError> {
    if index >= HOA_BASIS_TABLE_LEN_LOCAL {
        return Err(CodecError::InvalidData(
            "HOA basis index exceeds the fixed-angle table",
        ));
    }
    let offset = index * 2 * core::mem::size_of::<i16>();
    let azimuth = table_i16(offset);
    let elevation = table_i16(offset + core::mem::size_of::<i16>());
    if azimuth < 0
        || elevation < 0
        || usize::from(azimuth as u16) > HOA_FULL_TURN
        || usize::from(elevation as u16) > HOA_FULL_TURN
    {
        return Err(CodecError::InvalidData(
            "HOA fixed-angle table contains an invalid trigonometric index",
        ));
    }
    Ok([usize::from(azimuth as u16), usize::from(elevation as u16)])
}

#[inline]
fn sine_table(index: usize) -> f32 {
    debug_assert!(index < HOA_SINE_TABLE_VALUES);
    let offset = HOA_ANGLE_TABLE_BYTES + index * core::mem::size_of::<f32>();
    f32::from_le_bytes([
        HOA_SPATIAL_TABLE_BYTES[offset],
        HOA_SPATIAL_TABLE_BYTES[offset + 1],
        HOA_SPATIAL_TABLE_BYTES[offset + 2],
        HOA_SPATIAL_TABLE_BYTES[offset + 3],
    ])
}

#[inline]
fn quantized_sine(index: usize) -> f32 {
    debug_assert!(index <= HOA_FULL_TURN);
    if index <= HOA_QUARTER_TURN {
        sine_table(index)
    } else if index <= HOA_HALF_TURN {
        sine_table(HOA_HALF_TURN - index)
    } else if index <= HOA_THREE_QUARTER_TURN {
        -sine_table(index - HOA_HALF_TURN)
    } else {
        -sine_table(HOA_FULL_TURN - index)
    }
}

#[inline]
fn quantized_cosine(index: usize) -> f32 {
    debug_assert!(index <= HOA_FULL_TURN);
    if index <= HOA_QUARTER_TURN {
        sine_table(HOA_QUARTER_TURN - index)
    } else if index <= HOA_HALF_TURN {
        -sine_table(index - HOA_QUARTER_TURN)
    } else if index <= HOA_THREE_QUARTER_TURN {
        -sine_table(HOA_THREE_QUARTER_TURN - index)
    } else {
        sine_table(index - HOA_THREE_QUARTER_TURN)
    }
}

/// Return the real third-order HOA basis vector for one normative fixed-angle index.
///
/// The nine normalization constants are stored as their reference-compatible binary32 values, so
/// this hot path needs no square roots or libm trigonometry.
pub fn hoa_basis_coefficients(index: usize) -> Result<[f32; HOA_MAX_OUTPUT_CHANNELS], CodecError> {
    let [azimuth, elevation] = hoa_angle_pair(index)?;
    let sin_azimuth = quantized_sine(azimuth);
    let cos_azimuth = quantized_cosine(azimuth);
    let sin_elevation = quantized_sine(elevation);
    let cos_elevation = quantized_cosine(elevation);

    const R00: f32 = f32::from_bits(0x3e90_6eba);
    const R01: f32 = f32::from_bits(0x3efa_2a1c);
    const R04: f32 = f32::from_bits(0x3ea1_7b01);
    const R05: f32 = f32::from_bits(0x3f8b_d8a0);
    const R07: f32 = f32::from_bits(0x3f0b_d8a0);
    const R09: f32 = f32::from_bits(0x3ebf_10f8);
    const R10: f32 = f32::from_bits(0x3eea_01e8);
    const R12: f32 = f32::from_bits(0x3fb8_ffc7);
    const R14: f32 = f32::from_bits(0x3f17_0d18);

    let sin_azimuth_sq = sin_azimuth * sin_azimuth;
    let cos_azimuth_sq = cos_azimuth * cos_azimuth;
    let sin_elevation_sq = sin_elevation * sin_elevation;
    let cos_elevation_sq = cos_elevation * cos_elevation;
    let sin_cos_azimuth = sin_azimuth * cos_azimuth;
    let mut result = [0.0_f32; HOA_MAX_OUTPUT_CHANNELS];

    result[0] = R00;
    result[2] = R01 * sin_elevation;
    let mut temporary = R01 * cos_elevation;
    result[1] = temporary * sin_azimuth;
    result[3] = temporary * cos_azimuth;

    result[6] = R04 * (3.0 * sin_elevation_sq - 1.0);
    temporary = R05 * cos_elevation * sin_elevation;
    result[5] = temporary * sin_azimuth;
    result[7] = temporary * cos_azimuth;
    temporary = R07 * cos_elevation_sq;
    result[4] = temporary * 2.0 * sin_cos_azimuth;
    result[8] = temporary * (2.0 * cos_azimuth_sq - 1.0);

    result[12] = R09 * (5.0 * sin_elevation_sq * sin_elevation - 3.0 * sin_elevation);
    temporary = R10 * cos_elevation * (5.0 * sin_elevation_sq - 1.0);
    result[11] = temporary * sin_azimuth;
    result[13] = temporary * cos_azimuth;
    temporary = R12 * cos_elevation_sq * sin_elevation;
    result[10] = temporary * 2.0 * sin_cos_azimuth;
    result[14] = temporary * (2.0 * cos_azimuth_sq - 1.0);
    temporary = R14 * cos_elevation_sq * cos_elevation;
    result[9] = temporary * (3.0 * sin_azimuth - 4.0 * sin_azimuth_sq * sin_azimuth);
    result[15] = temporary * (4.0 * cos_azimuth_sq * cos_azimuth - 3.0 * cos_azimuth);
    Ok(result)
}

/// Stateful 512-hop HOA analysis, basis recovery and synthesis filter.
#[derive(Debug)]
pub struct HoaPostSynthesisWorkspace {
    transform: HoaTransformWorkspace,
    window: [f32; HOA_OVERLAP_SIZE],
    analysis_delay: Vec<[f32; HOA_FRAME_SAMPLES]>,
    spectra: Vec<[f32; HOA_FRAME_SAMPLES]>,
    recovery: Vec<[f32; HOA_FRAME_SAMPLES]>,
    synthesis_overlap: Vec<[f32; HOA_OVERLAP_SIZE]>,
    delayed_basis_indices: [[u16; MAX_HOA_BASIS]; HOA_BASIS_DELAY_FRAMES],
    basis_matrix: [[f32; MAX_HOA_BASIS]; HOA_MAX_OUTPUT_CHANNELS],
    transform_signal: [f32; HOA_TRANSFORM_LEN],
}

impl HoaPostSynthesisWorkspace {
    pub fn new() -> Self {
        let window = std::array::from_fn(|index| {
            let phase =
                core::f32::consts::PI / (2.0 * HOA_OVERLAP_SIZE as f32) * (index as f32 + 0.5);
            f64::from(phase).sin() as f32
        });
        Self {
            transform: HoaTransformWorkspace::new(),
            window,
            analysis_delay: vec![[0.0; HOA_FRAME_SAMPLES]; HOA_MAX_OUTPUT_CHANNELS],
            spectra: vec![[0.0; HOA_FRAME_SAMPLES]; HOA_MAX_OUTPUT_CHANNELS],
            recovery: vec![[0.0; HOA_FRAME_SAMPLES]; HOA_MAX_OUTPUT_CHANNELS],
            synthesis_overlap: vec![[0.0; HOA_OVERLAP_SIZE]; HOA_MAX_OUTPUT_CHANNELS],
            delayed_basis_indices: [[0; MAX_HOA_BASIS]; HOA_BASIS_DELAY_FRAMES],
            basis_matrix: [[0.0; MAX_HOA_BASIS]; HOA_MAX_OUTPUT_CHANNELS],
            transform_signal: [0.0; HOA_TRANSFORM_LEN],
        }
    }

    pub fn reset(&mut self) {
        for channel in &mut self.analysis_delay {
            channel.fill(0.0);
        }
        for channel in &mut self.spectra {
            channel.fill(0.0);
        }
        for channel in &mut self.recovery {
            channel.fill(0.0);
        }
        for channel in &mut self.synthesis_overlap {
            channel.fill(0.0);
        }
        self.delayed_basis_indices = [[0; MAX_HOA_BASIS]; HOA_BASIS_DELAY_FRAMES];
        self.basis_matrix = [[0.0; MAX_HOA_BASIS]; HOA_MAX_OUTPUT_CHANNELS];
        self.transform_signal.fill(0.0);
    }

    pub fn delayed_basis_indices(&self) -> &[[u16; MAX_HOA_BASIS]; HOA_BASIS_DELAY_FRAMES] {
        &self.delayed_basis_indices
    }

    pub fn process(
        &mut self,
        transport_pcm: &[[f32; HOA_FRAME_SAMPLES]],
        config: &HoaConfig,
        side: &HoaSideInfo,
        output: &mut [[f32; HOA_FRAME_SAMPLES]],
    ) -> Result<(), CodecError> {
        let transport_channels = usize::from(config.transport_channels);
        let output_channels = usize::from(config.output_channels);
        if transport_pcm.len() != transport_channels || output.len() != output_channels {
            return Err(CodecError::InvalidData(
                "HOA post-synthesis channel geometry mismatch",
            ));
        }
        if output_channels > HOA_MAX_OUTPUT_CHANNELS || side.groups.len() != config.groups.len() {
            return Err(CodecError::InvalidData(
                "HOA post-synthesis configuration exceeds decoder geometry",
            ));
        }

        self.analyze(transport_pcm)?;
        for channel in transport_channels..output_channels {
            self.spectra[channel].fill(0.0);
        }

        if side.spatial_analysis {
            self.recover_spatial(config, side)?;
        }
        self.synthesize(output)?;

        for (delay, current) in self.analysis_delay[..transport_channels]
            .iter_mut()
            .zip(transport_pcm)
        {
            delay.copy_from_slice(current);
        }
        self.delayed_basis_indices[0] = self.delayed_basis_indices[1];
        self.delayed_basis_indices[1] = [0; MAX_HOA_BASIS];
        for (slot, index) in self.delayed_basis_indices[1]
            .iter_mut()
            .zip(side.basis_indices.iter().copied())
        {
            *slot = index;
        }
        Ok(())
    }

    fn analyze(&mut self, transport_pcm: &[[f32; HOA_FRAME_SAMPLES]]) -> Result<(), CodecError> {
        for (channel, current) in transport_pcm.iter().enumerate() {
            for subframe in 0..2 {
                for sample in 0..HOA_OVERLAP_SIZE {
                    let (left, right) = if subframe == 0 {
                        (
                            self.analysis_delay[channel][HOA_OVERLAP_SIZE + sample],
                            current[sample],
                        )
                    } else {
                        (current[sample], current[HOA_OVERLAP_SIZE + sample])
                    };
                    self.transform_signal[sample] = left * self.window[sample];
                    self.transform_signal[HOA_OVERLAP_SIZE + sample] =
                        right * self.window[HOA_OVERLAP_SIZE - 1 - sample];
                }
                let start = subframe * HOA_TRANSFORM_BINS;
                self.transform.forward(
                    &self.transform_signal,
                    &mut self.spectra[channel][start..start + HOA_TRANSFORM_BINS],
                )?;
            }
        }
        Ok(())
    }

    fn recover_spatial(
        &mut self,
        config: &HoaConfig,
        side: &HoaSideInfo,
    ) -> Result<(), CodecError> {
        let vector_channels = usize::from(side.vector_channels);
        let residual_channels = usize::from(config.residual_channels);
        let transport_channels = usize::from(config.transport_channels);
        let output_channels = usize::from(config.output_channels);
        if vector_channels > MAX_HOA_BASIS
            || side.basis_indices.len() != vector_channels
            || vector_channels + residual_channels > transport_channels
        {
            return Err(CodecError::InvalidData(
                "HOA spatial recovery vector/residual layout mismatch",
            ));
        }

        for vector in 0..vector_channels {
            let coefficients =
                hoa_basis_coefficients(usize::from(self.delayed_basis_indices[0][vector]))?;
            for output in 0..output_channels {
                self.basis_matrix[output][vector] = coefficients[output];
            }
        }
        for channel in &mut self.recovery[..output_channels] {
            channel.fill(0.0);
        }

        for sample in 0..HOA_FRAME_SAMPLES {
            for output in 0..output_channels {
                let mut value = 0.0_f32;
                for vector in 0..vector_channels {
                    value = self.spectra[vector][sample]
                        .mul_add(self.basis_matrix[output][vector], value);
                }
                self.recovery[output][sample] = value;
            }
        }
        for residual in 0..residual_channels {
            let source = vector_channels + residual;
            for sample in 0..HOA_FRAME_SAMPLES {
                self.recovery[residual][sample] += self.spectra[source][sample];
            }
        }
        for channel in 0..output_channels {
            self.spectra[channel].copy_from_slice(&self.recovery[channel]);
        }
        Ok(())
    }

    fn synthesize(&mut self, output: &mut [[f32; HOA_FRAME_SAMPLES]]) -> Result<(), CodecError> {
        for (channel, channel_output) in output.iter_mut().enumerate() {
            channel_output.fill(0.0);
            for subframe in 0..2 {
                let start = subframe * HOA_TRANSFORM_BINS;
                self.transform.inverse(
                    &self.spectra[channel][start..start + HOA_TRANSFORM_BINS],
                    &mut self.transform_signal,
                )?;
                let output_start = subframe * HOA_OVERLAP_SIZE;
                for sample in 0..HOA_OVERLAP_SIZE {
                    let left = self.transform_signal[sample] * self.window[sample]
                        + self.synthesis_overlap[channel][sample];
                    let right = self.transform_signal[HOA_OVERLAP_SIZE + sample]
                        * self.window[HOA_OVERLAP_SIZE - 1 - sample];
                    channel_output[output_start + sample] = left;
                    self.synthesis_overlap[channel][sample] = right;
                }
            }
        }
        Ok(())
    }
}

impl Default for HoaPostSynthesisWorkspace {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Default)]
pub struct HoaSynthesisWorkspace {
    transport: HoaTransportSynthesisWorkspace,
    post: HoaPostSynthesisWorkspace,
    planar_output: Vec<[f32; HOA_FRAME_SAMPLES]>,
}

impl HoaSynthesisWorkspace {
    pub fn new() -> Self {
        Self::default()
    }

    fn prepare_output(&mut self, channels: usize) {
        if self.planar_output.len() != channels {
            self.planar_output = vec![[0.0; HOA_FRAME_SAMPLES]; channels];
        }
    }

    pub fn planar_output(&self) -> &[[f32; HOA_FRAME_SAMPLES]] {
        &self.planar_output
    }

    pub fn reset(&mut self) {
        self.transport.reset();
        self.post.reset();
        for channel in &mut self.planar_output {
            channel.fill(0.0);
        }
    }
}

pub fn parse_decode_hoa_pcm(
    nn_type: NeuralNetworkType,
    payload: &[u8],
    core_bit_offset: usize,
    order: u8,
    total_bitrate_kbps: u32,
    workspace: &mut HoaSynthesisWorkspace,
    pcm_interleaved: &mut [f32],
) -> Result<GaHoaFrameSideInfo, CodecError> {
    let config = HoaConfig::for_order_bitrate(order, total_bitrate_kbps)?;
    let output_channels = usize::from(config.output_channels);
    let expected_samples = output_channels
        .checked_mul(HOA_FRAME_SAMPLES)
        .ok_or(CodecError::InvalidData("HOA PCM output geometry overflow"))?;
    if pcm_interleaved.len() != expected_samples {
        return Err(CodecError::InvalidData(
            "HOA synthesis output has invalid interleaved PCM geometry",
        ));
    }

    let side = decode_hoa_transport_frame(
        nn_type,
        payload,
        core_bit_offset,
        order,
        total_bitrate_kbps,
        &mut workspace.transport,
    )?;
    if side.config != config {
        return Err(CodecError::Internal(
            "HOA transport parser changed the resolved bitrate configuration".into(),
        ));
    }
    workspace.prepare_output(output_channels);
    workspace.post.process(
        workspace.transport.transport_pcm(),
        &side.config,
        &side.hoa,
        &mut workspace.planar_output,
    )?;

    for frame in 0..HOA_FRAME_SAMPLES {
        let output_base = frame * output_channels;
        for channel in 0..output_channels {
            pcm_interleaved[output_base + channel] = workspace.planar_output[channel][frame];
        }
    }
    Ok(side)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fnv1a(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
        })
    }

    #[test]
    fn spatial_asset_has_normative_geometry_and_fingerprint() {
        assert_eq!(hoa_spatial_table_bytes().len(), HOA_SPATIAL_TABLE_BYTES_LEN);
        assert_eq!(fnv1a(hoa_spatial_table_bytes()), HOA_SPATIAL_TABLE_FNV1A);
        assert_eq!(hoa_angle_pair(0).unwrap(), [2, 768]);
        assert_eq!(hoa_angle_pair(1_339).unwrap(), [960, 128]);
    }

    #[test]
    fn first_fixed_basis_matches_reference_binary32_coefficients() {
        let coefficients = hoa_basis_coefficients(0).unwrap();
        assert_eq!(coefficients[0].to_bits(), 0x3e90_6eba);
        assert_eq!(coefficients[2].to_bits(), 0xbefa_2a1c);
        assert_eq!(coefficients[6].to_bits(), 0x3f21_7b01);
        assert_eq!(coefficients[12].to_bits(), 0xbf3f_10f8);
    }

    #[test]
    fn full_hoa_frontend_checks_output_geometry_before_payload_parsing() {
        let mut workspace = HoaSynthesisWorkspace::new();
        let mut pcm = [0.0_f32; HOA_FRAME_SAMPLES];
        assert!(
            parse_decode_hoa_pcm(
                NeuralNetworkType::Basic,
                &[],
                0,
                1,
                96,
                &mut workspace,
                &mut pcm,
            )
            .is_err()
        );
    }
}
