use crate::VoiceError;

/// Voice activity detector. Sync; callers run it in a blocking context if needed.
pub trait Vad: Send {
    /// Window size in samples at 16 kHz (Silero: 512).
    fn window_len(&self) -> usize;
    /// Probability in [0, 1] that `window` (16 kHz mono) contains speech.
    fn speech_prob(&mut self, window: &[f32]) -> Result<f32, VoiceError>;
    fn reset(&mut self);
}
