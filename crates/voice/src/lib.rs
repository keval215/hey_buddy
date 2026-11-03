//! hey_buddy `voice` crate: audio types, VAD/STT/TTS traits, and fakes.
//!
//! Error messages and logs here never contain transcript, reply text or audio.

pub mod audio;
pub mod chunker;
pub mod error;
pub mod io;
pub mod registry;
pub mod segmenter;
pub mod stt;
pub mod tts;
pub mod vad;

#[cfg(feature = "mic-cpal")]
pub mod mic_cpal;
#[cfg(feature = "stt-whisper")]
pub mod stt_whisper;
#[cfg(feature = "vad-silero")]
pub mod vad_silero;

#[cfg(feature = "fakes")]
pub mod fake;

pub use audio::{resample_to_16k_mono, AudioChunk};
pub use chunker::SentenceChunker;
pub use error::VoiceError;
pub use futures::stream::BoxStream;
pub use io::{AudioInput, AudioOutput, PlaybackHandle};
pub use segmenter::{SegmentEvent, Segmenter, SegmenterConfig};
pub use stt::{SttEvent, SttOptions, SttProvider, Transcript};
pub use tts::{TtsOptions, TtsProvider};
pub use vad::Vad;
