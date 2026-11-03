use crate::{AudioChunk, VoiceError};
use async_trait::async_trait;
use futures::stream::BoxStream;

#[derive(Clone, PartialEq)]
pub struct Transcript {
    pub text: String,
    pub language: Option<String>,
}

#[derive(Clone, PartialEq)]
pub enum SttEvent {
    Partial(String),
    Final(Transcript),
}

// Manual Debug impls: transcript text must never reach logs.
impl std::fmt::Debug for Transcript {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transcript")
            .field("text_len", &self.text.len())
            .field("language", &self.language)
            .finish()
    }
}

impl std::fmt::Debug for SttEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SttEvent::Partial(t) => write!(f, "Partial(<{} bytes>)", t.len()),
            SttEvent::Final(t) => write!(f, "Final({t:?})"),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SttOptions {
    /// BCP-47-ish language hint; `None` = auto-detect.
    pub language: Option<String>,
}

/// Streaming speech-to-text. Audio chunks must be 16 kHz mono.
/// Dropping the returned stream cancels the work.
#[async_trait]
pub trait SttProvider: Send + Sync {
    fn id(&self) -> &str;
    async fn transcribe(
        &self,
        audio: BoxStream<'static, AudioChunk>,
        opts: SttOptions,
    ) -> Result<BoxStream<'static, Result<SttEvent, VoiceError>>, VoiceError>;
}
