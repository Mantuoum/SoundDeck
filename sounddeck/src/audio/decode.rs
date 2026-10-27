use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use super::resample::StereoResampler;

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
pub const MAX_SECONDS: f32 = 20.0 * 60.0;

pub struct DecodedAudio {
    pub samples: Arc<Vec<f32>>,
    pub duration_secs: f32,
}

/// Estimated canonical size of a file, from its container metadata only.
///
/// Used before decoding to decide whether a sound is even eligible for the
/// preload cache, so a long file never costs a 400 MB decode just to be refused.
/// Returns `None` when the container does not expose a frame count; the caller
/// then has to fall back to decoding.
pub fn probe_bytes(path: &Path) -> Option<usize> {
    let file = File::open(path).ok()?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .ok()?;
    let track = probed.format.default_track()?;
    let params = &track.codec_params;
    let frames = params.n_frames?;
    let time_base = params.time_base?;
    let time = time_base.calc_time(frames);
    let seconds = time.seconds as f64 + time.frac;
    let bytes = crate::memory::bytes_for_seconds(seconds as f32);
    if bytes == 0 {
        return None;
    }
    Some(bytes)
}

/// Decode any supported file into interleaved stereo f32 @ 48 kHz.
pub fn decode_file(path: &Path) -> Result<DecodedAudio> {
    if !path.exists() {
        bail!("File not found: {}", path.display());
    }
    if !path.is_file() {
        bail!("Not a file: {}", path.display());
    }

    let file = File::open(path).with_context(|| format!("Cannot open {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions {
                enable_gapless: true,
                ..Default::default()
            },
            &MetadataOptions::default(),
        )
        .with_context(|| format!("Unsupported or corrupt audio file: {}", path.display()))?;

    let mut format = probed.format;
    let track = format
        .default_track()
        .filter(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| anyhow::anyhow!("No decodable audio track"))?;
    let track_id = track.id;
    let params = track.codec_params.clone();

    let mut decoder = symphonia::default::get_codecs()
        .make(&params, &DecoderOptions::default())
        .context("No decoder for this audio format")?;

    // Decoded audio is collected per (sample-rate, channel-count) segment, in case
    // the stream changes format mid-file (rare, but legal).
    let mut segments: Vec<(Vec<f32>, usize, u32)> = Vec::new();
    let mut current: Option<(Vec<f32>, usize, u32)> = None;

    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(err))
                if err.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(SymphoniaError::ResetRequired) => break,
            Err(err) => return Err(err).context("Failed while reading audio packets"),
        };
        if packet.track_id() != track_id {
            continue;
        }

        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(SymphoniaError::IoError(err))
                if err.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(err) => return Err(err).context("Failed while decoding audio"),
        };

        let spec = *decoded.spec();
        let channels = spec.channels.count();
        let rate = spec.rate;
        if channels == 0 || rate == 0 {
            continue;
        }

        let frames = decoded.capacity().max(decoded.frames()) as u64;
        if frames == 0 {
            continue;
        }
        let mut buf = SampleBuffer::<f32>::new(frames, spec);
        buf.copy_interleaved_ref(decoded);

        match &mut current {
            Some((samples, ch, r)) if *ch == channels && *r == rate => {
                samples.extend_from_slice(buf.samples());
            }
            Some(_) => {
                if let Some(segment) = current.take() {
                    segments.push(segment);
                }
                current = Some((buf.samples().to_vec(), channels, rate));
            }
            None => {
                current = Some((buf.samples().to_vec(), channels, rate));
            }
        }
    }

    if let Some(segment) = current.take() {
        segments.push(segment);
    }
    if segments.is_empty() {
        bail!("No audio samples found");
    }

    let mut canonical: Vec<f32> = Vec::new();
    for (samples, channels, rate) in segments {
        canonical.extend_from_slice(&to_canonical(&samples, channels, rate));
        let seconds = canonical.len() as f32 / (CHANNELS as f32 * SAMPLE_RATE as f32);
        if seconds > MAX_SECONDS {
            bail!("Sound is longer than 20 minutes; not loading");
        }
    }
    if canonical.is_empty() {
        bail!("Decoded audio is empty");
    }

    let duration_secs = canonical.len() as f32 / (CHANNELS as f32 * SAMPLE_RATE as f32);
    Ok(DecodedAudio {
        samples: Arc::new(canonical),
        duration_secs,
    })
}

/// Convert interleaved source audio to 48 kHz stereo with linear interpolation.
fn to_canonical(src: &[f32], channels: usize, rate: u32) -> Vec<f32> {
    let mut resampler = StereoResampler::new(rate, channels);
    let mut out = resampler.push(src);
    out.extend(resampler.finish());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_is_duplicated() {
        let src = vec![0.5f32, -0.5, 0.25];
        let out = to_canonical(&src, 1, SAMPLE_RATE);
        assert_eq!(out.len(), src.len() * 2);
        assert!((out[0] - 0.5).abs() < 1e-6);
        assert!((out[1] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn resamples_length() {
        let src = vec![0.0f32; 44100]; // 1 second of mono 44.1 kHz
        let out = to_canonical(&src, 1, 44100);
        assert!((out.len() as f32 / 2.0 - 48000.0).abs() <= 1.0);
    }

    #[test]
    fn empty_input() {
        assert!(to_canonical(&[], 2, 48000).is_empty());
    }
}
