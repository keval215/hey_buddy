use crate::{AudioChunk, VoiceError};
use futures::future::BoxFuture;
use futures::stream::BoxStream;

/// Microphone source. The stream lives only while the hotkey is held;
/// dropping it closes the mic.
pub trait AudioInput: Send + Sync {
    fn open_push_to_talk(&self) -> Result<BoxStream<'static, AudioChunk>, VoiceError>;
}

pub trait AudioOutput: Send + Sync {
    /// Starts playback of `audio`. Returns immediately; use the handle to stop (barge-in)
    /// or wait for completion.
    fn play(
        &self,
        audio: BoxStream<'static, Result<AudioChunk, VoiceError>>,
    ) -> Result<PlaybackHandle, VoiceError>;
}

/// Handle to ongoing playback. Dropping it stops playback.
pub struct PlaybackHandle {
    stop: Option<Box<dyn FnOnce() + Send>>,
    done: BoxFuture<'static, ()>,
}

impl PlaybackHandle {
    pub fn new(stop: impl FnOnce() + Send + 'static, done: BoxFuture<'static, ()>) -> Self {
        Self {
            stop: Some(Box::new(stop)),
            done,
        }
    }

    /// Stop playback now (barge-in).
    pub fn stop(mut self) {
        self.do_stop();
    }

    /// Resolves when playback finished or was stopped by the backend.
    pub async fn finished(mut self) {
        (&mut self.done).await;
        self.stop = None; // finished naturally; nothing to stop
    }

    fn do_stop(&mut self) {
        if let Some(s) = self.stop.take() {
            s();
        }
    }
}

impl Drop for PlaybackHandle {
    fn drop(&mut self) {
        self.do_stop();
    }
}
