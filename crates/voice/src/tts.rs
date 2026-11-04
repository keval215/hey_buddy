use crate::{AudioChunk, VoiceError};
use async_trait::async_trait;
use futures::stream::BoxStream;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TtsOptions {
    /// Voice id; `None` = provider default.
    pub voice: Option<String>,
}

/// Streaming text-to-speech: text pieces (usually sentences) in, audio out.
/// Dropping the returned stream cancels the work.
#[async_trait]
pub trait TtsProvider: Send + Sync {
    fn id(&self) -> &str;
    async fn synthesize(
        &self,
        text: BoxStream<'static, String>,
        opts: TtsOptions,
    ) -> Result<BoxStream<'static, Result<AudioChunk, VoiceError>>, VoiceError>;
}
