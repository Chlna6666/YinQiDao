use std::{
    ffi::OsString,
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Child, ChildStdout, Command, Stdio},
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

const AV3A_PROBE_PREFIX_BYTES: u64 = 1024 * 1024;
const AV3A_PROBE_TAIL_BYTES: u64 = 8 * 1024 * 1024;
const AV3A_PCM_FRAMES_PER_CHUNK: usize = 1024;

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
    #[error("检测到 AV3A / Audio Vivid 音频，但没有可用的 AVS3-P3 解码后端。请将 YINQIDAO_AVS3_DECODER 指向带 libarcdav3a/AVS3AudioDec 支持的 FFmpeg 可执行文件: {0}")]
    Av3aBackend(PathBuf),
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
    pub channels: u16,
}

struct SymphoniaBackend {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
}

struct Av3aProcessBackend {
    executable: OsString,
    path: PathBuf,
    sample_rate: u32,
    channels: u16,
    child: Child,
    stdout: ChildStdout,
    pending_bytes: Vec<u8>,
    eof: bool,
}

impl Av3aProcessBackend {
    fn open(
        executable: OsString,
        path: &Path,
        sample_rate: u32,
        channels: u16,
        position: Duration,
    ) -> Result<Self, DecodeError> {
        let (child, stdout) =
            spawn_av3a_process(&executable, path, position).map_err(|source| DecodeError::Open {
                path: path.to_path_buf(),
                source,
            })?;
        Ok(Self {
            executable,
            path: path.to_path_buf(),
            sample_rate: sample_rate.max(1),
            channels: channels.max(1),
            child,
            stdout,
            pending_bytes: Vec::with_capacity(
                AV3A_PCM_FRAMES_PER_CHUNK
                    .saturating_mul(usize::from(channels.max(1)))
                    .saturating_mul(4),
            ),
            eof: false,
        })
    }

    fn restart(&mut self, position: Duration) -> Result<(), DecodeError> {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let (child, stdout) = spawn_av3a_process(&self.executable, &self.path, position)
            .map_err(|error| DecodeError::Seek {
                path: self.path.clone(),
                reason: format!("无法重启 AV3A 解码后端: {error}"),
            })?;
        self.child = child;
        self.stdout = stdout;
        self.pending_bytes.clear();
        self.eof = false;
        Ok(())
    }

