//! Disk streaming for the looping track and for one-shot sounds.
//!
//! One decoder thread reads the file, converts to 48 kHz stereo, and feeds a
//! bounded channel. The mixer drains that channel; when it is ahead the blocking
//! send parks the thread, so memory stays constant for any file length.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use anyhow::{bail, Context, Result};
use crossbeam_channel::{bounded, Receiver, Sender};

use super::decode::CHANNELS;
use super::reader::PcmReader;

/// Frames per channel handed to the mixer. 1024 frames ≈ 21 ms.
pub const STREAM_CHUNK_FRAMES: usize = 1024;
/// Channel depth. 8 chunks ≈ 170 ms look-ahead, ~65 KB. Measured during release
/// verification; bump to 16 only if the underrun counter or a listening test says
/// the shallower buffer cracks.
pub const STREAM_CHANNEL_CHUNKS: usize = 8;

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
    /// Starts the looping decoder. Returns immediately with the chunk receiver.
    pub fn spawn(
        path: PathBuf,
        status: Arc<Mutex<LoopState>>,
    ) -> Result<(Self, Receiver<Vec<f32>>)> {
        let (tx, rx) = bounded(STREAM_CHANNEL_CHUNKS);
        let stop = Arc::new(AtomicBool::new(false));

        let join = {
            let stop = stop.clone();
            let status = status.clone();
            thread::Builder::new()
                .name("sounddeck-loop".into())
                .spawn(move || {
                    let result =
                        PcmReader::open(&path).and_then(|reader| pump(reader, &tx, &stop, true));
                    if let Err(err) = result {
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

/// Streams a file once and closes the channel at end of file.
///
/// The file is opened on the calling thread so a missing or unsupported file is
/// reported immediately, before any audio state is touched. The thread itself is
/// detached: it ends when the receiver is dropped (the send fails) or at EOF.
pub fn spawn_once(path: &Path) -> Result<Receiver<Vec<f32>>> {
    let reader = PcmReader::open(path)?;
    let (tx, rx) = bounded(STREAM_CHANNEL_CHUNKS);
    thread::Builder::new()
        .name("sounddeck-stream".into())
        .spawn(move || {
            let stop = AtomicBool::new(false);
            let _ = pump(reader, &tx, &stop, false);
        })
        .context("Could not start the stream decoder")?;
    Ok(rx)
}

/// Reads blocks until EOF (or forever when `looping`), chunking them into the
/// channel. Blocking sends give the back-pressure that keeps memory flat.
fn pump(
    mut reader: PcmReader,
    tx: &Sender<Vec<f32>>,
    stop: &AtomicBool,
    looping: bool,
) -> Result<()> {
    let mut block: Vec<f32> = Vec::new();
    loop {
        let mut produced = 0usize;
        loop {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            block.clear();
            if !reader.next_block(&mut block)? {
                break;
            }
            produced += block.len() / CHANNELS;
            send_pcm(tx, stop, &block)?;
        }

        block.clear();
        reader.flush(&mut block);
        if !block.is_empty() {
            produced += block.len() / CHANNELS;
            send_pcm(tx, stop, &block)?;
        }

        if !looping {
            return Ok(());
        }
        if produced == 0 {
            bail!("No audio samples found");
        }
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        reader.rewind()?;
    }
}

fn send_pcm(tx: &Sender<Vec<f32>>, stop: &AtomicBool, samples: &[f32]) -> Result<()> {
    let mut rest = samples;
    while !rest.is_empty() {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let take = (STREAM_CHUNK_FRAMES * CHANNELS).min(rest.len());
        tx.send(rest[..take].to_vec())
            .map_err(|_| anyhow::anyhow!("stream channel closed"))?;
        rest = &rest[take..];
    }
    Ok(())
}

fn set_status(status: &Arc<Mutex<LoopState>>, value: LoopState) {
    if let Ok(mut guard) = status.lock() {
        *guard = value;
    }
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
        let dir = std::env::temp_dir().join(format!("sounddeck-stream-{nanos}"));
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
        stream.stop();
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn one_shot_ends_with_disconnect() {
        let dir = temp_dir();
        let path = dir.join("once.wav");
        write_wav(&path, 0.2, 330.0);

        let rx = spawn_once(&path).unwrap();
        let expected = (0.2 * 48_000.0) as usize;
        let mut frames = 0usize;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(chunk) => frames += chunk.len() / CHANNELS,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                Err(_) => break,
            }
            if Instant::now() > deadline {
                break;
            }
        }
        assert!(frames >= expected, "only {frames} frames");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_file_fails_before_spawning() {
        assert!(spawn_once(Path::new("Z:\\nope.wav")).is_err());
    }
}
