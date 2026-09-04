use std::{path::Path, time::Duration};

use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rusty_chromaprint::{Configuration, FingerprintCompressor, Fingerprinter};

use super::decoder::DecoderStream;

const MAX_FINGERPRINT_DURATION: Duration = Duration::from_secs(120);

pub(crate) fn fingerprint_file(path: &Path) -> Result<String> {
    let mut decoder = DecoderStream::open(path)?;
    let configuration = Configuration::preset_test2();
    let mut fingerprinter = Fingerprinter::new(&configuration);
    let mut started = false;
    let mut format = None;

    while decoder.position() < MAX_FINGERPRINT_DURATION {
        let Some(chunk) = decoder.next_chunk()? else {
            break;
        };
        let current_format = (chunk.sample_rate, chunk.channels);
        if let Some(format) = format {
            if format != current_format {
                bail!("音频流中途改变了采样率或声道数，无法生成稳定指纹");
            }
        } else {
            fingerprinter
                .start(chunk.sample_rate, u32::from(chunk.channels))
                .context("初始化 Chromaprint 指纹器失败")?;
            format = Some(current_format);
            started = true;
        }
        let pcm = chunk
            .samples
            .iter()
            .map(|sample| (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16)
            .collect::<Vec<_>>();
        fingerprinter.consume(&pcm);
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
    Ok(URL_SAFE_NO_PAD.encode(compressed))
}
