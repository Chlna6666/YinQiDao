use std::{path::Path, time::Duration};

use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rusty_chromaprint::{Configuration, FingerprintCompressor, Fingerprinter};

use super::decoder::DecoderStream;

const MAX_FINGERPRINT_DURATION: Duration = Duration::from_secs(120);
pub(crate) const CHROMAPRINT_ALGORITHM: &str = "chromaprint-v1-compressed";

/// One Host-computed Chromaprint payload shared by authenticated plugin recognition and AcoustID.
/// `compressed` is the binary output of `FingerprintCompressor`; `acoustid` is the URL-safe base64
/// representation required by AcoustID. Keeping both forms avoids decoding the audio twice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AudioFingerprint {
    pub compressed: Vec<u8>,
    pub acoustid: String,
}

pub(crate) fn fingerprint_file_payload(path: &Path) -> Result<AudioFingerprint> {
    let mut decoder = DecoderStream::open(path)?;
    let configuration = Configuration::preset_test2();
    let mut fingerprinter = Fingerprinter::new(&configuration);
    let mut started = false;
    let mut format = None;
    let mut decoded_samples = Vec::<f32>::new();
    let mut quantized_pcm = Vec::<i16>::new();

    while decoder.position() < MAX_FINGERPRINT_DURATION {
        let Some((sample_rate, channels)) = decoder.next_chunk_into(&mut decoded_samples)? else {
            break;
        };
        let current_format = (sample_rate, channels);
        if let Some(format) = format {
            if format != current_format {
                bail!("音频流中途改变了采样率或声道数，无法生成稳定指纹");
            }
        } else {
            fingerprinter
                .start(sample_rate, u32::from(channels))
                .context("初始化 Chromaprint 指纹器失败")?;
            format = Some(current_format);
            started = true;
        }

        // Both buffers keep their capacity for the full fingerprint scan. The old path created a
        // DecodedChunk Vec and then collect() allocated another i16 Vec for every decoder chunk.
        quantized_pcm.clear();
        if quantized_pcm.capacity() < decoded_samples.len() {
            quantized_pcm.reserve(decoded_samples.len());
        }
        quantized_pcm.extend(
            decoded_samples
                .iter()
                .map(|sample| (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16),
        );
        fingerprinter.consume(&quantized_pcm);
    }

    if !started {
        bail!("音频中没有可用于识别的 PCM 数据");
    }
    fingerprinter.finish();
    if fingerprinter.fingerprint().is_empty() {
        bail!("音频过短，无法生成 AcoustID 指纹");
    }
    let compressed =
        FingerprintCompressor::from(&configuration).compress(fingerprinter.fingerprint());
    let acoustid = URL_SAFE_NO_PAD.encode(&compressed);
    Ok(AudioFingerprint {
        compressed,
        acoustid,
    })
}

pub(crate) fn fingerprint_file(path: &Path) -> Result<String> {
    Ok(fingerprint_file_payload(path)?.acoustid)
}
