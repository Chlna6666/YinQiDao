use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use symphonia::core::{
    audio::GenericAudioBufferRef,
    codecs::audio::{AudioDecoder as Decoder, AudioDecoderOptions as DecoderOptions},
    errors::Error as SymphoniaError,
    formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType, probe::Hint},
    io::MediaSourceStream,
    meta::MetadataOptions,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("无法打开音频文件 {path}: {source}")]
    Open { path: PathBuf, source: io::Error },
    #[error("无法识别音频格式 {path}: {reason}")]
    Probe { path: PathBuf, reason: String },
    #[error("音频没有可解码的默认轨道: {0}")]
    MissingTrack(PathBuf),
    #[error("音频解码失败 {path}: {reason}")]
    Decode { path: PathBuf, reason: String },
    #[error("音频定位失败 {path}: {reason}")]
    Seek { path: PathBuf, reason: String },
}

#[derive(Clone, Debug)]
pub struct DecodedChunk {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
}

#[derive(Clone, Debug)]
pub struct AudioFormatInfo {
    pub sample_rate: u32,
    pub total_frames: Option<u64>,
    pub container_duration: Option<Duration>,
    #[cfg(test)]
    pub channels: u16,
}

pub struct DecoderStream {
    path: PathBuf,
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    info: AudioFormatInfo,
    decoded_frames: u64,
}

impl DecoderStream {
    pub fn open(path: &Path) -> Result<Self, DecodeError> {
        let file = File::open(path).map_err(|source| DecodeError::Open {
            path: path.to_path_buf(),
            source,
        })?;
        let source = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(extension) = path.extension().and_then(|value| value.to_str()) {
            hint.with_extension(extension);
        }
        let format = symphonia::default::get_probe()
            .probe(
                &hint,
                source,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .map_err(|error| DecodeError::Probe {
                path: path.to_path_buf(),
                reason: error.to_string(),
            })?;
        let (track_id, codec_params, total_frames, container_duration) = {
            let track = format
                .default_track(TrackType::Audio)
                .ok_or_else(|| DecodeError::MissingTrack(path.to_path_buf()))?;
            let codec_params = track
                .codec_params
                .as_ref()
                .and_then(|params| params.audio())
                .cloned()
                .ok_or_else(|| DecodeError::MissingTrack(path.to_path_buf()))?;
            let container_duration = match (track.time_base, track.duration) {
                (Some(time_base), Some(duration)) => time_base.calc_duration(duration).and_then(|time| {
                    let seconds = time.as_secs_f64();
                    (seconds.is_finite() && seconds > 0.0)
                        .then(|| Duration::from_secs_f64(seconds))
                }),
                _ => None,
            };
            (track.id, codec_params, track.num_frames, container_duration)
        };
        let info = AudioFormatInfo {
            sample_rate: codec_params.sample_rate.unwrap_or(48_000),
            total_frames,
            container_duration,
            #[cfg(test)]
            channels: codec_params
                .channels
                .as_ref()
                .map_or(2, |channels| channels.count() as u16),
        };
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&codec_params, &DecoderOptions::default())
            .map_err(|error| DecodeError::Probe {
                path: path.to_path_buf(),
                reason: error.to_string(),
            })?;
        Ok(Self {
            path: path.to_path_buf(),
            format,
            decoder,
            track_id,
            info,
            decoded_frames: 0,
        })
    }

    #[cfg(test)]
    pub fn info(&self) -> &AudioFormatInfo {
        &self.info
    }

    pub fn duration(&self) -> Option<Duration> {
        self.info
            .total_frames
            .map(|frames| {
                Duration::from_secs_f64(
                    frames as f64 / f64::from(self.info.sample_rate.max(1)),
                )
            })
            .or(self.info.container_duration)
    }

    pub fn next_chunk(&mut self) -> Result<Option<DecodedChunk>, DecodeError> {
        let mut samples = Vec::new();
        let Some((sample_rate, channels)) = self.next_chunk_into(&mut samples)? else {
            return Ok(None);
        };
        Ok(Some(DecodedChunk {
            samples,
            sample_rate,
            channels,
        }))
    }

