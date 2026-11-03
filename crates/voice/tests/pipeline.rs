#![cfg(feature = "fakes")]
//! End-to-end voice pipeline using only fakes (no network, no models, no devices).

use futures::channel::mpsc;
use futures::stream::{self, BoxStream, StreamExt};
use std::time::Duration;
use voice::fake::*;
use voice::*;

fn tone(n: usize) -> Vec<f32> {
    (0..n).map(|i| 0.5 * (i as f32 * 0.1).sin()).collect()
}

fn mic_audio() -> Vec<AudioChunk> {
    let mut chunks = Vec::new();
    let mut push = |s: Vec<f32>| {
        chunks.push(AudioChunk {
            samples: s,
            sample_rate: 16_000,
        })
    };
    push(vec![0.0; 512 * 3]);
    push(tone(512 * 5));
    push(vec![0.0; 512 * 20]);
    chunks
}

/// Run VAD windows through the segmenter; return the audio between SpeechStart and SpeechEnd.
fn segment(chunks: Vec<AudioChunk>) -> Vec<AudioChunk> {
    let mut vad: Box<dyn Vad> = Box::new(FakeVad::new());
    let mut seg = Segmenter::new(SegmenterConfig {
        hangover_windows: 4,
        ..Default::default()
    });
    let w = vad.window_len();
    let all: Vec<f32> = chunks.into_iter().flat_map(|c| c.samples).collect();
    let mut out = Vec::new();
    let mut active = false;
    for win in all.chunks_exact(w) {
        match seg.push(vad.speech_prob(win).unwrap()) {
            Some(SegmentEvent::SpeechStart) => active = true,
            Some(SegmentEvent::SpeechEnd) => break,
            None => {}
        }
        if active {
            out.push(AudioChunk {
                samples: win.to_vec(),
                sample_rate: 16_000,
            });
        }
    }
    out
}

#[tokio::test]
async fn mic_to_speaker_streams_first_audio_before_text_ends() {
    let mic: Box<dyn AudioInput> = Box::new(FakeAudioIn::new(mic_audio()));
    let stt: Box<dyn SttProvider> = Box::new(FakeStt::new("what is on my screen"));
    let tts: Box<dyn TtsProvider> = Box::new(FakeTts);
    let out = FakeAudioOut::new();

    // mic -> segmenter/vad
    let chunks: Vec<AudioChunk> = mic.open_push_to_talk().unwrap().collect().await;
    let speech = segment(chunks);
    assert!(!speech.is_empty());
    assert!(speech.len() < 28, "silence must be trimmed");

    // -> stt
    let mut events = stt
        .transcribe(stream::iter(speech).boxed(), SttOptions::default())
        .await
        .unwrap();
    let mut transcript = None;
    while let Some(ev) = events.next().await {
        if let SttEvent::Final(t) = ev.unwrap() {
            transcript = Some(t.text);
        }
    }
    assert_eq!(transcript.as_deref(), Some("what is on my screen"));

    // -> "LLM" deltas (held open by a channel) -> sentence chunker -> tts -> audio out
    let (tx, rx) = mpsc::unbounded::<String>();
    let sentences: BoxStream<'static, String> = {
        let mut chunker = SentenceChunker::new();
        rx.flat_map(move |d| stream::iter(chunker.push(&d))).boxed()
    };
    let mut audio = tts
        .synthesize(sentences, TtsOptions::default())
        .await
        .unwrap();

    tx.unbounded_send("It looks like a terminal. ".into())
        .unwrap();
    // The text stream is still open; the first audio must already be available.
    let first = tokio::time::timeout(Duration::from_secs(5), audio.next())
        .await
        .expect("first audio before text stream ended")
        .unwrap()
        .unwrap();
    assert!(!first.samples.is_empty());
    assert_eq!(first.sample_rate, FakeTts::SAMPLE_RATE);
    assert!(!tx.is_closed());

    // Play everything through the fake speaker.
    let handle = out
        .play(stream::once(async move { Ok(first) }).chain(audio).boxed())
        .unwrap();
    tx.unbounded_send("Anything else? ".into()).unwrap();
    drop(tx);
    handle.finished().await;
    assert_eq!(out.recorded().len(), 2);
}

#[tokio::test]
async fn dropping_playback_handle_stops_playback() {
    let out = FakeAudioOut::new();
    let (tx, rx) = mpsc::unbounded::<Result<AudioChunk, VoiceError>>();
    let h = out.play(rx.boxed()).unwrap();
    tx.unbounded_send(Ok(AudioChunk {
        samples: vec![0.0; 4],
        sample_rate: 24_000,
    }))
    .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    h.stop();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let n = out.recorded().len();
    let _ = tx.unbounded_send(Ok(AudioChunk {
        samples: vec![0.0; 4],
        sample_rate: 24_000,
    }));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(out.recorded().len(), n);
    assert_eq!(n, 1);
}

#[tokio::test]
async fn unavailable_mic_errors() {
    let mic: Box<dyn AudioInput> = Box::new(FakeAudioIn::unavailable());
    assert!(matches!(
        mic.open_push_to_talk(),
        Err(VoiceError::DeviceUnavailable(_))
    ));
}

#[test]
fn registry_error_converts() {
    let e: VoiceError = registry::RegistryError::NotFound("m".into()).into();
    assert!(matches!(e, VoiceError::Backend(_)));
}
