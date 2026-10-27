//! One reader for every audio path.
//!
//! Both the preload path and the streaming path used to carry their own copy of
//! "open file, probe container, pick track, build decoder, resample". This module
//! owns that sequence once. `decode_file` collects it into RAM; the streaming
//! paths pump it through a bounded channel.

use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;

use super::resample::StereoResampler;

/// A corrupt file can emit a few bad packets; this many in a row is fatal.
const MAX_DECODE_ERRORS: usize = 64;

struct Source {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
}

impl Source {
    fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("Cannot open {}", path.display()))?;
        let stream = MediaSourceStream::new(Box::new(file), Default::default());

        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }

        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                stream,
                &FormatOptions {
                    enable_gapless: true,
                    ..Default::default()
                },
                &MetadataOptions::default(),
            )
            .with_context(|| format!("Unsupported or corrupt audio file: {}", path.display()))?;

        let format = probed.format;
        let (track_id, params) = {
            let track = format
                .default_track()
                .filter(|track| track.codec_params.codec != CODEC_TYPE_NULL)
                .ok_or_else(|| anyhow::anyhow!("No decodable audio track"))?;
            (track.id, track.codec_params.clone())
        };
        let decoder = symphonia::default::get_codecs()
            .make(&params, &DecoderOptions::default())
            .context("No decoder for this audio format")?;

        Ok(Self {
            format,
            decoder,
            track_id,
        })
    }

    /// Rewinds to the start, or reopens the file when the container cannot seek.
    fn rewind(&mut self, path: &Path) -> Result<()> {
        let seek = self.format.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time: Time::new(0, 0.0),
                track_id: Some(self.track_id),
            },
        );
        if seek.is_ok() {
            self.decoder.reset();
            return Ok(());
        }
        *self = Self::open(path).context("File is no longer available")?;
        Ok(())
    }
}

/// Decodes any supported file into canonical 48 kHz stereo blocks.
pub struct PcmReader {
    path: PathBuf,
    source: Source,
    resampler: Option<StereoResampler>,
    spec: Option<(u32, usize)>,
    consecutive_errors: usize,
}

impl PcmReader {
    pub fn open(path: &Path) -> Result<Self> {
        if !path.exists() {
            bail!("File not found: {}", path.display());
        }
        if !path.is_file() {
            bail!("Not a file: {}", path.display());
        }
        Ok(Self {
            path: path.to_path_buf(),
            source: Source::open(path)?,
            resampler: None,
            spec: None,
            consecutive_errors: 0,
        })
    }

    /// Appends the next decoded block to `out`. Returns `false` at end of file.
    pub fn next_block(&mut self, out: &mut Vec<f32>) -> Result<bool> {
        loop {
            let packet = match self.source.format.next_packet() {
                Ok(packet) => packet,
                Err(SymphoniaError::IoError(err))
                    if err.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    return Ok(false);
                }
                Err(SymphoniaError::ResetRequired) => {
                    self.reopen()?;
                    continue;
                }
                Err(err) => return Err(err).context("Failed while reading audio packets"),
            };
            if packet.track_id() != self.source.track_id {
                continue;
            }

            let decoded = match self.source.decoder.decode(&packet) {
                Ok(decoded) => {
                    self.consecutive_errors = 0;
                    decoded
                }
                Err(SymphoniaError::DecodeError(_)) => {
                    self.consecutive_errors += 1;
                    if self.consecutive_errors > MAX_DECODE_ERRORS {
                        bail!("Too many decode errors in {}", self.path.display());
                    }
                    continue;
                }
                Err(SymphoniaError::IoError(err))
                    if err.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    return Ok(false);
                }
                Err(SymphoniaError::ResetRequired) => {
                    self.reopen()?;
                    continue;
                }
                Err(err) => return Err(err).context("Failed while decoding audio"),
            };

            let spec = *decoded.spec();
            let channels = spec.channels.count();
            let rate = spec.rate;
            if channels == 0 || rate == 0 {
                continue;
            }

            if self.spec != Some((rate, channels)) {
                if let Some(mut old) = self.resampler.take() {
                    out.extend_from_slice(&old.finish());
                }
                self.resampler = Some(StereoResampler::new(rate, channels));
                self.spec = Some((rate, channels));
            }

            let frames = decoded.capacity().max(decoded.frames()) as u64;
            if frames == 0 {
                continue;
            }
            let mut buffer = SampleBuffer::<f32>::new(frames, spec);
            buffer.copy_interleaved_ref(decoded);