    fn next_chunk_into(&mut self, samples: &mut Vec<f32>) -> Result<bool, DecodeError> {
        if self.eof && self.pending_bytes.is_empty() {
            return Ok(false);
        }

        let target_bytes = AV3A_PCM_FRAMES_PER_CHUNK
            .saturating_mul(usize::from(self.channels))
            .saturating_mul(4);
        let mut read_buffer = [0_u8; 16 * 1024];

        while self.pending_bytes.len() < target_bytes && !self.eof {
            let read = self
                .stdout
                .read(&mut read_buffer)
                .map_err(|error| DecodeError::Decode {
                    path: self.path.clone(),
                    reason: format!("读取 AV3A PCM 管道失败: {error}"),
                })?;
            if read == 0 {
                self.eof = true;
                let status = self.child.wait().map_err(|error| DecodeError::Decode {
                    path: self.path.clone(),
                    reason: format!("等待 AV3A 解码进程失败: {error}"),
                })?;
                if !status.success() {
                    let mut stderr = String::new();
                    if let Some(mut stream) = self.child.stderr.take() {
                        let _ = stream.read_to_string(&mut stderr);
                    }
                    let detail = stderr.trim();
                    return Err(DecodeError::Decode {
                        path: self.path.clone(),
                        reason: if detail.is_empty() {
                            format!("AV3A 后端退出码 {status}。当前 ffmpeg 很可能没有编译 libarcdav3a/AVS3AudioDec")
                        } else {
                            format!("AV3A 后端失败: {detail}")
                        },
                    });
                }
                break;
            }
            self.pending_bytes.extend_from_slice(&read_buffer[..read]);
        }

        let complete_bytes = self.pending_bytes.len() / 4 * 4;
        if complete_bytes == 0 {
            return Ok(false);
        }

        let mut raw = self.pending_bytes.split_off(complete_bytes);
        std::mem::swap(&mut raw, &mut self.pending_bytes);
        samples.clear();
        samples.reserve(raw.len() / 4);
        for bytes in raw.chunks_exact(4) {
            samples.push(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
        }

        let channels = usize::from(self.channels);
        let complete_samples = samples.len() / channels * channels;
        samples.truncate(complete_samples);
        Ok(!samples.is_empty())
    }
}

impl Drop for Av3aProcessBackend {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

enum DecoderBackend {
    Symphonia(SymphoniaBackend),
    Av3a(Av3aProcessBackend),
}

pub struct DecoderStream {
    path: PathBuf,
    backend: DecoderBackend,
    info: AudioFormatInfo,
    decoded_frames: u64,
}

impl DecoderStream {
    pub fn open(path: &Path) -> Result<Self, DecodeError> {
        let av3a = probe_av3a_sample_entry(path).map_err(|source| DecodeError::Open {
            path: path.to_path_buf(),
            source,
        })?;
        if let Some((sample_rate, channels)) = av3a {
            let executable = resolve_av3a_decoder()
                .ok_or_else(|| DecodeError::Av3aBackend(path.to_path_buf()))?;
            let backend = Av3aProcessBackend::open(
                executable,
                path,
                sample_rate,
                channels,
                Duration::ZERO,
            )
            .map_err(|error| match error {
                DecodeError::Open { .. } => DecodeError::Av3aBackend(path.to_path_buf()),
                other => other,
            })?;
            tracing::info!(
                path = %path.display(),
                sample_rate,
                channels,
                "检测到 AV3A / Audio Vivid，启用 AVS3-P3 外部解码后端"
            );
            return Ok(Self {
                path: path.to_path_buf(),
                backend: DecoderBackend::Av3a(backend),
                info: AudioFormatInfo {
                    sample_rate,
                    total_frames: None,
                    container_duration: None,
                    channels,
                },
                decoded_frames: 0,
            });
        }

        Self::open_symphonia(path)
    }

