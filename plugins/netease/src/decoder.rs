use ncm2mp3_core::{AudioFormat, CoverMime, NcmDecoder};
use std::io::{Cursor, Read, Write};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputFormat {
    Mp3,
    Flac,
    M4a,
    Wav,
    Ogg,
}

impl OutputFormat {
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::Flac => "flac",
            Self::M4a => "m4a",
            Self::Wav => "wav",
            Self::Ogg => "ogg",
        }
    }
}

impl TryFrom<AudioFormat> for OutputFormat {
    type Error = DecodeError;

    fn try_from(format: AudioFormat) -> Result<Self, Self::Error> {
        match format {
            AudioFormat::Mp3 => Ok(Self::Mp3),
            AudioFormat::Flac => Ok(Self::Flac),
            AudioFormat::M4a => Ok(Self::M4a),
            AudioFormat::Wav => Ok(Self::Wav),
            AudioFormat::Ogg => Ok(Self::Ogg),
            AudioFormat::Unknown => Err(DecodeError::UnsupportedAudioFormat),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrackMetadata {
    pub title: String,
    pub artists: Vec<String>,
    pub album: String,
    pub bitrate: Option<u64>,
    pub duration_ms: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoverArt {
    pub mime_type: &'static str,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedTrack {
    pub format: OutputFormat,
    pub metadata: TrackMetadata,
    pub cover: Option<CoverArt>,
    pub audio_bytes: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("NCM decode failed: {0}")]
    Ncm(#[from] ncm2mp3_core::NcmError),
    #[error("NCM output failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("the decrypted audio format is not supported by YinQiDao")]
    UnsupportedAudioFormat,
}

/// Decrypt an NCM container held in memory into its original encoded audio bytes.
/// No transcoding is performed.
pub fn decode_ncm(input: &[u8]) -> Result<DecodedTrack, DecodeError> {
    let mut audio_bytes = Vec::with_capacity(input.len());
    let mut cover_bytes = Vec::new();
    let (format, metadata, cover_mime_type) =
        decode_ncm_to_writers(Cursor::new(input), &mut audio_bytes, &mut cover_bytes)?;
    let cover = cover_mime_type.map(|mime_type| CoverArt {
        mime_type,
        bytes: cover_bytes,
    });

    Ok(DecodedTrack {
        format,
        metadata,
        cover,
        audio_bytes,
    })
}

/// Stream an NCM container into its original encoded audio representation.
pub fn decode_ncm_to_writer<R: Read, W: Write>(
    reader: R,
    writer: &mut W,
) -> Result<(OutputFormat, TrackMetadata, Option<CoverArt>), DecodeError> {
    let mut cover_bytes = Vec::new();
    let (format, metadata, cover_mime_type) =
        decode_ncm_to_writers(reader, writer, &mut cover_bytes)?;
    let cover = cover_mime_type.map(|mime_type| CoverArt {
        mime_type,
        bytes: cover_bytes,
    });
    Ok((format, metadata, cover))
}

/// Stream cover art and decrypted audio to separate writers without buffering the complete song.
pub fn decode_ncm_to_writers<R: Read, W: Write, C: Write>(
    reader: R,
    audio_writer: &mut W,
    cover_writer: &mut C,
) -> Result<(OutputFormat, TrackMetadata, Option<&'static str>), DecodeError> {
    let (mut decoder, headers) = NcmDecoder::from_reader(reader)?;
    let format = OutputFormat::try_from(headers.effective_format())?;
    let metadata = TrackMetadata {
        title: headers.metadata.title,
        artists: headers.metadata.artists,
        album: headers.metadata.album,
        bitrate: headers.metadata.bitrate,
        duration_ms: headers.metadata.duration,
    };
    let cover_mime_type = if let Some(cover) = headers.cover {
        cover_writer.write_all(&cover.data)?;
        Some(cover_mime_type(cover.mime))
    } else {
        None
    };

    decoder.decode_to_writer(audio_writer)?;
    Ok((format, metadata, cover_mime_type))
}

const fn cover_mime_type(mime: CoverMime) -> &'static str {
    match mime {
        CoverMime::Jpeg => "image/jpeg",
        CoverMime::Png => "image/png",
        CoverMime::Unknown => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_ncm_rejects_non_ncm_input() {
        let error = decode_ncm(b"not an ncm file").expect_err("invalid magic must fail");
        assert!(matches!(
            error,
            DecodeError::Ncm(ncm2mp3_core::NcmError::InvalidMagic)
        ));
    }

    #[test]
    fn output_format_uses_yinqidao_extension() {
        assert_eq!(OutputFormat::M4a.extension(), "m4a");
    }

    #[test]
    fn unknown_audio_format_is_rejected() {
        let error = OutputFormat::try_from(AudioFormat::Unknown)
            .expect_err("unknown audio must not reach the player");
        assert!(matches!(error, DecodeError::UnsupportedAudioFormat));
    }
}
