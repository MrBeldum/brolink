//! PCM playback via cpal.
//!
//! Packets arrive as 48 kHz signed-16 stereo. The output device is often *not*
//! 48 kHz (44.1 kHz is the default on plenty of hardware), so we ask for a
//! 48 kHz stream when the device supports one and resample when it does not.
//! Playing 48 kHz samples out of a 44.1 kHz device shifts the pitch and drifts
//! about a second per eleven seconds of audio, which sounds like the stream
//! slowly falling apart.

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// The rate the host always sends at.
const SOURCE_RATE: u32 = 48_000;
/// Build up this much audio before playing, so ordinary jitter does not
/// underrun the device on the very first callback.
const PREBUFFER_MS: usize = 40;
/// Never let the queue grow past this — latency matters more than every sample.
const MAX_BUFFER_MS: usize = 120;

fn frames_for(ms: usize) -> usize {
    SOURCE_RATE as usize * ms / 1000
}

/// Counters for the audio path.
///
/// Without these a silent stream and a broken one look identical: the client
/// logs "audio out: 48000 Hz" either way, and the loopback test only ever
/// counted video. `frames_out` is the honest signal -- it only moves when
/// buffered PCM is actually handed to the output device.
#[derive(Debug, Default)]
pub struct AudioStats {
    packets: AtomicU64,
    frames_in: AtomicU64,
    frames_out: AtomicU64,
    peak: AtomicU64,
}

impl AudioStats {
    /// One audio packet came off the wire. Counted even when no output device
    /// opened, so "host sent nothing" stays distinguishable from "no speakers".
    pub fn packet(&self) {
        self.packets.fetch_add(1, Ordering::Relaxed);
    }

    pub fn packets(&self) -> u64 {
        self.packets.load(Ordering::Relaxed)
    }

    /// PCM frames accepted into the playback buffer.
    pub fn frames_in(&self) -> u64 {
        self.frames_in.load(Ordering::Relaxed)
    }

    /// PCM frames handed to the output device.
    pub fn frames_out(&self) -> u64 {
        self.frames_out.load(Ordering::Relaxed)
    }

    /// Loudest sample seen, 0..=32768.
    ///
    /// Frame counts alone cannot tell working audio from a silent stream:
    /// WASAPI loopback reports silent buffers rather than stopping, and the
    /// host faithfully forwards those as zeroes. Only a non-zero peak proves
    /// something audible was captured and made it across.
    pub fn peak(&self) -> u64 {
        self.peak.load(Ordering::Relaxed)
    }
}

/// Interleaved stereo frames at [`SOURCE_RATE`], read back at the device rate.
struct Playback {
    frames: VecDeque<[i16; 2]>,
    /// Fractional read position measured from the front of `frames`.
    pos: f64,
    /// Input frames consumed per output frame (`48000 / device_rate`).
    step: f64,
    volume: f32,
    /// Cleared on underrun so playback re-buffers instead of stuttering.
    playing: bool,
    stats: Arc<AudioStats>,
}

impl Playback {
    fn new(device_rate: u32, volume: f32, stats: Arc<AudioStats>) -> Self {
        Self {
            frames: VecDeque::with_capacity(frames_for(MAX_BUFFER_MS) + 1),
            pos: 0.0,
            step: SOURCE_RATE as f64 / device_rate.max(1) as f64,
            volume: volume.clamp(0.0, 2.0),
            playing: false,
            stats,
        }
    }

