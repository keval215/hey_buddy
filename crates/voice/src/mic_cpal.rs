//! cpal microphone input, push-to-talk only. Feature `mic-cpal`.
//!
//! The cpal stream exists only while the returned `BoxStream` is alive. A dedicated thread
//! owns the (possibly `!Send`) cpal stream; dropping the returned stream signals that thread
//! which drops the cpal stream, closing the microphone. No always-on capture.
//! Audio is never written to disk or logged; errors carry fixed text only.

use crate::{resample_to_16k_mono, AudioChunk, AudioInput, VoiceError};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;
use futures::channel::mpsc;
use futures::stream::{BoxStream, Stream};
use std::pin::Pin;
use std::sync::mpsc as std_mpsc;
use std::task::{Context, Poll};

#[derive(Debug, Default, Clone, Copy)]
pub struct CpalMic;

impl CpalMic {
    pub fn new() -> Self {
        Self
    }
}

/// Wraps the receiver; on drop tells the owner thread to close the device.
struct MicStream {
    rx: mpsc::UnboundedReceiver<AudioChunk>,
    stop: Option<std_mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Stream for MicStream {
    type Item = AudioChunk;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<AudioChunk>> {
        Pin::new(&mut self.rx).poll_next(cx)
    }
}

impl Drop for MicStream {
    fn drop(&mut self) {
        drop(self.stop.take()); // disconnect wakes the owner thread
        if let Some(t) = self.thread.take() {
            let _ = t.join(); // mic is closed once this returns
        }
    }
}

fn dev_err(what: &str) -> VoiceError {
    VoiceError::DeviceUnavailable(what.to_string())
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    tx: mpsc::UnboundedSender<AudioChunk>,
) -> Result<cpal::Stream, VoiceError>
where
    T: cpal::SizedSample + Send + 'static,
    f32: cpal::FromSample<T>,
{
    let channels = config.channels;
    let rate = config.sample_rate;
    device
        .build_input_stream(
            config,
            move |data: &[T], _: &_| {
                let samples: Vec<f32> = data.iter().map(|s| s.to_sample::<f32>()).collect();
                let chunk = resample_to_16k_mono(
                    &AudioChunk {
                        samples,
                        sample_rate: rate,
                    },
                    channels,
                );
                let _ = tx.unbounded_send(chunk);
            },
            |_err| {}, // never log: keep device errors out of logs; stream just ends on drop
            None,
        )
        .map_err(|_| dev_err("cannot open input stream"))
}

impl AudioInput for CpalMic {
    fn open_push_to_talk(&self) -> Result<BoxStream<'static, AudioChunk>, VoiceError> {
        let (tx, rx) = mpsc::unbounded();
        let (stop_tx, stop_rx) = std_mpsc::channel::<()>();
        let (ready_tx, ready_rx) = std_mpsc::channel::<Result<(), VoiceError>>();
        let thread = std::thread::Builder::new()
            .name("hey-buddy-mic".into())
            .spawn(move || {
                let setup = || -> Result<cpal::Stream, VoiceError> {
                    let device = cpal::default_host()
                        .default_input_device()
                        .ok_or_else(|| dev_err("no input device"))?;
                    let supported = device
                        .default_input_config()
                        .map_err(|_| dev_err("no input config"))?;
                    let fmt = supported.sample_format();
                    let config: cpal::StreamConfig = supported.into();
                    let stream = match fmt {
                        SampleFormat::F32 => build::<f32>(&device, config, tx),
                        SampleFormat::I16 => build::<i16>(&device, config, tx),
                        SampleFormat::I32 => build::<i32>(&device, config, tx),
                        SampleFormat::I8 => build::<i8>(&device, config, tx),
                        _ => Err(dev_err("unsupported sample format")),
                    }?;
                    stream.play().map_err(|_| dev_err("cannot start input"))?;
                    Ok(stream)
                };
                match setup() {
                    Ok(stream) => {
                        let _ = ready_tx.send(Ok(()));
                        let _ = stop_rx.recv(); // returns on drop of the sender
                        drop(stream); // closes the microphone
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                    }
                }
            })
            .map_err(|_| dev_err("cannot start mic thread"))?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Box::pin(MicStream {
                rx,
                stop: Some(stop_tx),
                thread: Some(thread),
            })),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => Err(dev_err("mic thread failed")), // fail closed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No microphone in CI: opening must either work or fail with DeviceUnavailable,
    /// never panic. We only assert the error type if it fails.
    #[test]
    fn open_fails_closed_or_succeeds() {
        match CpalMic::new().open_push_to_talk() {
            Ok(s) => drop(s), // dropping closes the mic
            Err(e) => assert!(matches!(e, VoiceError::DeviceUnavailable(_))),
        }
    }
}
