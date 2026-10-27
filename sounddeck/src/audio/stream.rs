//! Streaming decoder for the looping background track.
//!
//! One decoder thread reads the file, converts to 48 kHz stereo, and feeds a
//! bounded channel. The mixer drains that channel; when the mixer is ahead the
//! blocking send parks the thread, so memory stays constant for any file length.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use anyhow::{bail, Context, Result};
use crossbeam_channel::{bounded, Receiver, Sender};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;

use super::decode::CHANNELS;
use super::resample::StereoResampler;

/// Frames per channel sent to the mixer. 1024 frames ≈ 21 ms.
pub const LOOP_CHUNK_FRAMES: usize = 1024;
/// Channel depth ≈ 0.34 s of look-ahead, ~130 KB.
const CHANNEL_CHUNKS: usize = 16;
/// A corrupt file can emit a few bad packets; this many in a row is fatal.
const MAX_DECODE_ERRORS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoopState {
    Idle,
    Playing,
    Error(String),
}

impl LoopState {
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Playing)
    }

    pub fn error(&self) -> Option<&str> {
        match self {
            Self::Error(message) => Some(message),
            _ => None,
        }
    }
}

pub struct LoopStream {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    status: Arc<Mutex<LoopState>>,
}

impl LoopStream {
    /// Starts the decoder thread. Returns immediately with the chunk receiver.
    ///
    /// The receiver must be dropped before `stop` so a decoder parked in `send`
    /// wakes up and the join stays prompt.
    pub fn spawn(
        path: PathBuf,
        status: Arc<Mutex<LoopState>>,
    ) -> Result<(Self, Receiver<Vec<f32>>)> {
        let (tx, rx) = bounded(CHANNEL_CHUNKS);
        let stop = Arc::new(AtomicBool::new(false));

        let join = {
            let stop = stop.clone();
            let status = status.clone();
            thread::Builder::new()
                .name("sounddeck-loop".into())
                .spawn(move || {
                    if let Err(err) = decode_loop(&path, &tx, &stop) {
                        if !stop.load(Ordering::Relaxed) {
                            set_status(&status, LoopState::Error(err.to_string()));
                        }
                    }
                })
                .context("Could not start the loop decoder")?
        };

        Ok((
            Self {
                stop,
                join: Some(join),
                status,
            },
            rx,
        ))
    }

    pub fn state(&self) -> LoopState {
        self.status
            .lock()
            .map(|state| state.clone())
            .unwrap_or(LoopState::Idle)
    }

    /// True once the decoder thread has exited (normally, by error, or panic).
    pub fn finished(&self) -> bool {
        self.join
            .as_ref()
            .map(|join| join.is_finished())
            .unwrap_or(true)
    }

    /// Stops and joins the decoder. The caller must drop the receiver first.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        set_status(&self.status, LoopState::Idle);
    }
}

impl Drop for LoopStream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn set_status(status: &Arc<Mutex<LoopState>>, value: LoopState) {
    if let Ok(mut guard) = status.lock() {
        *guard = value;
    }
}

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

    /// Rewinds to the first packet, or reopens the file when the container
    /// cannot seek.
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

fn decode_loop(path: &Path, tx: &Sender<Vec<f32>>, stop: &AtomicBool) -> Result<()> {
    if !path.exists() {
        bail!("File not found");
    }
    if !path.is_file() {
        bail!("Not a file");
    }

    let mut source = Source::open(path)?;
    let mut resampler: Option<StereoResampler> = None;
    let mut current_spec: Option<(u32, usize)> = None;

    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }

        let mut produced_frames = 0usize;
        let mut consecutive_errors = 0usize;

        loop {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }

            let packet = match source.format.next_packet() {
                Ok(packet) => packet,
                Err(SymphoniaError::IoError(err))
                    if err.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    break;
                }
                Err(SymphoniaError::ResetRequired) => {
                    source = Source::open(path)?;
                    resampler = None;
                    current_spec = None;
                    continue;
                }
                Err(err) => return Err(err.into()),
            };
            if packet.track_id() != source.track_id {
                continue;
            }

            let decoded = match source.decoder.decode(&packet) {
                Ok(decoded) => {
                    consecutive_errors = 0;
                    decoded
                }
                Err(SymphoniaError::DecodeError(_)) => {
                    consecutive_errors += 1;
                    if consecutive_errors > MAX_DECODE_ERRORS {
                        bail!("Too many decode errors");
                    }
                    continue;
                }
                Err(SymphoniaError::IoError(err))
                    if err.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    break;
                }
                Err(SymphoniaError::ResetRequired) => {
                    source = Source::open(path)?;
                    resampler = None;
                    current_spec = None;
                    continue;
                }
                Err(err) => return Err(err.into()),
            };

            let spec = *decoded.spec();
            let rate = spec.rate;
            let channels = spec.channels.count();
            if rate == 0 || channels == 0 {
                continue;
            }

            if current_spec != Some((rate, channels)) {
                if let Some(mut old) = resampler.take() {
                    send_pcm(tx, stop, old.finish())?;
                }
                resampler = Some(StereoResampler::new(rate, channels));
                current_spec = Some((rate, channels));
            }

            let frames = decoded.capacity().max(decoded.frames()) as u64;
            if frames == 0 {
                continue;
            }
            let mut buffer = SampleBuffer::<f32>::new(frames, spec);
            buffer.copy_interleaved_ref(decoded);

            let Some(resampler) = resampler.as_mut() else {
                bail!("internal: resampler missing for the loop track");
            };
            let out = resampler.push(buffer.samples());
            produced_frames += out.len() / CHANNELS;
            send_pcm(tx, stop, out)?;
        }

        if produced_frames == 0 {
            bail!("No audio samples found");
        }
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }

        // Drop the sub-frame interpolation tail before the next pass: keeping it
        // would duplicate the last sample at every seam.
        resampler = None;
        current_spec = None;
        source.rewind(path)?;
    }
}

