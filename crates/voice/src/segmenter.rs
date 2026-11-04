/// Thresholds for turning per-window speech probabilities into start/end events.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SegmenterConfig {
    /// Probability at or above which a window counts as speech.
    pub start_threshold: f32,
    /// While in speech, probability below this counts as silence.
    pub end_threshold: f32,
    /// Consecutive speech windows needed to emit `SpeechStart`.
    pub min_speech_windows: usize,
    /// Consecutive silent windows needed to emit `SpeechEnd`.
    pub hangover_windows: usize,
}

impl Default for SegmenterConfig {
    fn default() -> Self {
        Self {
            start_threshold: 0.5,
            end_threshold: 0.35,
            min_speech_windows: 2,
            hangover_windows: 15,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentEvent {
    SpeechStart,
    SpeechEnd,
}

/// Hysteresis state machine: probabilities in, `SpeechStart`/`SpeechEnd` out.
#[derive(Debug, Clone)]
pub struct Segmenter {
    cfg: SegmenterConfig,
    in_speech: bool,
    run: usize,
}

impl Segmenter {
    pub fn new(cfg: SegmenterConfig) -> Self {
        Self {
            cfg,
            in_speech: false,
            run: 0,
        }
    }

    pub fn in_speech(&self) -> bool {
        self.in_speech
    }

    pub fn reset(&mut self) {
        self.in_speech = false;
        self.run = 0;
    }

    /// Feed one window's probability. Non-finite values count as silence.
    pub fn push(&mut self, prob: f32) -> Option<SegmentEvent> {
        let p = if prob.is_finite() { prob } else { 0.0 };
        if self.in_speech {
            if p < self.cfg.end_threshold {
                self.run += 1;
                if self.run >= self.cfg.hangover_windows.max(1) {
                    self.in_speech = false;
                    self.run = 0;
                    return Some(SegmentEvent::SpeechEnd);
                }
            } else {
                self.run = 0;
            }
        } else if p >= self.cfg.start_threshold {
            self.run += 1;
            if self.run >= self.cfg.min_speech_windows.max(1) {
                self.in_speech = true;
                self.run = 0;
                return Some(SegmentEvent::SpeechStart);
            }
        } else {
            self.run = 0;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg() -> Segmenter {
        Segmenter::new(SegmenterConfig {
            min_speech_windows: 2,
            hangover_windows: 3,
            ..Default::default()
        })
    }

    fn run(s: &mut Segmenter, probs: &[f32]) -> Vec<(usize, SegmentEvent)> {
        probs
            .iter()
            .enumerate()
            .filter_map(|(i, p)| s.push(*p).map(|e| (i, e)))
            .collect()
    }

    #[test]
    fn start_and_end_with_hangover() {
        let mut s = seg();
        let ev = run(&mut s, &[0.0, 0.9, 0.9, 0.9, 0.1, 0.1, 0.1, 0.1]);
        assert_eq!(
            ev,
            vec![(2, SegmentEvent::SpeechStart), (6, SegmentEvent::SpeechEnd)]
        );
        assert!(!s.in_speech());
    }

    #[test]
    fn single_blip_ignored() {
        let mut s = seg();
        assert!(run(&mut s, &[0.9, 0.0, 0.9, 0.0, 0.9, 0.0]).is_empty());
    }

    #[test]
    fn short_pause_does_not_end() {
        let mut s = seg();
        let ev = run(&mut s, &[0.9, 0.9, 0.1, 0.1, 0.9, 0.1, 0.1, 0.9]);
        assert_eq!(ev, vec![(1, SegmentEvent::SpeechStart)]);
        assert!(s.in_speech());
    }

    #[test]
    fn hysteresis_between_thresholds_keeps_speech() {
        let mut s = seg();
        let ev = run(&mut s, &[0.9, 0.9, 0.4, 0.4, 0.4, 0.4]);
        assert_eq!(ev, vec![(1, SegmentEvent::SpeechStart)]);
    }

    #[test]
    fn nan_counts_as_silence_and_reset_works() {
        let mut s = seg();
        run(&mut s, &[0.9, 0.9]);
        assert!(s.in_speech());
        s.reset();
        assert!(!s.in_speech());
        let ev = run(&mut s, &[0.9, 0.9, f32::NAN, f32::NAN, f32::NAN]);
        assert_eq!(
            ev,
            vec![(1, SegmentEvent::SpeechStart), (4, SegmentEvent::SpeechEnd)]
        );
    }
}
