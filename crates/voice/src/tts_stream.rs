//! Streaming TTS structure shared by the local adapters (Phonon, Kokoro).
//!
//! `StreamingTts<C>` turns a [`TtsCore`] (one-sentence synthesis) into a [`TtsProvider`]:
//! incoming text pieces are re-split with [`SentenceChunker`], each sentence is synthesised
//! on a dedicated worker thread, and audio chunks are yielded as soon as the core produces
//! them. The first chunk is therefore available before the text stream has ended.
//!
//! Dropping the output stream closes the channels; the worker notices and stops (cancel).
//! Errors carry fixed text only: never the text being spoken or audio.

use crate::{AudioChunk, SentenceChunker, TtsOptions, TtsProvider, VoiceError};
use async_trait::async_trait;
use futures::channel::mpsc;
use futures::stream::BoxStream;
use futures::{SinkExt, StreamExt};
use std::collections::VecDeque;
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;

/// Iterator of PCM pieces (mono, f32) for one sentence.
pub type PcmIter = Box<dyn Iterator<Item = Result<Vec<f32>, VoiceError>> + Send>;

/// Synthesis of a single sentence. Blocking; always called from a worker thread.
pub trait TtsCore: Send + Sync + 'static {
    fn sample_rate(&self) -> u32;
    fn synth(&self, sentence: &str, voice: Option<&str>) -> Result<PcmIter, VoiceError>;
}

pub struct StreamingTts<C: TtsCore> {
    id: String,
    core: Arc<C>,
}

impl<C: TtsCore> StreamingTts<C> {
    pub fn new(id: impl Into<String>, core: C) -> Self {
        Self {
            id: id.into(),
            core: Arc::new(core),
        }
    }
}

enum Msg {
    Chunk(Result<AudioChunk, VoiceError>),
    SentenceDone,
}

struct State {
    text: BoxStream<'static, String>,
    chunker: SentenceChunker,
    pending: VecDeque<String>,
    text_done: bool,
    active: bool,
    failed: bool,
    req: Option<std_mpsc::Sender<String>>,
    rx: mpsc::Receiver<Msg>,
}

fn worker<C: TtsCore>(
    core: Arc<C>,
    voice: Option<String>,
    req: std_mpsc::Receiver<String>,
    mut tx: mpsc::Sender<Msg>,
) {
    let rate = core.sample_rate();
    // `req` disconnects when the output stream is dropped.
    while let Ok(sentence) = req.recv() {
        match core.synth(&sentence, voice.as_deref()) {
            Ok(iter) => {
                for piece in iter {
                    let msg = match piece {
                        Ok(s) if s.is_empty() => continue,
                        Ok(samples) => Ok(AudioChunk {
                            samples,
                            sample_rate: rate,
                        }),
                        Err(e) => Err(e),
                    };
                    let is_err = msg.is_err();
                    // Bounded channel gives back-pressure; Err = consumer gone -> cancel.
                    if futures::executor::block_on(tx.send(Msg::Chunk(msg))).is_err() {
                        return;
                    }
                    if is_err {
                        break;
                    }
                }
            }
            Err(e) => {
                if futures::executor::block_on(tx.send(Msg::Chunk(Err(e)))).is_err() {
                    return;
                }
            }
        }
        if futures::executor::block_on(tx.send(Msg::SentenceDone)).is_err() {
            return;
        }
    }
}

#[async_trait]
impl<C: TtsCore> TtsProvider for StreamingTts<C> {
    fn id(&self) -> &str {
        &self.id
    }

