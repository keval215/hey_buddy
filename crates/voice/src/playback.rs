//! Device-independent pieces of streaming playback: a stateful linear resampler and a
//! mono sample queue that output callbacks drain. Used by `speaker_cpal`; kept free of
//! platform deps so it is testable everywhere. Audio is only ever held in memory.

use std::collections::VecDeque;

/// Streaming linear resampler (mono). Keeps phase and the last sample across chunks so
/// chunk boundaries do not click.
#[derive(Debug, Clone)]
pub struct LinearResampler {
    /// input samples consumed per output sample
    step: f64,
    /// position in the virtual stream `[prev, chunk...]`, relative to `prev` (index 0)
    pos: f64,
    prev: f32,
    primed: bool,
}

impl LinearResampler {
    pub fn new(from_rate: u32, to_rate: u32) -> Self {
        let step = if from_rate == 0 || to_rate == 0 {
            1.0
        } else {
            from_rate as f64 / to_rate as f64
        };
        Self {
            step,
            pos: 0.0,
            prev: 0.0,
            primed: false,
        }
    }

    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        if input.is_empty() {
            return Vec::new();
        }
        if !self.primed {
            self.prev = input[0];
            self.pos = 1.0; // start at input[0], not at the duplicated prev
            self.primed = true;
        }
        // virtual[0] = prev, virtual[i+1] = input[i]
        let n = input.len() + 1;
        let at = |i: usize| if i == 0 { self.prev } else { input[i - 1] };
        let mut out = Vec::with_capacity((input.len() as f64 / self.step) as usize + 1);
        while self.pos + 1.0 < n as f64 {
            let i = self.pos.floor() as usize;
            let frac = (self.pos - i as f64) as f32;
            let (a, b) = (at(i), at(i + 1));
            out.push(a + (b - a) * frac);
            self.pos += self.step;
        }
        // Next call's virtual[0] is this call's last sample.
        self.pos -= (n - 1) as f64;
        self.prev = *input.last().unwrap();
        out
    }
}

/// Mono sample queue shared between a feeder and an output callback.
#[derive(Debug, Default)]
pub struct PlayQueue {
    samples: VecDeque<f32>,
    input_done: bool,
}

impl PlayQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, s: &[f32]) {
        self.samples.extend(s.iter().copied());
    }

    /// No more input will arrive.
    pub fn finish(&mut self) {
        self.input_done = true;
    }

    pub fn clear(&mut self) {
        self.samples.clear();
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Input ended and everything was played.
    pub fn is_drained(&self) -> bool {
        self.input_done && self.samples.is_empty()
    }

    /// Fill interleaved `out` (`channels` per frame) from the queue; underrun -> silence.
    pub fn fill(&mut self, out: &mut [f32], channels: usize) {
        let ch = channels.max(1);
        for frame in out.chunks_mut(ch) {
            let s = self.samples.pop_front().unwrap_or(0.0);
            frame.iter_mut().for_each(|o| *o = s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_rate_is_passthrough() {
        let mut r = LinearResampler::new(48_000, 48_000);
        let x: Vec<f32> = (0..10).map(|i| i as f32).collect();
        let y = r.process(&x);
        // The final sample is held back as interpolation context for the next chunk.
        assert_eq!(y, x[..9].to_vec());
        assert_eq!(r.process(&[10.0]), vec![9.0]);
    }

    #[test]
    fn upsample_length_and_chunk_invariance() {
        let x: Vec<f32> = (0..480).map(|i| (i as f32 * 0.05).sin()).collect();
        let whole = LinearResampler::new(24_000, 48_000).process(&x);
        assert!((whole.len() as i64 - 960).abs() <= 2);
        let mut r = LinearResampler::new(24_000, 48_000);
        let mut parts = Vec::new();
        for c in x.chunks(37) {
            parts.extend(r.process(c));
        }
        let n = whole.len().min(parts.len());
        assert!((whole.len() as i64 - parts.len() as i64).abs() <= 2);
        for i in 0..n {
            assert!((whole[i] - parts[i]).abs() < 1e-4, "i={i}");
        }
    }

    #[test]
    fn downsample_halves() {
        let x = vec![0.5f32; 960];
        let y = LinearResampler::new(48_000, 24_000).process(&x);
        assert!((y.len() as i64 - 480).abs() <= 2);
        assert!(y.iter().all(|s| (*s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn queue_fill_duplicates_channels_and_pads_silence() {
        let mut q = PlayQueue::new();
        q.push(&[0.1, 0.2]);
        let mut out = [9.0f32; 8]; // 4 frames x 2 ch
        q.fill(&mut out, 2);
        assert_eq!(out, [0.1, 0.1, 0.2, 0.2, 0.0, 0.0, 0.0, 0.0]);
        assert!(!q.is_drained());
        q.finish();
        assert!(q.is_drained());
    }
}
