use std::{
    error::Error,
    fmt, io,
    sync::mpsc::{self, Receiver, SyncSender},
    thread::{self, JoinHandle},
    time::Duration,
};

use yinqidao_codec_avs3::{
    Av3aIsoBmffDemuxer, Avs3Decoder, Avs3SpecificConfig, parse_dca3,
};
use yinqidao_codec_core::{AudioDecoder, AudioFrame, CodecError, DecodeStatus};

const AVS3_FRAME_SAMPLES_PER_CHANNEL: usize = 1024;
/// The AVS3 implementation still has several large synthesis stack frames. Keep the complete codec
/// lifetime on one dedicated large-stack thread instead of only protecting decoder construction and
/// frame zero; otherwise frame one and later would execute `decode_packet()` on the generic audio
/// worker and could reproduce the same Windows STATUS_STACK_OVERFLOW.
const AVS3_CODEC_WORKER_STACK_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug)]
pub(crate) enum Av3aRustError {
    Io(io::Error),
    Codec(CodecError),
    CodecWorkerStopped,
    UnexpectedStatus,
    InvalidFrame,
}

impl fmt::Display for Av3aRustError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "AV3A ISO-BMFF 读取失败: {error}"),
            Self::Codec(error) => write!(formatter, "AVS3-P3 解码失败: {error}"),
            Self::CodecWorkerStopped => formatter.write_str("AVS3-P3 专用解码线程意外退出"),
            Self::UnexpectedStatus => formatter.write_str("AVS3-P3 完整 sample 未产生 PCM frame"),
            Self::InvalidFrame => formatter.write_str("AVS3-P3 Pure Rust PCM 输出几何不合法"),
        }
    }
}

impl Error for Av3aRustError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Codec(error) => Some(error),
            Self::CodecWorkerStopped | Self::UnexpectedStatus | Self::InvalidFrame => None,
        }
    }
}

impl From<io::Error> for Av3aRustError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<CodecError> for Av3aRustError {
    fn from(error: CodecError) -> Self {
        Self::Codec(error)
    }
}

enum CodecCommand {
    Decode(AudioFrame),
    Seek {
        position: Duration,
        reply: SyncSender<Duration>,
    },
    Shutdown,
}

enum DecodeOutcome {
    Frame(AudioFrame),
    End(AudioFrame),
}

/// Player-side pure-Rust AV3A backend for complete general-full-rate PCM paths.
///
/// The demuxer, decoder and all heavy synthesis state live exclusively on `yinqidao-avs3-codec`.
/// The generic audio worker only transfers ownership of a reusable `AudioFrame` across a bounded
/// channel and waits for the result. This keeps every `decode_packet()` call on the 16 MiB codec
/// stack while preserving the PCM `Vec` allocation across frames without cloning sample buffers.
pub(crate) struct Av3aRustBackend {
    command_tx: SyncSender<CodecCommand>,
    decode_rx: Receiver<Result<DecodeOutcome, Av3aRustError>>,
    worker: Option<JoinHandle<()>>,
    first_frame: Option<AudioFrame>,
    sample_rate: u32,
    channels: u16,
    sample_count: usize,
    duration: Duration,
}