    pub fn next_chunk_into(
        &mut self,
        samples: &mut Vec<f32>,
    ) -> Result<Option<(u32, u16)>, DecodeError> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => return Ok(None),
                Err(SymphoniaError::ResetRequired) => {
                    self.decoder.reset();
                    continue;
                }
                Err(SymphoniaError::IoError(error))
                    if error.kind() == io::ErrorKind::UnexpectedEof =>
                {
                    return Ok(None);
                }
                Err(SymphoniaError::IoError(error)) => {
                    return Err(DecodeError::Decode {
                        path: self.path.clone(),
                        reason: error.to_string(),
                    });
                }
                Err(error) => {
                    return Err(DecodeError::Decode {
                        path: self.path.clone(),
                        reason: error.to_string(),
                    });
                }
            };
            if packet.track_id != self.track_id {
                continue;
            }
            let decoded = match self.decoder.decode(&packet) {
                Ok(decoded) => decoded,
                Err(SymphoniaError::DecodeError(_)) => continue,
                Err(error) => {
                    return Err(DecodeError::Decode {
                        path: self.path.clone(),
                        reason: error.to_string(),
                    });
                }
            };
            let (sample_rate, channels) = decoded_to_f32_into(decoded, samples);
            self.decoded_frames += samples.len() as u64 / channels as u64;
            return Ok(Some((sample_rate, channels)));
        }
    }

    pub fn seek(&mut self, position: Duration) -> Result<(), DecodeError> {
        let time = symphonia::core::units::Time::try_from_secs_f64(position.as_secs_f64())
            .ok_or_else(|| DecodeError::Seek {
                path: self.path.clone(),
                reason: "定位时间超出范围".into(),
            })?;
        self.format
            .seek(
                SeekMode::Coarse,
                SeekTo::Time {
                    time,
                    track_id: Some(self.track_id),
                },
            )
            .map_err(|error| DecodeError::Seek {
                path: self.path.clone(),
                reason: error.to_string(),
            })?;
        self.decoder.reset();
        self.decoded_frames = (position.as_secs_f64() * self.info.sample_rate as f64) as u64;
        Ok(())
    }

    pub fn position(&self) -> Duration {
        Duration::from_secs_f64(self.decoded_frames as f64 / self.info.sample_rate.max(1) as f64)
    }
}

#[cfg(test)]
pub fn probe_file(path: &Path) -> Result<AudioFormatInfo, DecodeError> {
    Ok(DecoderStream::open(path)?.info().clone())
}

#[cfg(test)]
pub fn decode_to_pcm(path: &Path) -> Result<DecodedChunk, DecodeError> {
    let mut decoder = DecoderStream::open(path)?;
    let info = decoder.info().clone();
    let mut samples = Vec::new();
    while let Some(chunk) = decoder.next_chunk()? {
        samples.extend(chunk.samples);
    }
    Ok(DecodedChunk {
        samples,
        sample_rate: info.sample_rate,
        channels: info.channels,
    })
}

fn decoded_to_f32_into(decoded: GenericAudioBufferRef<'_>, samples: &mut Vec<f32>) -> (u32, u16) {
    let spec = decoded.spec().clone();
    let channels = spec.channels().count() as u16;
    let sample_rate = spec.rate();
    samples.clear();
    samples.resize(decoded.samples_interleaved(), 0.0);
    decoded.copy_to_slice_interleaved(samples);
    (sample_rate, channels)
}

#[cfg(test)]
mod tests {
    use std::{fs, time::SystemTime};

    use super::*;

    fn pcm_wav() -> Vec<u8> {
        let samples = [0i16, 8_000, -8_000, 0];
        let data_size = samples.len() * 2;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36u32 + data_size as u32).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&8_000u32.to_le_bytes());
        bytes.extend_from_slice(&16_000u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&(data_size as u32).to_le_bytes());
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn wav_probe_and_decode_return_f32_pcm() {
        let suffix = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("yinqidao-{suffix}.wav"));
        fs::write(&path, pcm_wav()).expect("wav");
        let info = probe_file(&path).expect("probe");
        assert_eq!(info.sample_rate, 8_000);
        assert_eq!(info.channels, 1);
        assert_eq!(info.total_frames, Some(4));
        let decoder = DecoderStream::open(&path).expect("decoder");
        assert_eq!(decoder.duration(), Some(Duration::from_micros(500)));
        let decoded = decode_to_pcm(&path).expect("decode");
        assert_eq!(decoded.samples.len(), 4);
        assert!(decoded.samples.iter().any(|sample| sample.abs() > 0.1));
        fs::remove_file(path).expect("cleanup");
    }

    #[test]
    fn missing_file_has_a_clear_error() {
        let error = probe_file(Path::new("missing-audio-file.flac")).expect_err("must fail");
        assert!(error.to_string().contains("无法打开音频文件"));
    }
}
