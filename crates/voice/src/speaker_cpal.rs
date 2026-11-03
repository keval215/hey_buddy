//! cpal speaker output with streaming playback. Feature `speaker-cpal`.
//!
//! `play()` opens the default output device, then a feeder thread pulls chunks from the
//! audio stream and queues them (resampled to the device rate) while the device callback
//! drains the queue: the first chunk is audible before the stream ends. A dedicated owner
//! thread holds the (possibly `!Send`) cpal stream. `stop()` / dropping the handle cancels
//! the feeder (dropping the audio stream, which cancels synthesis), then joins the owner
//! thread, so the device is closed when it returns. Nothing is written to disk or logged;
//! errors carry fixed text only.

use crate::playback::{LinearResampler, PlayQueue};
use crate::{AudioChunk, AudioOutput, PlaybackHandle, VoiceError};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;
use futures::channel::oneshot;
use futures::future::{select, Either};
use futures::stream::BoxStream;
use futures::{FutureExt, StreamExt};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Back-pressure: the feeder waits while more than this much audio is queued.
const MAX_QUEUED_SECS: usize = 3;

#[derive(Debug, Default, Clone, Copy)]
pub struct CpalSpeaker;

impl CpalSpeaker {
    pub fn new() -> Self {
        Self
    }
}

fn dev_err(what: &str) -> VoiceError {
    VoiceError::DeviceUnavailable(what.to_string())
}

type Shared = Arc<Mutex<PlayQueue>>;

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    q: Shared,
) -> Result<cpal::Stream, VoiceError>
where
    T: cpal::SizedSample + cpal::FromSample<f32> + Send + 'static,
{
    let channels = config.channels as usize;
    let mut scratch: Vec<f32> = Vec::new();
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _: &_| {
                scratch.resize(data.len(), 0.0);
                match q.lock() {
                    Ok(mut g) => g.fill(&mut scratch, channels),
                    Err(_) => scratch.iter_mut().for_each(|s| *s = 0.0),
                }
                for (o, s) in data.iter_mut().zip(scratch.iter()) {
                    *o = T::from_sample(*s);
                }
            },
            |_err| {}, // never log device errors
            None,
        )
        .map_err(|_| dev_err("cannot open output stream"))
}

impl AudioOutput for CpalSpeaker {
    fn play(
        &self,
        audio: BoxStream<'static, Result<AudioChunk, VoiceError>>,
    ) -> Result<PlaybackHandle, VoiceError> {
        let queue: Shared = Arc::new(Mutex::new(PlayQueue::new()));
        let stopped = Arc::new(AtomicBool::new(false));
        let (ctl_tx, ctl_rx) = std_mpsc::channel::<()>();
        let (ready_tx, ready_rx) = std_mpsc::channel::<Result<u32, VoiceError>>();
        let (done_tx, done_rx) = oneshot::channel::<()>();

        let q = Arc::clone(&queue);
        let owner = std::thread::Builder::new()
            .name("hey-buddy-speaker".into())
            .spawn(move || {
                let setup = || -> Result<(cpal::Stream, u32), VoiceError> {
                    let device = cpal::default_host()
                        .default_output_device()
                        .ok_or_else(|| dev_err("no output device"))?;
                    let supported = device
                        .default_output_config()
                        .map_err(|_| dev_err("no output config"))?;
                    let fmt = supported.sample_format();
                    let config: cpal::StreamConfig = supported.into();
                    let rate = config.sample_rate;
                    let stream = match fmt {
                        SampleFormat::F32 => build::<f32>(&device, config, q.clone()),
                        SampleFormat::I16 => build::<i16>(&device, config, q.clone()),
                        SampleFormat::I32 => build::<i32>(&device, config, q.clone()),
                        _ => Err(dev_err("unsupported sample format")),
                    }?;
                    stream.play().map_err(|_| dev_err("cannot start output"))?;
                    Ok((stream, rate))
                };
                match setup() {
                    Ok((stream, rate)) => {
                        let _ = ready_tx.send(Ok(rate));
                        // Wait for stop (message or sender dropped) or natural end.
                        while let Err(std_mpsc::RecvTimeoutError::Timeout) =
                            ctl_rx.recv_timeout(Duration::from_millis(20))
                        {
                            if q.lock().map(|g| g.is_drained()).unwrap_or(true) {
                                // Let the device flush its last buffer.
                                std::thread::sleep(Duration::from_millis(150));
                                break;
                            }
                        }
                        drop(stream); // closes the output device
                        let _ = done_tx.send(());
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                    }
                }
            })
            .map_err(|_| dev_err("cannot start speaker thread"))?;

        let rate = match ready_rx.recv() {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                let _ = owner.join();
                return Err(e);
            }
            Err(_) => return Err(dev_err("speaker thread failed")), // fail closed
        };

        // Feeder: drains `audio` into the queue. Cancelled via `cancel_rx`.
        let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();
        let fq = Arc::clone(&queue);
        let fstopped = Arc::clone(&stopped);
        let feeder = std::thread::Builder::new()
            .name("hey-buddy-speaker-feed".into())
            .spawn(move || {
                let max_queued = rate as usize * MAX_QUEUED_SECS;
                let fq2 = Arc::clone(&fq);
                futures::executor::block_on(async move {
                    let fq = fq2;
                    let mut audio = audio;
                    let mut resamplers: Option<(u32, LinearResampler)> = None;
                    loop {
                        let next = audio.next();
                        futures::pin_mut!(next);
                        let item = match select(next, &mut cancel_rx).await {
                            Either::Left((item, _)) => item,
                            Either::Right(_) => break, // stopped: drop `audio` -> cancel
                        };
                        let Some(item) = item else { break };
                        let Ok(chunk) = item else { break }; // fail closed on error
                        let r = match &mut resamplers {
                            Some((sr, r)) if *sr == chunk.sample_rate => r,
                            slot => {
                                &mut slot
                                    .insert((
                                        chunk.sample_rate,
                                        LinearResampler::new(chunk.sample_rate, rate),
                                    ))
                                    .1
                            }
                        };
                        let out = r.process(&chunk.samples);
                        // Wait for room (device is draining), unless stopped.
                        while !fstopped.load(Ordering::Relaxed)
                            && fq.lock().map(|g| g.len() > max_queued).unwrap_or(false)
                        {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        if fstopped.load(Ordering::Relaxed) {
                            break;
                        }
                        if let Ok(mut g) = fq.lock() {
                            g.push(&out);
                        }
                    }
                });
                if let Ok(mut g) = fq.lock() {
                    g.finish();
                }
            })
            .map_err(|_| dev_err("cannot start feeder thread"))?;

        let stop = move || {
            stopped.store(true, Ordering::Relaxed);
            let _ = cancel_tx.send(());
            if let Ok(mut g) = queue.lock() {
                g.finish();
                g.clear();
            }
            drop(ctl_tx); // wakes the owner, which drops the cpal stream
            let _ = feeder.join();
            let _ = owner.join(); // device closed once this returns
        };
        Ok(PlaybackHandle::new(
            stop,
            async move {
                let _ = done_rx.await;
            }
            .boxed(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No speaker in CI: play must either start (and stop cleanly) or fail with
    /// DeviceUnavailable, never panic.
    #[test]
    fn play_fails_closed_or_stops_cleanly() {
        let (_keep, rx) = futures::channel::mpsc::unbounded::<Result<AudioChunk, VoiceError>>();
        match CpalSpeaker::new().play(Box::pin(rx)) {
            Ok(h) => h.stop(), // returns only after the device is closed
            Err(e) => assert!(matches!(e, VoiceError::DeviceUnavailable(_))),
        }
    }
}
