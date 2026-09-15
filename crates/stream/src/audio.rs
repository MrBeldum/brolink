//! Opus packets in, sound out. Decoding happens on the stream's audio
//! thread; the OS pulls from a ring buffer on its own thread, resampled to
//! whatever rate the output device runs at and spread over its channels.
//! If the buffer grows past a fifth of a second the oldest audio is
//! dropped: latency matters more than never skipping. What the output side
//! found is kept, so "no sound" comes with its reason.

use anyhow::{bail, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// Queued sound beyond this is cut back to [`KEEP_MS`].
const MAX_QUEUED_MS: usize = 200;
const KEEP_MS: usize = 80;
/// Opus never carries more channels than this
/// (`AUDIO_CONFIGURATION_MAX_CHANNEL_COUNT` in moonlight-common-c).
const MAX_CHANNELS: usize = 8;

/// The output device, as the output thread found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    Opening,
    Playing {
        device: String,
        rate: u32,
        channels: u16,
    },
    /// No device, or the device refused the stream; the reason.
    Failed(String),
}

pub struct Player {
    decoder: *mut audiopus_sys::OpusMSDecoder,
    channels: usize,
    samples_per_frame: usize,
    pcm: Vec<f32>,
    /// Interleaved samples at the stream's rate and channel count.
    queue: Arc<Mutex<VecDeque<f32>>>,
    stop: Arc<AtomicBool>,
    output: Arc<Mutex<Output>>,
    /// Frames the output device has pulled, silence included: proof that
    /// sound is leaving.
    pulled: Arc<AtomicU64>,
    max_queued: usize,
    keep: usize,
    packets: u64,
    lost: u64,
    decoded: u64,
    undecodable: u64,
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
        if channels == 0 || channels > MAX_CHANNELS || mapping.len() < channels {
            bail!("audio with {channels} channels is not something BroLink plays");
        }
        // Validate before any integer casts, allocation, or call into Opus.
        if !matches!(sample_rate, 8_000 | 12_000 | 16_000 | 24_000 | 48_000) {
            bail!("unsupported Opus sample rate {sample_rate}");
        }
        let max_frame = sample_rate as usize * 120 / 1000;
        let frame_step = sample_rate as usize / 400; // Opus uses 2.5 ms units.
        if samples_per_frame == 0
            || samples_per_frame > max_frame
            || !samples_per_frame.is_multiple_of(frame_step)
        {
            bail!("invalid Opus frame size {samples_per_frame}");
        }
        if streams <= 0
            || streams > 255
            || coupled < 0
            || coupled > streams
            || streams + coupled > 255
            || mapping[..channels]
                .iter()
                .any(|&c| c != 255 && i32::from(c) >= streams + coupled)
        {
            bail!("invalid Opus channel mapping");
        }
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
        let output = Arc::new(Mutex::new(Output::Opening));
        let pulled = Arc::new(AtomicU64::new(0));
        output_thread(
            sample_rate,
            channels as u16,
            queue.clone(),
            stop.clone(),
            output.clone(),
            pulled.clone(),
        );
        let per_ms = sample_rate as usize * channels / 1000;
        // Whole frames only: dropping half a frame would swap the channels.
        let keep = per_ms * KEEP_MS / channels * channels;
        Ok(Self {
            decoder,
            channels,
            samples_per_frame,
            // Opus can emit up to 120 ms; a buffer sized only for the
            // negotiated frame would refuse a longer packet outright.
            pcm: vec![0.0; max_frame * channels],
            queue,
            stop,
            output,
            pulled,
            max_queued: per_ms * MAX_QUEUED_MS,
            keep,
            packets: 0,
            lost: 0,
            decoded: 0,
            undecodable: 0,
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
        if packet.is_empty() {
            self.lost += 1;
        } else {
            self.packets += 1;
        }
        if n <= 0 {
            self.undecodable += 1;
            if self.undecodable == 1 {
                tracing::warn!("audio: Opus refused a packet ({n})");
            }
            return;
        }
        self.decoded += n as u64;
        let samples = &self.pcm[..n as usize * self.channels];
        let mut q = self.queue.lock();
        q.extend(samples);
        if q.len() > self.max_queued {
            let drop = q.len() - self.keep;
            q.drain(..drop);
        }
    }

    pub fn output(&self) -> Output {
        self.output.lock().clone()
    }

    /// Packets received from the PC, lost ones not counted.
    pub fn packets(&self) -> u64 {
        self.packets
    }

    /// Samples per channel decoded so far.
    pub fn decoded(&self) -> u64 {
        self.decoded
    }

    /// Frames the output device has taken so far.
    pub fn pulled(&self) -> u64 {
        self.pulled.load(Ordering::Relaxed)
    }

    /// One phrase for the stats line: where sound goes, or why it does not.
    pub fn describe(&self) -> String {
        phrase(&self.output.lock(), self.packets, self.lost, self.pulled())
    }
}

/// The stats-line phrase for an output state. `pulled` is the device's own
/// count: a device that has opened but taken nothing is not playing, however
/// many packets the PC sent.
fn phrase(output: &Output, packets: u64, lost: u64, pulled: u64) -> String {
    match output {
        Output::Failed(e) => format!("no sound: {e}"),
        Output::Opening => "audio: opening the output".into(),
        Output::Playing { device, .. } => {
            if packets == 0 {
                // The PC sends nothing while it is silent.
                "no audio from the PC yet".into()
            } else if pulled == 0 {
                format!("no sound: {device} is not taking audio")
            } else if lost > 0 {
                format!("audio → {device} · {lost} lost")
            } else {
                format!("audio → {device}")
            }
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
/// thread of its own for as long as the player does. The device is opened
/// at its own rate, channel count and sample format and [`Mixer`] converts;
/// asking a device for 48 kHz stereo f32 it does not offer was silence
/// with the reason only in the log.
fn output_thread(
    source_rate: u32,
    source_channels: u16,
    queue: Arc<Mutex<VecDeque<f32>>>,
    stop: Arc<AtomicBool>,
    output: Arc<Mutex<Output>>,
    pulled: Arc<AtomicU64>,
) {
    std::thread::Builder::new()
        .name("audio-out".into())
        .spawn(move || {
            let fail = |why: String| {
                tracing::warn!("audio output: {why}");
                *output.lock() = Output::Failed(why);
            };
            let Some(device) = cpal::default_host().default_output_device() else {
                fail("this Mac has no audio output device".into());
                return;
            };
            let name = {
                let s = device.to_string();
                if s.is_empty() {
                    "the output device".into()
                } else {
                    s
                }
            };
            let supported = match device.default_output_config() {
                Ok(c) => c,
                Err(e) => {
                    fail(format!("{name} has no output configuration: {e}"));
                    return;
                }
            };
            let rate = supported.sample_rate().max(8_000);
            let channels = supported.channels().max(1);
            let config = cpal::StreamConfig {
                channels,
                sample_rate: rate,
                buffer_size: cpal::BufferSize::Default,
            };
            let mixer = || {
                Mixer::new(
                    source_rate,
                    source_channels as usize,
                    rate,
                    channels as usize,
                    queue.clone(),
                    pulled.clone(),
                    source_rate as usize * source_channels as usize * 40 / 1000,
                )
            };
            let err_fn = |e| tracing::warn!("audio output: {e}");
            let stream = match supported.sample_format() {
                cpal::SampleFormat::F32 => {
                    let mut m = mixer();
                    device.build_output_stream(
                        config,
                        move |out: &mut [f32], _| m.fill(out),
                        err_fn,
                        None,
                    )
                }
                cpal::SampleFormat::I16 => {
                    let mut m = mixer();
                    device.build_output_stream(
                        config,
                        move |out: &mut [i16], _| m.fill_i16(out),
                        err_fn,
                        None,
                    )
                }
                cpal::SampleFormat::I32 => {
                    let mut m = mixer();
                    device.build_output_stream(
                        config,
                        move |out: &mut [i32], _| m.fill_i32(out),
                        err_fn,
                        None,
                    )
                }
                other => {
                    // Most leftover formats still accept f32 or i16.
                    let mut m = mixer();
                    match device.build_output_stream(
                        config,
                        move |out: &mut [f32], _| m.fill(out),
                        err_fn,
                        None,
                    ) {
                        Ok(s) => Ok(s),
                        Err(e) => {
                            tracing::warn!("audio output: {other} as f32 failed ({e}); trying i16");
                            let mut m = mixer();
                            device.build_output_stream(
                                config,
                                move |out: &mut [i16], _| m.fill_i16(out),
                                err_fn,
                                None,
                            )
                        }
                    }
                }
            };
            let stream = match stream {
                Ok(s) => s,
                Err(e) => {
                    fail(format!(
                        "{name} refused {rate} Hz, {channels} channels: {e}"
                    ));
                    return;
                }
            };
            if let Err(e) = stream.play() {
                fail(format!("{name} would not start: {e}"));
                return;
            }
            tracing::info!("audio: playing on {name} at {rate} Hz, {channels} channels");
            *output.lock() = Output::Playing {
                device: name,
                rate,
                channels,
            };
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        })
        .expect("spawn audio thread");
}

/// Pulls stream frames from the queue for the device: linear
/// interpolation between rates, left and right onto whatever channels the
/// device has (the mean onto a mono one), silence when the queue is empty.
struct Mixer {
    queue: Arc<Mutex<VecDeque<f32>>>,
    pulled: Arc<AtomicU64>,
    src_channels: usize,
    out_channels: usize,
    /// Source frames per output frame: 48000 / 44100 ≈ 1.088.
    step: f64,
    /// How far from `prev` towards `next` the output is; a whole frame or
    /// more means it is time to pull.
    pos: f64,
    prev: [f32; MAX_CHANNELS],
    next: [f32; MAX_CHANNELS],
    /// Source samples that must be queued before the first audible frame,
    /// so a late first packet is not a click of silence. Zero in tests.
    preroll: usize,
    started: bool,
    scratch: Vec<f32>,
}

impl Mixer {
    fn new(
        src_rate: u32,
        src_channels: usize,
        out_rate: u32,
        out_channels: usize,
        queue: Arc<Mutex<VecDeque<f32>>>,
        pulled: Arc<AtomicU64>,
        preroll: usize,
    ) -> Self {
        Self {
            queue,
            pulled,
            src_channels: src_channels.clamp(1, MAX_CHANNELS),
            out_channels: out_channels.max(1),
            step: src_rate.max(1) as f64 / out_rate.max(1) as f64,
            pos: 1.0,
            prev: [0.0; MAX_CHANNELS],
            next: [0.0; MAX_CHANNELS],
            preroll,
            started: false,
            scratch: Vec::new(),
        }
    }

    fn fill(&mut self, out: &mut [f32]) {
        let mut q = self.queue.lock();
        if !self.started {
            if q.len() < self.preroll {
                drop(q);
                out.fill(0.0);
                return;
            }
            self.started = true;
        }
        let mut frames = 0u64;
        for frame in out.chunks_mut(self.out_channels) {
            while self.pos >= 1.0 {
                self.pos -= 1.0;
                self.prev = self.next;
                self.next = [0.0; MAX_CHANNELS];
                if q.len() >= self.src_channels {
                    for c in 0..self.src_channels {
                        self.next[c] = q.pop_front().unwrap_or(0.0);
                    }
                }
            }
            let t = self.pos as f32;
            let sample = |c: usize| self.prev[c] + (self.next[c] - self.prev[c]) * t;
            for (c, o) in frame.iter_mut().enumerate() {
                *o = if self.out_channels == 1 && self.src_channels > 1 {
                    (0..self.src_channels).map(sample).sum::<f32>() / self.src_channels as f32
                } else if c < self.src_channels {
                    sample(c)
                } else if self.src_channels == 1 {
                    sample(0)
                } else {
                    0.0
                };
            }
            self.pos += self.step;
            frames += 1;
        }
        self.pulled.fetch_add(frames, Ordering::Relaxed);
    }

    fn fill_converted<T>(&mut self, out: &mut [T], conv: impl Fn(f32) -> T) {
        let n = out.len();
        let mut scratch = std::mem::take(&mut self.scratch);
        scratch.resize(n, 0.0);
        self.fill(&mut scratch);
        for (o, s) in out.iter_mut().zip(&scratch) {
            *o = conv(*s);
        }
        self.scratch = scratch;
    }

    fn fill_i16(&mut self, out: &mut [i16]) {
        self.fill_converted(out, |s| (s.clamp(-1.0, 1.0) * 32767.0) as i16);
    }

    fn fill_i32(&mut self, out: &mut [i32]) {
        self.fill_converted(out, |s| (s.clamp(-1.0, 1.0) * 2147483647.0) as i32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn invalid_audio_configuration_is_rejected_before_allocating() {
        for rate in [0, 44_100, u32::MAX] {
            assert!(Player::new(rate, 2, 1, 1, 480, &[0, 1]).is_err());
        }
        for frame in [0, 1, 5_761, usize::MAX] {
            assert!(Player::new(48_000, 2, 1, 1, frame, &[0, 1]).is_err());
        }
        for (streams, coupled, mapping) in [
            (0, 0, [0, 1]),
            (1, 2, [0, 1]),
            (1, 0, [0, 1]),
            (i32::MAX, 1, [0, 1]),
        ] {
            assert!(Player::new(48_000, 2, streams, coupled, 480, &mapping).is_err());
        }
    }

    /// A stereo Opus packet of `frames` samples of a tone, from libopus's
    /// own encoder: what Sunshine sends, minus the network.
    fn tone_packet(frames: usize) -> Vec<u8> {
        let mut err = 0;
        let enc = unsafe {
            audiopus_sys::opus_encoder_create(
                48_000,
                2,
                audiopus_sys::OPUS_APPLICATION_AUDIO,
                &mut err,
            )
        };
        assert!(!enc.is_null() && err == 0, "encoder: {err}");
        let pcm: Vec<f32> = (0..frames)
            .flat_map(|i| {
                let s = (i as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.5;
                [s, s]
            })
            .collect();
        let mut out = vec![0u8; 4000];
        let n = unsafe {
            audiopus_sys::opus_encode_float(
                enc,
                pcm.as_ptr(),
                frames as i32,
                out.as_mut_ptr(),
                out.len() as i32,
            )
        };
        unsafe { audiopus_sys::opus_encoder_destroy(enc) };
        assert!(n > 0, "encode: {n}");
        out.truncate(n as usize);
        out
    }

    #[test]
    fn opus_packets_become_samples_and_the_device_pulls_them() {
        // Sunshine's stereo layout: one coupled stream, channels 0 and 1,
        // 10 ms frames as negotiated on a remote path.
        let mut p = Player::new(48_000, 2, 1, 1, 480, &[0, 1]).unwrap();
        let packet = tone_packet(480);
        for _ in 0..10 {
            p.push(&packet);
        }
        assert_eq!(p.packets(), 10);
        assert_eq!(p.decoded(), 10 * 480);
        // A lost packet is concealed at the stream's frame size.
        p.push(&[]);
        assert_eq!(p.decoded(), 11 * 480);
        assert_eq!(p.undecodable, 0);
        // The queue never grows past the cap, and never by a partial frame.
        for _ in 0..100 {
            p.push(&packet);
        }
        let len = p.queue.lock().len();
        assert!(len <= p.max_queued, "{len} > {}", p.max_queued);
        assert_eq!(len % 2, 0);
        // Where there is a device, the output thread must be pulling.
        let start = Instant::now();
        loop {
            match p.output() {
                Output::Opening if start.elapsed() < Duration::from_secs(5) => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Output::Opening => panic!("the output never opened"),
                Output::Failed(e) => {
                    eprintln!("no output here ({e}); skipping the playback check");
                    break;
                }
                Output::Playing {
                    device,
                    rate,
                    channels,
                } => {
                    assert!(
                        rate >= 8_000 && channels >= 1,
                        "{device}: {rate} Hz, {channels} ch"
                    );
                    let t = Instant::now();
                    while p.pulled() == 0 && t.elapsed() < Duration::from_secs(3) {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    assert!(p.pulled() > 0, "{device} pulled nothing in 3 s");
                    break;
                }
            }
        }
        let desc = p.describe();
        assert!(desc.contains("audio") || desc.contains("sound"), "{desc}");
        assert!(Player::new(48_000, 0, 1, 0, 480, &[]).is_err());
        assert!(Player::new(48_000, 2, 1, 1, 480, &[0]).is_err());
    }

    #[test]
    fn the_phrase_never_names_a_device_that_takes_nothing() {
        let playing = Output::Playing {
            device: "MacBook Pro Speakers".into(),
            rate: 48_000,
            channels: 2,
        };
        assert_eq!(phrase(&playing, 0, 0, 0), "no audio from the PC yet");
        assert_eq!(
            phrase(&playing, 120, 0, 0),
            "no sound: MacBook Pro Speakers is not taking audio"
        );
        assert_eq!(phrase(&playing, 120, 0, 1), "audio → MacBook Pro Speakers");
        assert_eq!(
            phrase(&playing, 120, 3, 500),
            "audio → MacBook Pro Speakers · 3 lost"
        );
        assert_eq!(
            phrase(&Output::Opening, 120, 0, 0),
            "audio: opening the output"
        );
        assert_eq!(
            phrase(&Output::Failed("no output device".into()), 120, 0, 0),
            "no sound: no output device"
        );
    }

    #[test]
    fn the_mixer_resamples_and_maps_channels() {
        // 48 kHz stereo into a 96 kHz mono device: two output frames per
        // source frame, left and right averaged.
        let queue: Arc<Mutex<VecDeque<f32>>> = Arc::default();
        queue
            .lock()
            .extend([1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0]);
        let pulled = Arc::new(AtomicU64::new(0));
        let mut m = Mixer::new(48_000, 2, 96_000, 1, queue.clone(), pulled.clone(), 0);
        let mut out = vec![9.0f32; 8];
        m.fill(&mut out);
        assert_eq!(pulled.load(Ordering::Relaxed), 8);
        // After the ramp in from silence, every sample is the L/R mean.
        assert!(out[4..].iter().all(|s| (s - 0.5).abs() < 1e-6), "{out:?}");
        // An empty queue plays silence rather than stale sound.
        let mut out = vec![9.0f32; 16];
        m.fill(&mut out);
        assert!(out[8..].iter().all(|s| *s == 0.0), "{out:?}");
        // Stereo into a six-channel device at the same rate: L, R, silence.
        let queue: Arc<Mutex<VecDeque<f32>>> = Arc::default();
        queue
            .lock()
            .extend(std::iter::repeat_n([0.25f32, -0.25], 20).flatten());
        let mut m = Mixer::new(48_000, 2, 48_000, 6, queue, Arc::new(AtomicU64::new(0)), 0);
        let mut out = vec![9.0f32; 6 * 10];
        m.fill(&mut out);
        assert_eq!(&out[6 * 9..], &[0.25, -0.25, 0.0, 0.0, 0.0, 0.0]);
    }
}
