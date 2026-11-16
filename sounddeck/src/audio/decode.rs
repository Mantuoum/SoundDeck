//! Full-file decode, used only by the preload cache.
//!
//! Playback never comes through here any more: sounds that are not preloaded are
//! streamed. That keeps this path bounded by the preload limits (30 s), so the
//! old 460 MB transient decode of a long file no longer exists.

use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Result};

use super::reader::PcmReader;

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
pub const SAMPLE_BYTES: usize = std::mem::size_of::<f32>();
pub const MAX_SECONDS: f32 = 20.0 * 60.0;

pub struct DecodedAudio {
    pub samples: Arc<Vec<f32>>,
    pub duration_secs: f32,
}

/// Why a bounded decode stopped.
#[derive(Debug)]
pub enum BoundedDecodeError {
    /// The decoded audio would cross the caller's byte limit. The caller should
    /// stream the sound instead of keeping it resident.
    TooBig,
    /// A real failure: unreadable, corrupt, or no audio at all.
    Failed(anyhow::Error),
}

/// Decode a file into interleaved stereo f32 at 48 kHz, stopping once the
/// decoded buffer would cross `max_bytes`.
///
/// The preload cache uses this so a wrong container estimate cannot spike
/// memory: the decode is abandoned at the bound and the sound streams.
pub fn decode_file_bounded(
    path: &Path,
    max_bytes: usize,
) -> Result<DecodedAudio, BoundedDecodeError> {
    let mut reader = PcmReader::open(path).map_err(BoundedDecodeError::Failed)?;
    let mut samples = Vec::new();

    loop {
        match reader.next_block(&mut samples) {
            Ok(true) => {
                if samples.len().saturating_mul(SAMPLE_BYTES) > max_bytes {
                    return Err(BoundedDecodeError::TooBig);
                }
            }
            Ok(false) => break,
            Err(err) => return Err(BoundedDecodeError::Failed(err)),
        }
    }
    reader.flush(&mut samples);

    if samples.is_empty() {
        return Err(BoundedDecodeError::Failed(anyhow::anyhow!(
            "Decoded audio is empty"
        )));
    }
    let duration_secs = samples.len() as f32 / (CHANNELS as f32 * SAMPLE_RATE as f32);
    Ok(DecodedAudio {
        samples: Arc::new(samples),
        duration_secs,
    })
}

/// Decode a file into interleaved stereo f32 at 48 kHz.
///
/// The length limit is checked while collecting, not afterwards, so a 40-minute
/// file cannot fill memory before being rejected.
pub fn decode_file(path: &Path) -> Result<DecodedAudio> {
    let max_samples = (MAX_SECONDS * SAMPLE_RATE as f32) as usize * CHANNELS;
    match decode_file_bounded(path, max_samples.saturating_mul(SAMPLE_BYTES)) {
        Ok(audio) => Ok(audio),
        Err(BoundedDecodeError::TooBig) => {
            bail!("Sound is longer than 20 minutes; not loading")
        }
        Err(BoundedDecodeError::Failed(err)) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_wav(path: &Path, seconds: f32) {
        let rate = 48_000u32;
        let frames = (rate as f32 * seconds) as u32;
        let mut data = Vec::new();
        for _ in 0..frames {
            data.extend_from_slice(&0i16.to_le_bytes());
            data.extend_from_slice(&0i16.to_le_bytes());
        }
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&((36 + data.len()) as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&rate.to_le_bytes());
        wav.extend_from_slice(&(rate * 4).to_le_bytes());
        wav.extend_from_slice(&4u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
        wav.extend_from_slice(&data);
        fs::write(path, wav).unwrap();
    }

    #[test]
    fn decodes_a_short_wav() {
        let dir = std::env::temp_dir().join(format!(
            "sounddeck-decode-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tone.wav");
        write_wav(&path, 0.25);

        let decoded = decode_file(&path).unwrap();
        assert!((decoded.duration_secs - 0.25).abs() < 0.01);
        assert_eq!(decoded.samples.len(), 48_000 / 4 * 2);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_file_is_an_error() {
        assert!(decode_file(Path::new("Z:\\nope.wav")).is_err());
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sounddeck-decode-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_bounded_decode_returns_audio_within_the_limit() {
        let dir = temp_dir("within");
        let path = dir.join("tone.wav");
        write_wav(&path, 0.5);
        // 0.5 s = 96,000 bytes; give it room.
        let decoded = decode_file_bounded(&path, 200_000).unwrap();
        assert!((decoded.duration_secs - 0.5).abs() < 0.01);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_bounded_decode_stops_once_the_limit_would_be_crossed() {
        let dir = temp_dir("over");
        let path = dir.join("tone.wav");
        write_wav(&path, 1.0);
        // 1 s decodes to 384,000 bytes; 100,000 cannot hold it.
        assert!(matches!(
            decode_file_bounded(&path, 100_000),
            Err(BoundedDecodeError::TooBig)
        ));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_bounded_decode_reports_a_real_failure() {
        assert!(matches!(
            decode_file_bounded(Path::new("Z:\\nope.wav"), 1_000_000),
            Err(BoundedDecodeError::Failed(_))
        ));
    }
}