            let Some(resampler) = self.resampler.as_mut() else {
                bail!("internal: resampler missing");
            };
            out.extend_from_slice(&resampler.push(buffer.samples()));
            return Ok(true);
        }
    }

    /// Appends the resampler's interpolation tail. Call once at end of stream.
    pub fn flush(&mut self, out: &mut Vec<f32>) {
        if let Some(mut resampler) = self.resampler.take() {
            out.extend_from_slice(&resampler.finish());
        }
        self.spec = None;
    }

    /// Back to the first sample. Used by the looping track.
    pub fn rewind(&mut self) -> Result<()> {
        self.resampler = None;
        self.spec = None;
        self.consecutive_errors = 0;
        self.source.rewind(&self.path)
    }

    fn reopen(&mut self) -> Result<()> {
        self.source = Source::open(&self.path)?;
        self.resampler = None;
        self.spec = None;
        Ok(())
    }
}

/// Duration from container metadata, without decoding. `None` when unknown.
pub fn probe_seconds(path: &Path) -> Option<f32> {
    let file = File::open(path).ok()?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions {
                enable_gapless: true,
                ..Default::default()
            },
            &MetadataOptions::default(),
        )
        .ok()?;
    let track = probed.format.default_track()?;
    let params = &track.codec_params;
    let frames = params.n_frames?;
    let time_base = params.time_base?;
    let time = time_base.calc_time(frames);
    let seconds = time.seconds as f64 + time.frac;
    if !seconds.is_finite() || seconds <= 0.0 {
        return None;
    }
    Some(seconds as f32)
}

/// Number of canonical frames a duration represents, for progress reporting.
pub fn frames_for_seconds(seconds: f32) -> usize {
    if !seconds.is_finite() || seconds <= 0.0 {
        return 0;
    }
    (seconds * 48_000.0) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::decode::CHANNELS;
    use std::fs;

    fn write_wav(path: &Path, seconds: f32, channels: u16, rate: u32) {
        let frames = (rate as f32 * seconds) as u32;
        let mut data = Vec::new();
        for i in 0..frames {
            let t = i as f32 / rate as f32;
            let sample = (t * 440.0 * std::f32::consts::TAU).sin() * 0.4;
            let value = (sample * 32_767.0) as i16;
            for _ in 0..channels {
                data.extend_from_slice(&value.to_le_bytes());
            }
        }
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&((36 + data.len()) as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&channels.to_le_bytes());
        wav.extend_from_slice(&rate.to_le_bytes());
        wav.extend_from_slice(&(rate * 4).to_le_bytes());
        wav.extend_from_slice(&(channels * 2).to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
        wav.extend_from_slice(&data);
        fs::write(path, wav).unwrap();
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sounddeck-reader-{name}-{}-{}",
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
    fn reads_to_end_and_reports_duration() {
        let dir = temp("duration");
        let path = dir.join("tone.wav");
        write_wav(&path, 0.5, 2, 48_000);

        let seconds = probe_seconds(&path).expect("wav has a frame count");
        assert!((seconds - 0.5).abs() < 0.02, "{seconds}");

        let mut reader = PcmReader::open(&path).unwrap();
        let mut samples = Vec::new();
        while reader.next_block(&mut samples).unwrap() {}
        reader.flush(&mut samples);
        let frames = samples.len() / CHANNELS;
        assert!((frames as f32 - 24_000.0).abs() < 200.0, "{frames}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn mono_becomes_stereo() {
        let dir = temp("mono");
        let path = dir.join("mono.wav");
        write_wav(&path, 0.2, 1, 48_000);
        let mut reader = PcmReader::open(&path).unwrap();
        let mut samples = Vec::new();
        while reader.next_block(&mut samples).unwrap() {}
        reader.flush(&mut samples);
        assert_eq!(samples.len() % CHANNELS, 0);
        assert!(!samples.is_empty());
        for pair in samples.chunks(2) {
            assert!((pair[0] - pair[1]).abs() < 1e-6);
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn rewind_starts_again() {
        let dir = temp("rewind");
        let path = dir.join("tone.wav");
        write_wav(&path, 0.1, 2, 44_100);
        let mut reader = PcmReader::open(&path).unwrap();
        let mut first = Vec::new();
        while reader.next_block(&mut first).unwrap() {}
        reader.flush(&mut first);
        reader.rewind().unwrap();
        let mut again = Vec::new();
        while reader.next_block(&mut again).unwrap() {}
        reader.flush(&mut again);
        assert!((first.len() as i64 - again.len() as i64).abs() < 8);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_file_is_an_error() {
        assert!(PcmReader::open(Path::new("Z:\\nope.wav")).is_err());
    }
}