    async fn synthesize(
        &self,
        text: BoxStream<'static, String>,
        opts: TtsOptions,
    ) -> Result<BoxStream<'static, Result<AudioChunk, VoiceError>>, VoiceError> {
        let (req_tx, req_rx) = std_mpsc::channel::<String>();
        let (tx, rx) = mpsc::channel::<Msg>(4);
        let core = Arc::clone(&self.core);
        std::thread::Builder::new()
            .name("hey-buddy-tts".into())
            .spawn(move || worker(core, opts.voice, req_rx, tx))
            .map_err(|_| VoiceError::Backend("tts: cannot start worker".into()))?;
        let state = State {
            text,
            chunker: SentenceChunker::new(),
            pending: VecDeque::new(),
            text_done: false,
            active: false,
            failed: false,
            req: Some(req_tx),
            rx,
        };
        let out = futures::stream::unfold(state, |mut st| async move {
            loop {
                if st.failed {
                    return None;
                }
                if st.active {
                    match st.rx.next().await {
                        Some(Msg::Chunk(r)) => {
                            st.failed = r.is_err();
                            return Some((r, st));
                        }
                        Some(Msg::SentenceDone) => st.active = false,
                        None => {
                            st.failed = true;
                            let e = VoiceError::Backend("tts: worker stopped".into());
                            return Some((Err(e), st));
                        }
                    }
                } else if let Some(s) = st.pending.pop_front() {
                    let sent = st.req.as_ref().is_some_and(|r| r.send(s).is_ok());
                    if !sent {
                        st.failed = true;
                        let e = VoiceError::Backend("tts: worker stopped".into());
                        return Some((Err(e), st));
                    }
                    st.active = true;
                } else if st.text_done {
                    return None;
                } else {
                    match st.text.next().await {
                        Some(t) => {
                            let done = st.chunker.push(&t);
                            st.pending.extend(done);
                        }
                        None => {
                            st.text_done = true;
                            if let Some(s) = st.chunker.finish() {
                                st.pending.push_back(s);
                            }
                        }
                    }
                }
            }
        });
        Ok(Box::pin(out))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use futures::channel::mpsc as fmpsc;
    use std::sync::Mutex;
    use std::time::Duration;

    /// Stub core: each sentence yields two pieces; records the sentences it was given.
    pub struct StubCore {
        pub seen: Arc<Mutex<Vec<String>>>,
    }

    impl TtsCore for StubCore {
        fn sample_rate(&self) -> u32 {
            24_000
        }
        fn synth(&self, s: &str, _voice: Option<&str>) -> Result<PcmIter, VoiceError> {
            if s.contains("FAIL") {
                return Err(VoiceError::Backend("stub failure".into()));
            }
            self.seen.lock().unwrap().push(s.to_string());
            let n = s.len();
            Ok(Box::new((0..2).map(move |i| Ok(vec![i as f32; n]))))
        }
    }

    fn tts() -> (StreamingTts<StubCore>, Arc<Mutex<Vec<String>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        (
            StreamingTts::new("stub", StubCore { seen: seen.clone() }),
            seen,
        )
    }

    #[tokio::test]
    async fn first_chunk_arrives_before_text_ends() {
        let (t, seen) = tts();
        let (mut text_tx, text_rx) = fmpsc::unbounded::<String>();
        // Stream stays open: no end-of-text yet.
        text_tx.send("Hello there. Still typ".into()).await.unwrap();
        let mut audio = t
            .synthesize(Box::pin(text_rx), TtsOptions::default())
            .await
            .unwrap();
        let first = tokio::time::timeout(Duration::from_secs(5), audio.next())
            .await
            .expect("first chunk must not wait for end of text")
            .unwrap()
            .unwrap();
        assert_eq!(first.sample_rate, 24_000);
        assert_eq!(first.samples.len(), "Hello there.".len());
        // The unfinished sentence has not been synthesised.
        let second = audio.next().await.unwrap().unwrap();
        assert!(!second.samples.is_empty());
        assert!(
            tokio::time::timeout(Duration::from_millis(100), audio.next())
                .await
                .is_err(),
            "must wait for more text"
        );
        assert_eq!(*seen.lock().unwrap(), vec!["Hello there."]);
        // Finish the text: the partial sentence is flushed.
        text_tx.send("ing...".into()).await.unwrap();
        drop(text_tx);
        let rest: Vec<_> = audio.collect().await;
        assert_eq!(rest.len(), 2);
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn error_is_terminal_and_fixed_text() {
        let (t, _) = tts();
        let text = futures::stream::iter(vec!["FAIL secret words. Next one.".to_string()]);
        let mut audio = t
            .synthesize(Box::pin(text), TtsOptions::default())
            .await
            .unwrap();
        let e = audio.next().await.unwrap().unwrap_err();
        assert!(!e.to_string().contains("secret"));
        assert!(audio.next().await.is_none());
    }

    #[tokio::test]
    async fn dropping_output_stops_worker() {
        let (t, _) = tts();
        let (_text_tx, text_rx) = fmpsc::unbounded::<String>();
        let audio = t
            .synthesize(Box::pin(text_rx), TtsOptions::default())
            .await
            .unwrap();
        drop(audio); // must not hang or panic
    }
}
