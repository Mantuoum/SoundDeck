//! Stateful conversion of decoded audio to 48 kHz interleaved stereo.
//!
//! The one-shot decoder and the streaming loop decoder share this type so the
//! output is identical no matter how the input is chunked.

use super::decode::{CHANNELS, SAMPLE_RATE};

/// Linear-interpolation resampler with a one-frame carry.
///
/// `push` can be called with any block size. Output positions are counted in
/// absolute input frames, so block boundaries never change the result. Call
/// `finish` once at end of stream to emit the tail samples.
pub struct StereoResampler {
    rate: u32,
    channels: usize,
    /// Native-rate interleaved input that has not been consumed yet.
    pending: Vec<f32>,
    /// Global input-frame index of `pending[0]`.
    start: u64,
    /// Number of 48 kHz frames emitted so far.
    out_frames: u64,
}

impl StereoResampler {
    pub fn new(rate: u32, channels: usize) -> Self {
        Self {
            rate,
            channels,
            pending: Vec::new(),
            start: 0,
            out_frames: 0,
        }
    }

    pub fn push(&mut self, src: &[f32]) -> Vec<f32> {
        if src.is_empty() || self.rate == 0 || self.channels == 0 {
            return Vec::new();
        }
        if self.rate == SAMPLE_RATE && self.channels == CHANNELS {
            return src.to_vec();
        }
        self.pending.extend_from_slice(src);
        self.render(false)
    }

    pub fn finish(&mut self) -> Vec<f32> {
        if self.rate == SAMPLE_RATE && self.channels == CHANNELS {
            return Vec::new();
        }
        self.render(true)
    }

    fn render(&mut self, finish: bool) -> Vec<f32> {
        let channels = self.channels;
        let step = self.rate as f64 / SAMPLE_RATE as f64;
        let available = self.start + (self.pending.len() / channels) as u64;
        let mut out = Vec::new();

        loop {
            // Position of the next output sample in absolute input frames.
            let t = self.out_frames as f64 * step;
            if finish {
                if t >= available as f64 {
                    break;
                }
            } else if t + 1.0 >= available as f64 {
                // Both interpolation endpoints must exist, otherwise the last
                // sample would depend on where the block happened to end.
                break;
            }

            let i0_global = t.floor() as u64;
            let i1_global = (i0_global + 1).min(available.saturating_sub(1));
            let i0 = (i0_global - self.start) as usize;
            let i1 = (i1_global - self.start) as usize;
            let frac = (t - i0_global as f64) as f32;

            let index = |frame: usize, side: usize| {
                if channels == 1 {
                    frame
                } else {
                    frame * channels + side.min(channels - 1)
                }
            };
            let l0 = self.pending[index(i0, 0)];
            let r0 = self.pending[index(i0, 1)];
            let l1 = self.pending[index(i1, 0)];
            let r1 = self.pending[index(i1, 1)];
            out.push(l0 + (l1 - l0) * frac);
            out.push(r0 + (r1 - r0) * frac);
            self.out_frames += 1;
        }

        let next = self.out_frames as f64 * step;
        let keep_from = (next.floor() as u64).min(available);
        let drop = keep_from.saturating_sub(self.start) as usize;
        if drop > 0 {
            self.pending.drain(..drop * channels);
            self.start = keep_from;
        }
        if finish {
            self.pending.clear();
            self.start = available;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stereo_48k_is_passthrough() {
        let mut r = StereoResampler::new(48_000, 2);
        let src = vec![0.1f32, -0.1, 0.2, -0.2];
        assert_eq!(r.push(&src), src);
        assert!(r.finish().is_empty());
    }

    #[test]
    fn mono_is_duplicated() {
        let mut r = StereoResampler::new(48_000, 1);
        let mut out = r.push(&[0.5f32, -0.5, 0.25]);
        out.extend(r.finish());
        assert_eq!(out.len(), 6);
        assert!((out[0] - 0.5).abs() < 1e-6);
        assert!((out[1] - 0.5).abs() < 1e-6);
        assert!((out[2] + 0.5).abs() < 1e-6);
    }

    #[test]
    fn chunking_does_not_change_output() {
        // 0.1 s of 44.1 kHz stereo: not an integer number of output frames.
        let frames = 4_410usize;
        let mut src = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let t = i as f32 / 44_100.0;
            src.push((t * 440.0 * std::f32::consts::TAU).sin() * 0.5);
            src.push((t * 330.0 * std::f32::consts::TAU).cos() * 0.5);
        }

        let mut whole = StereoResampler::new(44_100, 2);
        let mut expected = whole.push(&src);
        expected.extend(whole.finish());

        let mut chunked = StereoResampler::new(44_100, 2);
        let mut actual = Vec::new();
        for frame in src.chunks(2) {
            actual.extend(chunked.push(frame));
        }
        actual.extend(chunked.finish());

        assert_eq!(actual.len(), expected.len());
        for (a, b) in actual.iter().zip(expected.iter()) {
            assert!((a - b).abs() < 1e-6, "{a} vs {b}");
        }
    }

    #[test]
    fn chunk_sizes_match() {
        let src = vec![0.3f32; 4_410 * 2];
        let mut whole = StereoResampler::new(44_100, 2);
        let mut expected = whole.push(&src);
        expected.extend(whole.finish());

        let mut chunked = StereoResampler::new(44_100, 2);
        let mut actual = Vec::new();
        for block in src.chunks(1_024 * 2) {
            actual.extend(chunked.push(block));
        }
        actual.extend(chunked.finish());
        assert_eq!(actual.len(), expected.len());
    }

    #[test]
    fn output_length_matches_ceil() {
        let frames = 4_410usize;
        let src = vec![0.1f32; frames * 2];
        let mut r = StereoResampler::new(44_100, 2);
        let mut out = r.push(&src);
        out.extend(r.finish());
        assert_eq!(out.len() / 2, 4_800);
    }
}
