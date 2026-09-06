//! Opus packets in, sound out. Decoding happens on the stream's audio thread;
//! the OS pulls from a ring buffer on its own thread. If the buffer grows past
//! a quarter second the oldest audio is dropped: latency matters more than
//! never skipping.

use anyhow::{bail, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct Player {
    decoder: *mut audiopus_sys::OpusMSDecoder,
    channels: usize,
    samples_per_frame: usize,
    pcm: Vec<f32>,
    queue: Arc<Mutex<VecDeque<f32>>>,
    stop: Arc<AtomicBool>,
    max_queued: usize,
}

// The decoder pointer is only touched from the thread that owns the Player.
unsafe impl Send for Player {}

impl Player {
    pub fn new(
        sample_rate: u32,
        channels: usize,
        streams: i32,
        coupled: i32,
        samples_per_frame: usize,
        mapping: &[u8],
    ) -> Result<Self> {
        let mut err = 0;
        let decoder = unsafe {
            audiopus_sys::opus_multistream_decoder_create(
                sample_rate as i32,
                channels as i32,
                streams,
                coupled,
                mapping.as_ptr(),
                &mut err,
            )
        };
        if decoder.is_null() || err != 0 {
            bail!("Opus decoder failed ({err})");
        }
        let queue: Arc<Mutex<VecDeque<f32>>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        output_thread(sample_rate, channels as u16, queue.clone(), stop.clone());
        Ok(Self {
            decoder,
            channels,
            samples_per_frame,
            pcm: vec![0.0; samples_per_frame * channels * 6],
            queue,
            stop,
            max_queued: sample_rate as usize * channels / 4,
        })
    }

    /// Decode one packet. An empty slice means the packet was lost; Opus
    /// then synthesises what it can.
    pub fn push(&mut self, packet: &[u8]) {
        let frame = self.pcm.len() / self.channels;
        let n = unsafe {
            audiopus_sys::opus_multistream_decode_float(
                self.decoder,
                if packet.is_empty() {
                    std::ptr::null()
                } else {
                    packet.as_ptr()
                },
                packet.len() as i32,
                self.pcm.as_mut_ptr(),
                if packet.is_empty() {
                    self.samples_per_frame as i32
                } else {
                    frame as i32
                },
                0,
            )
        };
        if n <= 0 {
            return;
        }
        let samples = &self.pcm[..n as usize * self.channels];
        let mut q = self.queue.lock();
        q.extend(samples);
        if q.len() > self.max_queued {
            let drop = q.len() - self.max_queued / 2;
            q.drain(..drop);
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        unsafe { audiopus_sys::opus_multistream_decoder_destroy(self.decoder) };
    }
}

/// cpal streams are not `Send` everywhere, so the output stream lives on a
/// thread of its own for as long as the player does.
fn output_thread(
    sample_rate: u32,
    channels: u16,
    queue: Arc<Mutex<VecDeque<f32>>>,
    stop: Arc<AtomicBool>,
) {
    std::thread::Builder::new()
        .name("audio-out".into())
        .spawn(move || {
            let Some(device) = cpal::default_host().default_output_device() else {
                tracing::warn!("no audio output device");
                return;
            };
            let config = cpal::StreamConfig {
                channels,
                sample_rate,
                buffer_size: cpal::BufferSize::Default,
            };
            let q = queue.clone();
            let stream = device.build_output_stream(
                config,
                move |out: &mut [f32], _| {
                    let mut q = q.lock();
                    for s in out.iter_mut() {
                        *s = q.pop_front().unwrap_or(0.0);
                    }
                },
                |e| tracing::warn!("audio output: {e}"),
                None,
            );
            let stream = match stream {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("audio output unavailable: {e}");
                    return;
                }
            };
            if let Err(e) = stream.play() {
                tracing::warn!("audio output: {e}");
                return;
            }
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        })
        .expect("spawn audio thread");
}