    fn push(&mut self, pcm: &[u8]) {
        // Whole stereo frames only: a truncated tail is discarded rather than
        // shifting every later sample by two bytes.
        let whole = pcm.as_chunks::<4>().0;
        let mut peak = 0u64;
        for s in whole {
            let l = i16::from_le_bytes([s[0], s[1]]);
            let r = i16::from_le_bytes([s[2], s[3]]);
            // unsigned_abs: i16::MIN has no positive counterpart.
            peak = peak
                .max(l.unsigned_abs() as u64)
                .max(r.unsigned_abs() as u64);
            self.frames.push_back([l, r]);
        }
        self.stats
            .frames_in
            .fetch_add(whole.len() as u64, Ordering::Relaxed);
        self.stats.peak.fetch_max(peak, Ordering::Relaxed);
        // Trim from the front rather than clearing: dumping the whole queue
        // turns a small overrun into an audible dropout, and then the next
        // packet underruns again.
        if self.frames.len() > frames_for(MAX_BUFFER_MS) {
            let excess = self.frames.len() - frames_for(PREBUFFER_MS);
            self.frames.drain(..excess);
            self.pos = 0.0;
        }
        if !self.playing && self.frames.len() >= frames_for(PREBUFFER_MS) {
            self.playing = true;
        }
    }

    /// One output frame as normalised left/right, or `None` while starved.
    fn pull(&mut self) -> Option<[f32; 2]> {
        if !self.playing {
            return None;
        }
        while self.pos >= 1.0 && self.frames.len() >= 2 {
            self.frames.pop_front();
            self.pos -= 1.0;
        }
        if self.frames.len() < 2 {
            // Wait for more audio before making noise again.
            self.playing = false;
            return None;
        }
        let a = self.frames[0];
        let b = self.frames[1];
        let t = self.pos as f32;
        let lerp = |x: i16, y: i16| (x as f32 + (y as f32 - x as f32) * t) / 32768.0 * self.volume;
        let out = [lerp(a[0], b[0]), lerp(a[1], b[1])];
        self.pos += self.step;
        Some(out)
    }

    fn fill(&mut self, data: &mut [f32], channels: usize) {
        // Counted once per callback rather than per frame: this runs on the
        // realtime audio thread, where 48k atomic increments a second is work
        // for nothing.
        let mut emitted = 0u64;
        for frame in data.chunks_mut(channels.max(1)) {
            let pulled = self.pull();
            emitted += u64::from(pulled.is_some());
            let [l, r] = pulled.unwrap_or([0.0, 0.0]);
            match frame.len() {
                0 => {}
                1 => frame[0] = (l + r) * 0.5,
                _ => {
                    frame[0] = l;
                    frame[1] = r;
                    for s in frame.iter_mut().skip(2) {
                        *s = 0.0;
                    }
                }
            }
        }
        self.stats.frames_out.fetch_add(emitted, Ordering::Relaxed);
    }
}

fn to_i16(v: f32) -> i16 {
    (v.clamp(-1.0, 1.0) * 32767.0) as i16
}

pub struct AudioPlayer {
    _stream: cpal::Stream,
    playback: Arc<Mutex<Playback>>,
}

impl AudioPlayer {
    /// `volume` is the linear gain from `ClientConfig::volume`; 0 mutes.
    /// A muted player still counts frames, so a test can prove the path works
    /// without the client's own output feeding back into the host's capture.
    pub fn start(volume: f32, stats: Arc<AudioStats>) -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| anyhow!("no audio output device"))?;
        let cfg = pick_output_config(&device)?;
        let rate = cfg.sample_rate().0;
        tracing::info!(
            "audio out: {} Hz, {} ch, {:?}{}",
            rate,
            cfg.channels(),
            cfg.sample_format(),
            if rate == SOURCE_RATE {
                ""
            } else {
                " (resampling)"
            }
        );

        let playback = Arc::new(Mutex::new(Playback::new(rate, volume, stats)));
        let channels = cfg.channels() as usize;
        let for_cb = playback.clone();
        let err_fn = |e| tracing::warn!("audio out: {e}");
        let stream = match cfg.sample_format() {
            cpal::SampleFormat::F32 => device.build_output_stream(
                &cfg.config(),
                move |data: &mut [f32], _: &_| for_cb.lock().fill(data, channels),
                err_fn,
                None,
            )?,
            cpal::SampleFormat::I16 => {
                let mut scratch: Vec<f32> = Vec::new();
                device.build_output_stream(
                    &cfg.config(),
                    move |data: &mut [i16], _: &_| {
                        scratch.clear();
                        scratch.resize(data.len(), 0.0);
                        for_cb.lock().fill(&mut scratch, channels);
                        for (dst, src) in data.iter_mut().zip(&scratch) {
                            *dst = to_i16(*src);
                        }
                    },
                    err_fn,
                    None,
                )?
            }
            cpal::SampleFormat::U16 => {
                let mut scratch: Vec<f32> = Vec::new();
                device.build_output_stream(
                    &cfg.config(),
                    move |data: &mut [u16], _: &_| {
                        scratch.clear();
                        scratch.resize(data.len(), 0.0);
                        for_cb.lock().fill(&mut scratch, channels);
                        for (dst, src) in data.iter_mut().zip(&scratch) {
                            *dst = (to_i16(*src) as i32 + 32768) as u16;
                        }
                    },
                    err_fn,
                    None,
                )?
            }
            other => anyhow::bail!("unsupported audio output format {other:?}"),
        };
        stream.play()?;
        Ok(Self {
            _stream: stream,
            playback,
        })
    }

    pub fn push_s16_48k_stereo(&self, pcm: &[u8]) {
        self.playback.lock().push(pcm);
    }
}