fn send_pcm(tx: &Sender<Vec<f32>>, stop: &AtomicBool, samples: Vec<f32>) -> Result<()> {
    let mut rest = samples.as_slice();
    while !rest.is_empty() {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let take = (LOOP_CHUNK_FRAMES * CHANNELS).min(rest.len());
        let chunk = rest[..take].to_vec();
        tx.send(chunk)
            .map_err(|_| anyhow::anyhow!("Loop channel closed"))?;
        rest = &rest[take..];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, Instant};

    fn temp_dir() -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("sounddeck-loop-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Minimal 16-bit stereo PCM WAV writer.
    fn write_wav(path: &Path, seconds: f32, freq: f32) {
        let rate = 44_100u32;
        let frames = (rate as f32 * seconds) as u32;
        let mut data = Vec::with_capacity(frames as usize * 4);
        for i in 0..frames {
            let t = i as f32 / rate as f32;
            let sample = (t * freq * std::f32::consts::TAU).sin() * 0.4;
            let value = (sample * 32_767.0) as i16;
            data.extend_from_slice(&value.to_le_bytes());
            data.extend_from_slice(&value.to_le_bytes());
        }

        let mut wav = Vec::with_capacity(44 + data.len());
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

    fn write_empty_wav(path: &Path) {
        let rate = 48_000u32;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&36u32.to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&rate.to_le_bytes());
        wav.extend_from_slice(&(rate * 4).to_le_bytes());
        wav.extend_from_slice(&4u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&0u32.to_le_bytes());
        fs::write(path, wav).unwrap();
    }

    #[test]
    fn loops_and_releases_the_file() {
        let dir = temp_dir();
        let path = dir.join("tone.wav");
        write_wav(&path, 0.25, 440.0);

        let status = Arc::new(Mutex::new(LoopState::Playing));
        let (stream, rx) = LoopStream::spawn(path.clone(), status.clone()).unwrap();

        let expected = (0.25 * 48_000.0) as usize;
        let mut frames = 0usize;
        let mut peak_after_wrap = 0.0f32;
        let deadline = Instant::now() + Duration::from_secs(5);
        while frames < expected * 2 && Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(chunk) => {
                    if frames > expected {
                        let peak = chunk.iter().fold(0.0f32, |peak, s| peak.max(s.abs()));
                        peak_after_wrap = peak_after_wrap.max(peak);
                    }
                    frames += chunk.len() / CHANNELS;
                }
                Err(_) => break,
            }
        }

        assert!(frames >= expected * 2, "only {frames} frames");
        assert!(peak_after_wrap > 0.05, "silent after the loop seam");
        assert_ne!(stream.state(), LoopState::Idle);
        assert!(!stream.finished());

        drop(rx);
        let started = Instant::now();
        stream.stop();
        assert!(started.elapsed() < Duration::from_secs(2));

        // The decoder must not hold the file open after stop.
        fs::rename(&path, dir.join("moved.wav")).unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_file_reports_an_error() {
        let dir = temp_dir();
        let path = dir.join("empty.wav");
        write_empty_wav(&path);

        let status = Arc::new(Mutex::new(LoopState::Playing));
        let (stream, rx) = LoopStream::spawn(path.clone(), status).unwrap();

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut error = None;
        while Instant::now() < deadline {
            if let LoopState::Error(message) = stream.state() {
                error = Some(message);
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        drop(rx);
        stream.stop();
        let _ = fs::remove_dir_all(&dir);
        assert!(error.is_some(), "expected a decode error");
    }

    #[test]
    fn missing_file_reports_an_error() {
        let status = Arc::new(Mutex::new(LoopState::Playing));
        let (stream, rx) =
            LoopStream::spawn(PathBuf::from("Z:\\no-such-file.wav"), status).unwrap();
        drop(rx);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && !stream.finished() {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(stream.state(), LoopState::Error("File not found".into()));
        stream.stop();
    }
}