impl Av3aRustBackend {
    pub(crate) fn from_demuxer(demuxer: Av3aIsoBmffDemuxer) -> Result<Option<Self>, Av3aRustError> {
        let entry = demuxer.sample_entry().clone();
        if entry.decoder_config.is_empty() {
            return Ok(None);
        }

        // Lossless remains deliberately gated until Chapter 8 is bit-exact. Do this cheap dca3
        // classification before starting any GA codec worker or allocating heavy synthesis state.
        if matches!(
            parse_dca3(&entry.decoder_config)?,
            Avs3SpecificConfig::Lossless(_)
        ) {
            tracing::debug!(
                "AV3A Lossless 尚未打开 pure-Rust synthesis gate，跳过重型 GA codec worker"
            );
            return Ok(None);
        }

        let sample_count = demuxer.sample_count();
        if sample_count == 0 {
            return Ok(None);
        }
        let duration = demuxer.duration();

        // Capacity one provides backpressure without allowing decode requests to accumulate. The
        // caller is synchronous today, so the channel adds no PCM copy and at most one in-flight
        // command/result pair exists.
        let (command_tx, command_rx) = mpsc::sync_channel::<CodecCommand>(1);
        let (decode_tx, decode_rx) =
            mpsc::sync_channel::<Result<DecodeOutcome, Av3aRustError>>(1);
        let (init_tx, init_rx) = mpsc::sync_channel::<Result<Option<AudioFrame>, Av3aRustError>>(1);

        let worker = thread::Builder::new()
            .name("yinqidao-avs3-codec".into())
            .stack_size(AVS3_CODEC_WORKER_STACK_BYTES)
            .spawn(move || {
                let mut demuxer = demuxer;
                let initial = (|| -> Result<Option<_>, Av3aRustError> {
                    let mut packet = Vec::new();
                    let Some(_) = demuxer.next_sample_into(&mut packet)? else {
                        return Ok(None);
                    };

                    let mut decoder = match Avs3Decoder::new(&entry) {
                        Ok(decoder) => Box::new(decoder),
                        Err(CodecError::Unsupported(_)) => return Ok(None),
                        Err(error) => return Err(error.into()),
                    };
                    let mut frame = AudioFrame::default();
                    match decoder.decode_packet(&packet, &mut frame)? {
                        DecodeStatus::FrameReady => {}
                        DecodeStatus::NeedMoreData | DecodeStatus::EndOfStream => {
                            return Err(Av3aRustError::UnexpectedStatus);
                        }
                    }
                    validate_frame_geometry(&frame)?;
                    Ok(Some((decoder, packet, frame)))
                })();

                let (mut decoder, mut packet, first_frame) = match initial {
                    Ok(Some(initial)) => initial,
                    Ok(None) => {
                        let _ = init_tx.send(Ok(None));
                        return;
                    }
                    Err(error) => {
                        let _ = init_tx.send(Err(error));
                        return;
                    }
                };

                if init_tx.send(Ok(Some(first_frame))).is_err() {
                    return;
                }

                while let Ok(command) = command_rx.recv() {
                    match command {
                        CodecCommand::Decode(frame) => {
                            let result = decode_next_frame(
                                &mut demuxer,
                                decoder.as_mut(),
                                &mut packet,
                                frame,
                            );
                            if decode_tx.send(result).is_err() {
                                return;
                            }
                        }
                        CodecCommand::Seek { position, reply } => {
                            demuxer.seek(position);
                            decoder.reset();
                            packet.clear();
                            let _ = reply.send(demuxer.position());
                        }
                        CodecCommand::Shutdown => return,
                    }
                }
            })?;

        let first_frame = match init_rx.recv() {
            Ok(Ok(Some(frame))) => frame,
            Ok(Ok(None)) => {
                let _ = worker.join();
                return Ok(None);
            }
            Ok(Err(error)) => {
                let _ = worker.join();
                return Err(error);
            }
            Err(_) => {
                let _ = worker.join();
                return Err(Av3aRustError::CodecWorkerStopped);
            }
        };

        let sample_rate = first_frame.sample_rate.max(1);
        let channels = first_frame.channels;
        Ok(Some(Self {
            command_tx,
            decode_rx,
            worker: Some(worker),
            first_frame: Some(first_frame),
            sample_rate,
            channels,
            sample_count,
            duration,
        }))
    }

    pub(crate) fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub(crate) fn channels(&self) -> u16 {
        self.channels
    }

    pub(crate) fn sample_count(&self) -> usize {
        self.sample_count
    }

    pub(crate) fn duration(&self) -> Duration {
        self.duration
    }

    pub(crate) fn next_chunk_into(
        &mut self,
        samples: &mut Vec<f32>,
    ) -> Result<bool, Av3aRustError> {
        if let Some(mut frame) = self.first_frame.take() {
            std::mem::swap(samples, &mut frame.samples);
            return Ok(true);
        }

        // Hand the caller's allocation to the codec worker. The returned frame swaps the same Vec
        // back, so steady-state AVS3 playback does not clone or copy decoded PCM between threads.
        let mut recycle = AudioFrame::default();
        std::mem::swap(samples, &mut recycle.samples);
        self.command_tx
            .send(CodecCommand::Decode(recycle))
            .map_err(|_| Av3aRustError::CodecWorkerStopped)?;

        match self
            .decode_rx
            .recv()
            .map_err(|_| Av3aRustError::CodecWorkerStopped)??
        {
            DecodeOutcome::Frame(mut frame) => {
                validate_frame_geometry(&frame)?;
                if frame.channels != self.channels || frame.sample_rate != self.sample_rate {
                    return Err(Av3aRustError::InvalidFrame);
                }
                std::mem::swap(samples, &mut frame.samples);
                Ok(true)
            }
            DecodeOutcome::End(mut frame) => {
                std::mem::swap(samples, &mut frame.samples);
                samples.clear();
                Ok(false)
            }
        }
    }

