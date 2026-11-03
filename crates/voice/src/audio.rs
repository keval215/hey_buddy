/// Mono PCM audio, f32 samples in [-1, 1].
#[derive(Clone, PartialEq)]
pub struct AudioChunk {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

// Manual Debug: never print audio content.
impl std::fmt::Debug for AudioChunk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioChunk")
            .field("len", &self.samples.len())
            .field("sample_rate", &self.sample_rate)
            .finish()
    }
}

pub const TARGET_RATE: u32 = 16_000;

/// Downmix interleaved `channels` to mono (average) and linearly resample to 16 kHz.
/// `input.samples` is interleaved when `channels > 1`. `channels == 0` is treated as 1.
/// A zero input sample rate yields an empty chunk.
pub fn resample_to_16k_mono(input: &AudioChunk, channels: u16) -> AudioChunk {
    let ch = channels.max(1) as usize;
    let mono: Vec<f32> = if ch == 1 {
        input.samples.clone()
    } else {
        input
            .samples
            .chunks_exact(ch)
            .map(|f| f.iter().sum::<f32>() / ch as f32)
            .collect()
    };
    if input.sample_rate == 0 {
        return AudioChunk {
            samples: Vec::new(),
            sample_rate: TARGET_RATE,
        };
    }
    if input.sample_rate == TARGET_RATE || mono.is_empty() {
        return AudioChunk {
            samples: mono,
            sample_rate: TARGET_RATE,
        };
    }
    let ratio = input.sample_rate as f64 / TARGET_RATE as f64;
    let out_len = ((mono.len() as f64) / ratio).floor() as usize;
    let last = mono.len() - 1;
    let samples = (0..out_len)
        .map(|i| {
            let pos = i as f64 * ratio;
            let idx = pos.floor() as usize;
            let frac = (pos - idx as f64) as f32;
            let a = mono[idx.min(last)];
            let b = mono[(idx + 1).min(last)];
            a + (b - a) * frac
        })
        .collect();
    AudioChunk {
        samples,
        sample_rate: TARGET_RATE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(samples: Vec<f32>, sample_rate: u32) -> AudioChunk {
        AudioChunk {
            samples,
            sample_rate,
        }
    }

    #[test]
    fn passthrough_at_16k_mono() {
        let c = chunk(vec![0.1, 0.2, 0.3], 16_000);
        assert_eq!(resample_to_16k_mono(&c, 1), c);
    }

    #[test]
    fn downmixes_stereo() {
        let c = chunk(vec![1.0, 0.0, 0.5, 0.5], 16_000);
        let o = resample_to_16k_mono(&c, 2);
        assert_eq!(o.samples, vec![0.5, 0.5]);
    }

    #[test]
    fn downsamples_48k_by_three() {
        let c = chunk((0..480).map(|i| i as f32).collect(), 48_000);
        let o = resample_to_16k_mono(&c, 1);
        assert_eq!(o.sample_rate, 16_000);
        assert_eq!(o.samples.len(), 160);
        assert_eq!(o.samples[1], 3.0);
        assert_eq!(o.samples[10], 30.0);
    }

    #[test]
    fn upsamples_8k_interpolating() {
        let c = chunk(vec![0.0, 1.0, 0.0, 1.0], 8_000);
        let o = resample_to_16k_mono(&c, 1);
        assert_eq!(o.samples.len(), 8);
        assert!((o.samples[1] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn edge_cases() {
        assert!(resample_to_16k_mono(&chunk(vec![], 44_100), 2)
            .samples
            .is_empty());
        assert!(resample_to_16k_mono(&chunk(vec![1.0], 0), 1)
            .samples
            .is_empty());
        // channels == 0 treated as mono; ragged trailing frame dropped
        assert_eq!(
            resample_to_16k_mono(&chunk(vec![0.2], 16_000), 0).samples,
            vec![0.2]
        );
        assert_eq!(
            resample_to_16k_mono(&chunk(vec![1.0, 1.0, 9.0], 16_000), 2).samples,
            vec![1.0]
        );
    }
}