    fn open_symphonia(path: &Path) -> Result<Self, DecodeError> {
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
            backend: DecoderBackend::Symphonia(SymphoniaBackend {
                format,
                decoder,
                track_id,
            }),
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
        let (sample_rate, channels) = match &mut self.backend {
            DecoderBackend::Symphonia(backend) => loop {
                let packet = match backend.format.next_packet() {
                    Ok(Some(packet)) => packet,
                    Ok(None) => return Ok(None),
                    Err(SymphoniaError::ResetRequired) => {
                        backend.decoder.reset();
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
                if packet.track_id != backend.track_id {
                    continue;
                }
                let decoded = match backend.decoder.decode(&packet) {
                    Ok(decoded) => decoded,
                    Err(SymphoniaError::DecodeError(_)) => continue,
                    Err(error) => {
                        return Err(DecodeError::Decode {
                            path: self.path.clone(),
                            reason: error.to_string(),
                        });
                    }
                };
                break decoded_to_f32_into(decoded, samples);
            },
            DecoderBackend::Av3a(backend) => {
                if !backend.next_chunk_into(samples)? {
                    return Ok(None);
                }
                (backend.sample_rate, backend.channels)
            }
        };

        self.decoded_frames = self
            .decoded_frames
            .saturating_add(samples.len() as u64 / u64::from(channels.max(1)));
        Ok(Some((sample_rate, channels)))
    }

    pub fn seek(&mut self, position: Duration) -> Result<(), DecodeError> {
        match &mut self.backend {
            DecoderBackend::Symphonia(backend) => {
                let time = symphonia::core::units::Time::try_from_secs_f64(position.as_secs_f64())
                    .ok_or_else(|| DecodeError::Seek {
                        path: self.path.clone(),
                        reason: "定位时间超出范围".into(),
                    })?;
                backend
                    .format
                    .seek(
                        SeekMode::Coarse,
                        SeekTo::Time {
                            time,
                            track_id: Some(backend.track_id),
                        },
                    )
                    .map_err(|error| DecodeError::Seek {
                        path: self.path.clone(),
                        reason: error.to_string(),
                    })?;
                backend.decoder.reset();
            }
            DecoderBackend::Av3a(backend) => backend.restart(position)?,
        }

        self.decoded_frames =
            (position.as_secs_f64() * self.info.sample_rate.max(1) as f64) as u64;
        Ok(())
    }

    pub fn position(&self) -> Duration {
        Duration::from_secs_f64(
            self.decoded_frames as f64 / self.info.sample_rate.max(1) as f64,
        )
    }
}

fn resolve_av3a_decoder() -> Option<OsString> {
    if let Some(path) = std::env::var_os("YINQIDAO_AVS3_DECODER")
        && !path.is_empty()
    {
        return Some(path);
    }

    for candidate in [
        "ffmpeg-av3a.exe",
        "tools/ffmpeg-av3a.exe",
        "av3a/ffmpeg.exe",
        "ffmpeg-av3a",
        "tools/ffmpeg-av3a",
        "av3a/ffmpeg",
    ] {
        if Path::new(candidate).is_file() {
            return Some(OsString::from(candidate));
        }
    }

    Some(OsString::from("ffmpeg"))
}

fn spawn_av3a_process(
    executable: &OsString,
    path: &Path,
    position: Duration,
) -> io::Result<(Child, ChildStdout)> {
    let mut command = Command::new(executable);
    command
        .arg("-hide_banner")
        .arg("-nostdin")
        .arg("-loglevel")
        .arg("error");
    if !position.is_zero() {
        command.arg("-ss").arg(format!("{:.6}", position.as_secs_f64()));
    }
    command
        .arg("-i")
        .arg(path)
        .arg("-map")
        .arg("0:a:0")
        .arg("-vn")
        .arg("-sn")
        .arg("-dn")
        .arg("-c:a")
        .arg("pcm_f32le")
        .arg("-f")
        .arg("f32le")
        .arg("pipe:1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("AV3A decoder stdout pipe unavailable"))?;
    Ok((child, stdout))
}

fn probe_av3a_sample_entry(path: &Path) -> io::Result<Option<(u32, u16)>> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let mut regions = Vec::new();

    let prefix_len = length.min(AV3A_PROBE_PREFIX_BYTES) as usize;
    let mut prefix = vec![0_u8; prefix_len];
    file.read_exact(&mut prefix)?;
    regions.push(prefix);

    if length > AV3A_PROBE_PREFIX_BYTES {
        let tail_len = length.min(AV3A_PROBE_TAIL_BYTES) as usize;
        file.seek(SeekFrom::End(-(tail_len as i64)))?;
        let mut tail = vec![0_u8; tail_len];
        file.read_exact(&mut tail)?;
        regions.push(tail);
    }

    for region in regions {
        if let Some(info) = av3a_entry_from_bytes(&region) {
            return Ok(Some(info));
        }
    }
    Ok(None)
}

fn av3a_entry_from_bytes(bytes: &[u8]) -> Option<(u32, u16)> {
    let mut fallback = false;
    for (position, window) in bytes.windows(4).enumerate() {
        if window != b"av3a" {
            continue;
        }
        fallback = true;
        if position + 32 > bytes.len() {
            continue;
        }
        let channels = u16::from_be_bytes([bytes[position + 20], bytes[position + 21]]);
        let sample_rate_fixed = u32::from_be_bytes([
            bytes[position + 28],
            bytes[position + 29],
            bytes[position + 30],
            bytes[position + 31],
        ]);
        let sample_rate = sample_rate_fixed >> 16;
        if (1..=32).contains(&channels) && (8_000..=384_000).contains(&sample_rate) {
            return Some((sample_rate, channels));
        }
    }

    fallback.then_some((48_000, 2))
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
    fn av3a_sample_entry_reports_7_1_4_metadata() {
        let mut bytes = vec![0_u8; 64];
        bytes[8..12].copy_from_slice(b"av3a");
        bytes[28..30].copy_from_slice(&12_u16.to_be_bytes());
        bytes[36..40].copy_from_slice(&(44_100_u32 << 16).to_be_bytes());
        assert_eq!(av3a_entry_from_bytes(&bytes), Some((44_100, 12)));
    }

    #[test]
    fn missing_file_has_a_clear_error() {
        let error = probe_file(Path::new("missing-audio-file.flac")).expect_err("must fail");
        assert!(error.to_string().contains("无法打开音频文件"));
    }
}
