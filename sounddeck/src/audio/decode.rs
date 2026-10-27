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
pub const MAX_SECONDS: f32 = 20.0 * 60.0;

pub struct DecodedAudio {
    pub samples: Arc<Vec<f32>>,
    pub duration_secs: f32,
}

/// Decode a file into interleaved stereo f32 at 48 kHz.
///
/// The length limit is checked while collecting, not afterwards, so a 40-minute
/// file cannot fill memory before being rejected.
pub fn decode_file(path: &Path) -> Result<DecodedAudio> {
    let mut reader = PcmReader::open(path)?;
    let max_samples = (MAX_SECONDS * SAMPLE_RATE as f32) as usize * CHANNELS;
    let mut samples = Vec::new();

    while reader.next_block(&mut samples)? {
        if samples.len() > max_samples {
            bail!("Sound is longer than 20 minutes; not loading");
        }
    }
    reader.flush(&mut samples);

    if samples.is_empty() {
        bail!("Decoded audio is empty");
    }
    let duration_secs = samples.len() as f32 / (CHANNELS as f32 * SAMPLE_RATE as f32);
    Ok(DecodedAudio {
        samples: Arc::new(samples),
        duration_secs,
    })
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
}