/// Prefer a 48 kHz stream so no resampling is needed at all.
fn pick_output_config(device: &cpal::Device) -> Result<cpal::SupportedStreamConfig> {
    let default = device.default_output_config()?;
    if default.sample_rate().0 == SOURCE_RATE {
        return Ok(default);
    }
    let Ok(ranges) = device.supported_output_configs() else {
        return Ok(default);
    };
    let native = ranges
        .filter(|r| {
            matches!(
                r.sample_format(),
                cpal::SampleFormat::F32 | cpal::SampleFormat::I16 | cpal::SampleFormat::U16
            ) && r.min_sample_rate().0 <= SOURCE_RATE
                && r.max_sample_rate().0 >= SOURCE_RATE
        })
        // Two channels is what we actually have; more just wastes work.
        .min_by_key(|r| (r.channels() as i32 - 2).abs());
    match native {
        Some(r) => Ok(r.with_sample_rate(cpal::SampleRate(SOURCE_RATE))),
        None => Ok(default),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pb(rate: u32, volume: f32) -> Playback {
        Playback::new(rate, volume, Arc::new(AudioStats::default()))
    }

    fn pcm(frames: &[(i16, i16)]) -> Vec<u8> {
        let mut v = Vec::new();
        for (l, r) in frames {
            v.extend_from_slice(&l.to_le_bytes());
            v.extend_from_slice(&r.to_le_bytes());
        }
        v
    }

    fn silence(n: usize) -> Vec<u8> {
        pcm(&vec![(0, 0); n])
    }

    #[test]
    fn playback_waits_for_the_prebuffer_before_making_noise() {
        let mut p = pb(48_000, 1.0);
        p.push(&silence(10));
        assert!(p.pull().is_none(), "10 frames is far below the prebuffer");
        p.push(&silence(frames_for(PREBUFFER_MS)));
        assert!(p.pull().is_some());
    }

    #[test]
    fn matching_rates_play_back_sample_for_sample() {
        let mut p = pb(48_000, 1.0);
        let n = frames_for(PREBUFFER_MS) + 8;
        let input: Vec<(i16, i16)> = (0..n).map(|i| (i as i16 * 4, -(i as i16) * 4)).collect();
        p.push(&pcm(&input));
        for (i, expect) in input.iter().enumerate().take(16) {
            let [l, r] = p.pull().expect("buffered");
            assert!(
                (l - expect.0 as f32 / 32768.0).abs() < 1e-6,
                "frame {i}: {l} vs {}",
                expect.0
            );
            assert!((r - expect.1 as f32 / 32768.0).abs() < 1e-6);
        }
    }

    #[test]
    fn a_44100_device_consumes_more_source_frames_than_it_emits() {
        let mut p = pb(44_100, 1.0);
        // Stay under MAX_BUFFER_MS so the overrun trim does not skew the count.
        let start = 3000;
        p.push(&silence(start));
        for _ in 0..1000 {
            p.pull().expect("buffered");
        }
        // 1000 output frames at 44.1 kHz is ~1088 source frames of audio.
        let consumed = start - p.frames.len();
        assert!(
            (1080..=1092).contains(&consumed),
            "consumed {consumed}, expected ~1088"
        );
    }

    #[test]
    fn a_96000_device_stretches_the_source_across_more_output_frames() {
        let mut p = pb(96_000, 1.0);
        let start = frames_for(PREBUFFER_MS) * 2;
        p.push(&silence(start));
        for _ in 0..1000 {
            p.pull().expect("buffered");
        }
        let consumed = start - p.frames.len();
        assert!((495..=505).contains(&consumed), "consumed {consumed}");
    }

    #[test]
    fn interpolation_lands_between_neighbouring_samples() {
        // Half rate: every other output frame sits midway between two inputs.
        let mut p = pb(96_000, 1.0);
        let lead = frames_for(PREBUFFER_MS);
        let mut input = vec![(0i16, 0i16); lead];
        input.push((1000, 1000));
        input.push((1000, 1000));
        p.push(&pcm(&input));
        // Step to the boundary between the last zero and the first 1000.
        for _ in 0..(lead - 1) * 2 {
            p.pull().expect("buffered");
        }
        let [l, _] = p.pull().expect("buffered");
        let midpoint = 500.0 / 32768.0;
        assert!(
            (l - midpoint).abs() < 0.02,
            "expected roughly the midpoint, got {l}"
        );
    }

    #[test]
    fn volume_scales_the_output() {
        let n = frames_for(PREBUFFER_MS) + 4;
        let mut p = pb(48_000, 0.5);
        p.push(&pcm(&vec![(16384, 16384); n]));
        let [l, r] = p.pull().expect("buffered");
        assert!((l - 0.25).abs() < 1e-3, "{l}");
        assert!((r - 0.25).abs() < 1e-3);

        let mut muted = pb(48_000, 0.0);
        muted.push(&pcm(&vec![(32000, 32000); n]));
        assert_eq!(muted.pull(), Some([0.0, 0.0]));
    }

    #[test]
    fn a_backlog_is_trimmed_instead_of_thrown_away() {
        let mut p = pb(48_000, 1.0);
        p.push(&silence(frames_for(MAX_BUFFER_MS) * 3));
        assert_eq!(
            p.frames.len(),
            frames_for(PREBUFFER_MS),
            "trimmed back to the prebuffer, not emptied"
        );
        assert!(p.pull().is_some(), "audio keeps playing across a trim");
    }

    #[test]
    fn underrun_stops_playback_until_more_audio_arrives() {
        let mut p = pb(48_000, 1.0);
        let n = frames_for(PREBUFFER_MS);
        p.push(&silence(n));
        for _ in 0..n {
            if p.pull().is_none() {
                break;
            }
        }
        assert!(!p.playing, "starving flips playback back to buffering");
        assert!(p.pull().is_none());
        p.push(&silence(n));
        assert!(p.pull().is_some(), "recovers once refilled");
    }

    #[test]
    fn odd_length_packets_do_not_panic() {
        let mut p = pb(48_000, 1.0);
        p.push(&[1, 2, 3]);
        p.push(&[]);
        p.push(&[1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(p.frames.len(), 1, "only whole stereo frames are taken");
    }

    #[test]
    fn fill_writes_every_channel_layout() {
        let mut p = pb(48_000, 1.0);
        p.push(&pcm(&vec![(32767, -32768); frames_for(PREBUFFER_MS) + 64]));

        let mut mono = [0.0f32; 8];
        p.fill(&mut mono, 1);
        // Left and right very nearly cancel when folded down to one channel.
        assert!(mono.iter().all(|s| s.abs() < 0.01), "{mono:?}");

        let mut stereo = [0.0f32; 8];
        p.fill(&mut stereo, 2);
        assert!(stereo[0] > 0.9 && stereo[1] < -0.9);

        let mut surround = [1.0f32; 12];
        p.fill(&mut surround, 6);
        assert!(surround[0] > 0.9 && surround[1] < -0.9);
        assert_eq!(&surround[2..6], &[0.0; 4], "extra channels are silenced");
    }

    #[test]
    fn silence_is_emitted_when_nothing_is_buffered() {
        let mut p = pb(48_000, 1.0);
        let mut data = [0.5f32; 16];
        p.fill(&mut data, 2);
        assert_eq!(data, [0.0; 16]);
    }

    #[test]
    fn sample_conversion_saturates() {
        assert_eq!(to_i16(2.0), 32767);
        assert_eq!(to_i16(-2.0), -32767);
        assert_eq!(to_i16(0.0), 0);
    }

    #[test]
    fn stats_count_frames_in_and_out() {
        let stats = Arc::new(AudioStats::default());
        let mut p = Playback::new(48_000, 1.0, stats.clone());
        let frames: Vec<(i16, i16)> = (0..frames_for(PREBUFFER_MS)).map(|_| (100, -100)).collect();
        p.push(&pcm(&frames));
        assert_eq!(stats.frames_in(), frames_for(PREBUFFER_MS) as u64);
        assert_eq!(stats.frames_out(), 0, "nothing pulled yet");

        let mut out = [0.0f32; 64];
        p.fill(&mut out, 2);
        assert_eq!(
            stats.frames_out(),
            32,
            "32 stereo frames in a 64-sample buffer"
        );
    }

    #[test]
    fn peak_separates_real_audio_from_silence() {
        let stats = Arc::new(AudioStats::default());
        let mut p = Playback::new(48_000, 1.0, stats.clone());
        p.push(&pcm(&[(0, 0), (0, 0)]));
        assert_eq!(stats.peak(), 0, "silence is silence, however many frames");
        p.push(&pcm(&[(0, -1234), (500, 0)]));
        assert_eq!(stats.peak(), 1234, "loudest sample across both channels");
        p.push(&pcm(&[(3, 3)]));
        assert_eq!(stats.peak(), 1234, "the peak is a high-water mark");
    }

    #[test]
    fn peak_handles_the_most_negative_sample() {
        let stats = Arc::new(AudioStats::default());
        let mut p = Playback::new(48_000, 1.0, stats.clone());
        p.push(&pcm(&[(i16::MIN, 0)]));
        assert_eq!(stats.peak(), 32768, "i16::MIN must not overflow the abs");
    }

    #[test]
    fn silence_does_not_count_as_played() {
        // The distinction the loopback test rests on: a device happily writing
        // zeroes because nothing arrived must not look like working audio.
        let stats = Arc::new(AudioStats::default());
        let mut p = Playback::new(48_000, 1.0, stats.clone());
        let mut out = [0.0f32; 64];
        p.fill(&mut out, 2);
        assert_eq!(stats.frames_out(), 0);
        assert_eq!(stats.packets(), 0);
        stats.packet();
        assert_eq!(stats.packets(), 1);
    }

    #[test]
    fn muted_playback_still_counts_frames() {
        // The loopback test mutes the client so its own output is not captured
        // back by the host; the counters have to keep working when it does.
        let stats = Arc::new(AudioStats::default());
        let mut p = Playback::new(48_000, 0.0, stats.clone());
        let frames: Vec<(i16, i16)> = (0..frames_for(PREBUFFER_MS))
            .map(|_| (i16::MAX, i16::MIN))
            .collect();
        p.push(&pcm(&frames));
        let mut out = [0.0f32; 64];
        p.fill(&mut out, 2);
        assert_eq!(out, [0.0; 64], "muted output is silent");
        assert_eq!(stats.frames_out(), 32, "but the frames were still consumed");
    }
}