    /// Reset codec history on the codec worker and select the independently decodable sample that
    /// contains `position`. Seek is not on the playback hot path, so a rendezvous reply keeps the
    /// existing exact resolved-position semantics without another shared lock. Both a dead worker
    /// before command delivery and a worker that dies before replying are surfaced explicitly.
    pub(crate) fn seek(&mut self, position: Duration) -> Result<Duration, Av3aRustError> {
        self.first_frame = None;
        let (reply_tx, reply_rx) = mpsc::sync_channel(0);
        self.command_tx
            .send(CodecCommand::Seek {
                position,
                reply: reply_tx,
            })
            .map_err(|_| Av3aRustError::CodecWorkerStopped)?;
        reply_rx
            .recv()
            .map_err(|_| Av3aRustError::CodecWorkerStopped)
    }
}

impl Drop for Av3aRustBackend {
    fn drop(&mut self) {
        let _ = self.command_tx.send(CodecCommand::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn decode_next_frame(
    demuxer: &mut Av3aIsoBmffDemuxer,
    decoder: &mut Avs3Decoder,
    packet: &mut Vec<u8>,
    mut frame: AudioFrame,
) -> Result<DecodeOutcome, Av3aRustError> {
    loop {
        let Some(_) = demuxer.next_sample_into(packet)? else {
            frame.samples.clear();
            return Ok(DecodeOutcome::End(frame));
        };
        match decoder.decode_packet(packet, &mut frame)? {
            DecodeStatus::FrameReady => {
                validate_frame_geometry(&frame)?;
                return Ok(DecodeOutcome::Frame(frame));
            }
            DecodeStatus::NeedMoreData => continue,
            DecodeStatus::EndOfStream => {
                frame.samples.clear();
                return Ok(DecodeOutcome::End(frame));
            }
        }
    }
}

fn validate_frame_geometry(frame: &AudioFrame) -> Result<(), Av3aRustError> {
    if frame.channels == 0 {
        return Err(Av3aRustError::InvalidFrame);
    }
    let expected_samples = AVS3_FRAME_SAMPLES_PER_CHANNEL
        .checked_mul(usize::from(frame.channels))
        .ok_or(Av3aRustError::InvalidFrame)?;
    if frame.samples.len() != expected_samples {
        return Err(Av3aRustError::InvalidFrame);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(channels: u16, samples: usize) -> AudioFrame {
        AudioFrame {
            samples: vec![0.0; samples],
            sample_rate: 48_000,
            channels,
        }
    }

    fn backend_for_seek_test(
        command_tx: SyncSender<CodecCommand>,
        worker: Option<JoinHandle<()>>,
    ) -> Av3aRustBackend {
        let (_decode_tx, decode_rx) =
            mpsc::sync_channel::<Result<DecodeOutcome, Av3aRustError>>(1);
        Av3aRustBackend {
            command_tx,
            decode_rx,
            worker,
            first_frame: None,
            sample_rate: 48_000,
            channels: 2,
            sample_count: 0,
            duration: Duration::ZERO,
        }
    }

    #[test]
    fn accepts_multichannel_and_hoa_frame_geometry() {
        assert!(
            validate_frame_geometry(&frame(12, 12 * AVS3_FRAME_SAMPLES_PER_CHANNEL)).is_ok()
        );
        assert!(
            validate_frame_geometry(&frame(16, 16 * AVS3_FRAME_SAMPLES_PER_CHANNEL)).is_ok()
        );
    }

    #[test]
    fn rejects_zero_channel_and_misaligned_frames() {
        assert!(validate_frame_geometry(&frame(0, 0)).is_err());
        assert!(
            validate_frame_geometry(&frame(6, 6 * AVS3_FRAME_SAMPLES_PER_CHANNEL - 1)).is_err()
        );
    }

    #[test]
    fn seek_reports_worker_stopped_before_command_delivery() {
        let (command_tx, command_rx) = mpsc::sync_channel(1);
        drop(command_rx);
        let mut backend = backend_for_seek_test(command_tx, None);

        assert!(matches!(
            backend.seek(Duration::from_secs(1)),
            Err(Av3aRustError::CodecWorkerStopped)
        ));
    }

    #[test]
    fn seek_reports_worker_stopped_before_reply() {
        let (command_tx, command_rx) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            if let Ok(CodecCommand::Seek { reply, .. }) = command_rx.recv() {
                drop(reply);
            }
        });
        let mut backend = backend_for_seek_test(command_tx, Some(worker));

        assert!(matches!(
            backend.seek(Duration::from_secs(1)),
            Err(Av3aRustError::CodecWorkerStopped)
        ));
    }
}
