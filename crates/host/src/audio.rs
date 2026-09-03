//! WASAPI loopback capture -> 48 kHz stereo s16 PCM packets.

use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

/// The wire format. Everything captured is converted to this before it is sent.
pub const RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
/// 5 ms keeps a PCM stereo packet under the 1200-byte datagram budget.
const FRAME_MS: u32 = 5;
const FRAME_SAMPLES: usize = (RATE as usize) * (FRAME_MS as usize) / 1000; // 240
/// Interleaved i16 values per packet (240 frames x 2 channels).
const FRAME_VALUES: usize = FRAME_SAMPLES * CHANNELS;

pub struct AudioCapture {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

#[derive(Clone)]
pub struct AudioPacket {
    pub timestamp_us: u64,
    pub pcm: Vec<u8>,
}

impl AudioCapture {
    pub fn start(tx: crossbeam_channel::Sender<AudioPacket>) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let thread = std::thread::Builder::new()
            .name("wasapi-loopback".into())
            .spawn(move || {
                if let Err(e) = run(tx, stop2) {
                    tracing::error!("audio capture ended: {e:#}");
                }
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
    }
}

impl Drop for AudioCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Streaming linear resampler to 48 kHz stereo.
///
/// Windows mixes at whatever the endpoint is configured for — 44.1 kHz on most
/// USB headsets, 96 kHz on some DACs. Sending those samples while claiming
/// 48 kHz on the wire makes audio play at the wrong pitch and drift steadily
/// out of sync with the video, so we convert here.
pub struct Resampler {
    ratio: f64,
    frac: f64,
    prev: [f32; CHANNELS],
    started: bool,
}

impl Resampler {
    pub fn new(src_rate: u32) -> Self {
        let src_rate = if src_rate == 0 { RATE } else { src_rate };
        Self {
            ratio: src_rate as f64 / RATE as f64,
            frac: 0.0,
            prev: [0.0; CHANNELS],
            started: false,
        }
    }

    pub fn is_passthrough(&self) -> bool {
        (self.ratio - 1.0).abs() < f64::EPSILON
    }

    /// Feed one source frame, appending any output frames it completes.
    pub fn push_frame(&mut self, cur: [f32; CHANNELS], out: &mut Vec<i16>) {
        if !self.started {
            // Nothing to interpolate from yet; start the line at this sample
            // instead of sliding up from silence.
            self.prev = cur;
            self.started = true;
        }
        while self.frac < 1.0 {
            let t = self.frac as f32;
            for (prev, cur) in self.prev.iter().zip(&cur) {
                out.push(to_i16(prev + (cur - prev) * t));
            }
            self.frac += self.ratio;
        }
        self.frac -= 1.0;
        self.prev = cur;
    }
}

fn to_i16(v: f32) -> i16 {
    // 32767.0 rather than 32768.0 so full-scale +1.0 does not wrap to -32768.
    (v.clamp(-1.0, 1.0) * 32767.0) as i16
}

/// Split a run of interleaved samples into wire-sized packets.
pub struct PacketBuilder {
    acc: Vec<i16>,
    start: std::time::Instant,
}

impl PacketBuilder {
    pub fn new() -> Self {
        Self {
            acc: Vec::with_capacity(FRAME_VALUES * 4),
            start: std::time::Instant::now(),
        }
    }

    pub fn push(&mut self, samples: &[i16], out: &mut Vec<AudioPacket>) {
        self.acc.extend_from_slice(samples);
        while self.acc.len() >= FRAME_VALUES {
            let mut pcm = Vec::with_capacity(FRAME_VALUES * 2);
            for s in self.acc.drain(..FRAME_VALUES) {
                pcm.extend_from_slice(&s.to_le_bytes());
            }
            out.push(AudioPacket {
                timestamp_us: self.start.elapsed().as_micros() as u64,
                pcm,
            });
        }
    }
}

impl Default for PacketBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// How the endpoint hands us samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleKind {
    Float32,
    Int16,
    Int24,
    Int32,
}

impl SampleKind {
    pub fn bytes(self) -> usize {
        match self {
            Self::Int16 => 2,
            Self::Int24 => 3,
            Self::Float32 | Self::Int32 => 4,
        }
    }

    /// Decide from the mix format's tag, sub-format tag, and bit depth.
    ///
    /// `WAVE_FORMAT_EXTENSIBLE` carries the real type in a sub-format GUID
    /// whose first field is the underlying format tag. Guessing "32-bit means
    /// float" instead would turn 32-bit integer endpoints into noise.
    pub fn detect(format_tag: u16, sub_format_tag: Option<u32>, bits: u16) -> Option<Self> {
        const WAVE_FORMAT_PCM: u16 = 1;
        const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
        const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

        let effective = if format_tag == WAVE_FORMAT_EXTENSIBLE {
            sub_format_tag? as u16
        } else {
            format_tag
        };
        match (effective, bits) {
            (WAVE_FORMAT_IEEE_FLOAT, 32) => Some(Self::Float32),
            (WAVE_FORMAT_PCM, 16) => Some(Self::Int16),
            (WAVE_FORMAT_PCM, 24) => Some(Self::Int24),
            (WAVE_FORMAT_PCM, 32) => Some(Self::Int32),
            _ => None,
        }
    }
}

/// Read one channel's sample as -1.0..=1.0.
fn sample_at(bytes: &[u8], kind: SampleKind) -> f32 {
    match kind {
        SampleKind::Float32 => f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        SampleKind::Int16 => i16::from_le_bytes([bytes[0], bytes[1]]) as f32 / 32768.0,
        SampleKind::Int24 => {
            // Sign-extend the 24-bit little-endian value.
            let v =
                ((bytes[2] as i32) << 24 | (bytes[1] as i32) << 16 | (bytes[0] as i32) << 8) >> 8;
            v as f32 / 8_388_608.0
        }
        SampleKind::Int32 => {
            i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f32 / 2_147_483_648.0
        }
    }
}

/// Convert an interleaved device buffer into stereo frames at 48 kHz.
pub fn convert_buffer(
    raw: &[u8],
    frames: usize,
    channels: usize,
    kind: SampleKind,
    silent: bool,
    resampler: &mut Resampler,
    out: &mut Vec<i16>,
) {
    if channels == 0 {
        return;
    }
    let width = kind.bytes();
    let stride = width * channels;
    for i in 0..frames {
        let frame = if silent {
            [0.0; CHANNELS]
        } else {
            let base = i * stride;
            let Some(chunk) = raw.get(base..base + stride) else {
                break;
            };
            let left = sample_at(&chunk[..width], kind);
            let right = if channels > 1 {
                sample_at(&chunk[width..width * 2], kind)
            } else {
                left
            };
            [left, right]
        };
        resampler.push_frame(frame, out);
    }
}

#[cfg(not(windows))]
fn run(_tx: crossbeam_channel::Sender<AudioPacket>, stop: Arc<AtomicBool>) -> Result<()> {
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Ok(())
}

#[cfg(windows)]
fn run(tx: crossbeam_channel::Sender<AudioPacket>, stop: Arc<AtomicBool>) -> Result<()> {
    use anyhow::anyhow;
    use windows::Win32::Media::Audio::*;
    use windows::Win32::System::Com::*;

    /// Frees the mix format on every exit path, including the `?` ones.
    struct MixFormat(*mut WAVEFORMATEX);
    impl Drop for MixFormat {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CoTaskMemFree(Some(self.0 as *const _)) };
            }
        }
    }

    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED).ok();
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let mix = MixFormat(client.GetMixFormat()?);
        if mix.0.is_null() {
            return Err(anyhow!("GetMixFormat returned null"));
        }

        let wf = &*mix.0;
        let channels = wf.nChannels as usize;
        let bits = wf.wBitsPerSample;
        let rate = wf.nSamplesPerSec;
        let tag = wf.wFormatTag;
        // For WAVE_FORMAT_EXTENSIBLE the real sample type lives in the
        // sub-format GUID that follows the WAVEFORMATEX header.
        let sub_tag = if tag == 0xFFFE && wf.cbSize as usize >= 22 {
            let ext = &*(mix.0 as *const WAVEFORMATEXTENSIBLE);
            Some(ext.SubFormat.data1)
        } else {
            None
        };
        let Some(kind) = SampleKind::detect(tag, sub_tag, bits) else {
            return Err(anyhow!(
                "unsupported loopback format (tag {tag:#x}, sub {sub_tag:?}, {bits} bit)"
            ));
        };
        tracing::info!(channels, bits, rate, ?kind, "WASAPI loopback mix format");

        // 20 ms of buffer, in 100 ns units.
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK,
            200_000,
            0,
            mix.0,
            None,
        )?;
        let capture: IAudioCaptureClient = client.GetService()?;
        client.Start()?;

        let mut resampler = Resampler::new(rate);
        if !resampler.is_passthrough() {
            tracing::info!("resampling loopback audio from {rate} Hz to {RATE} Hz");
        }
        let mut builder = PacketBuilder::new();
        let mut samples: Vec<i16> = Vec::with_capacity(4096);
        let mut packets: Vec<AudioPacket> = Vec::new();

        let result = (|| -> Result<()> {
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(FRAME_MS as u64));
                let mut packet_len = capture.GetNextPacketSize()?;
                while packet_len > 0 {
                    let mut data_ptr: *mut u8 = std::ptr::null_mut();
                    let mut num_frames: u32 = 0;
                    let mut flags_out: u32 = 0;
                    capture.GetBuffer(
                        &mut data_ptr,
                        &mut num_frames,
                        &mut flags_out,
                        None,
                        None,
                    )?;
                    let frames = num_frames as usize;
                    if !data_ptr.is_null() && frames > 0 {
                        let silent = flags_out & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                        let raw =
                            std::slice::from_raw_parts(data_ptr, frames * channels * kind.bytes());
                        samples.clear();
                        convert_buffer(
                            raw,
                            frames,
                            channels,
                            kind,
                            silent,
                            &mut resampler,
                            &mut samples,
                        );
                        packets.clear();
                        builder.push(&samples, &mut packets);
                        for p in packets.drain(..) {
                            if tx.send(p).is_err() {
                                capture.ReleaseBuffer(num_frames)?;
                                return Ok(());
                            }
                        }
                    }
                    capture.ReleaseBuffer(num_frames)?;
                    packet_len = capture.GetNextPacketSize()?;
                }
            }
            Ok(())
        })();

        let _ = client.Stop();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_sample_formats() {
        // Plain tags.
        assert_eq!(SampleKind::detect(3, None, 32), Some(SampleKind::Float32));
        assert_eq!(SampleKind::detect(1, None, 16), Some(SampleKind::Int16));
        assert_eq!(SampleKind::detect(1, None, 24), Some(SampleKind::Int24));
        assert_eq!(SampleKind::detect(1, None, 32), Some(SampleKind::Int32));
        // Extensible: the sub-format decides, not the bit depth.
        assert_eq!(
            SampleKind::detect(0xFFFE, Some(3), 32),
            Some(SampleKind::Float32)
        );
        assert_eq!(
            SampleKind::detect(0xFFFE, Some(1), 32),
            Some(SampleKind::Int32),
            "32-bit integer must not be mistaken for float"
        );
        assert_eq!(
            SampleKind::detect(0xFFFE, Some(1), 16),
            Some(SampleKind::Int16)
        );
        // Unknown combinations are refused rather than silently producing noise.
        assert_eq!(SampleKind::detect(0xFFFE, None, 32), None);
        assert_eq!(SampleKind::detect(1, None, 8), None);
        assert_eq!(SampleKind::detect(99, None, 16), None);
    }

    #[test]
    fn sample_widths_are_right() {
        assert_eq!(SampleKind::Int16.bytes(), 2);
        assert_eq!(SampleKind::Int24.bytes(), 3);
        assert_eq!(SampleKind::Int32.bytes(), 4);
        assert_eq!(SampleKind::Float32.bytes(), 4);
    }

    #[test]
    fn decodes_each_sample_format_to_the_same_value() {
        // ~0.5 full scale in every representation.
        assert!((sample_at(&0.5f32.to_le_bytes(), SampleKind::Float32) - 0.5).abs() < 1e-6);
        assert!((sample_at(&16384i16.to_le_bytes(), SampleKind::Int16) - 0.5).abs() < 1e-4);
        let i24 = 4_194_304i32; // 0.5 * 2^23
        assert!((sample_at(&i24.to_le_bytes()[..3], SampleKind::Int24) - 0.5).abs() < 1e-4);
        assert!((sample_at(&1_073_741_824i32.to_le_bytes(), SampleKind::Int32) - 0.5).abs() < 1e-6);
        // Negative values sign-extend correctly.
        assert!((sample_at(&(-16384i16).to_le_bytes(), SampleKind::Int16) + 0.5).abs() < 1e-4);
        assert!((sample_at(&(-i24).to_le_bytes()[..3], SampleKind::Int24) + 0.5).abs() < 1e-4);
    }

    #[test]
    fn full_scale_does_not_wrap() {
        assert_eq!(to_i16(1.0), 32767);
        assert_eq!(to_i16(-1.0), -32767);
        assert_eq!(to_i16(2.0), 32767, "clamped, not wrapped");
        assert_eq!(to_i16(-2.0), -32767);
        assert_eq!(to_i16(0.0), 0);
    }

    fn resample_count(src_rate: u32, input_frames: usize) -> usize {
        let mut r = Resampler::new(src_rate);
        let mut out = Vec::new();
        for i in 0..input_frames {
            let v = (i as f32 * 0.001).sin();
            r.push_frame([v, v], &mut out);
        }
        out.len() / CHANNELS
    }

    #[test]
    fn resampler_produces_the_right_number_of_frames() {
        // One second of input at each common rate should yield ~48000 frames.
        for rate in [44_100u32, 48_000, 88_200, 96_000, 32_000] {
            let got = resample_count(rate, rate as usize);
            let want = RATE as usize;
            let drift = (got as i64 - want as i64).abs();
            assert!(
                drift <= 2,
                "{rate} Hz -> {got} frames, expected about {want} (drift {drift})"
            );
        }
    }

    #[test]
    fn resampler_is_exact_at_48k() {
        let mut r = Resampler::new(RATE);
        assert!(r.is_passthrough());
        let mut out = Vec::new();
        for i in 0..1000 {
            let v = i as f32 / 1000.0;
            r.push_frame([v, -v], &mut out);
        }
        assert_eq!(out.len(), 1000 * CHANNELS);
        // Passthrough must not distort: values come back in order, one frame
        // behind (the interpolator's anchor).
        assert_eq!(out[2], to_i16(0.0));
        assert_eq!(out[4], to_i16(1.0 / 1000.0));
    }

    #[test]
    fn resampler_preserves_a_constant_signal() {
        // A steady tone must not develop ramps or clicks through the converter.
        let mut r = Resampler::new(44_100);
        let mut out = Vec::new();
        for _ in 0..2000 {
            r.push_frame([0.25, -0.25], &mut out);
        }
        assert!(!out.is_empty());
        for pair in out.chunks(2).skip(1) {
            assert_eq!(pair[0], to_i16(0.25));
            assert_eq!(pair[1], to_i16(-0.25));
        }
    }

    #[test]
    fn resampler_handles_a_zero_rate_without_dividing_by_zero() {
        let mut r = Resampler::new(0);
        let mut out = Vec::new();
        r.push_frame([0.5, 0.5], &mut out);
        assert!(!out.is_empty());
    }

    #[test]
    fn mono_endpoints_are_duplicated_to_stereo() {
        let mut r = Resampler::new(RATE);
        let mut out = Vec::new();
        let raw: Vec<u8> = (0..4i16).flat_map(|v| (v * 1000).to_le_bytes()).collect();
        convert_buffer(&raw, 4, 1, SampleKind::Int16, false, &mut r, &mut out);
        assert_eq!(out.len() % CHANNELS, 0);
        for pair in out.chunks(2) {
            assert_eq!(pair[0], pair[1], "mono must be copied to both channels");
        }
    }

    #[test]
    fn extra_channels_are_dropped_not_misread() {
        // A 5.1 endpoint: take the first two channels, skip the rest.
        let mut r = Resampler::new(RATE);
        let mut out = Vec::new();
        let frames = 3;
        let channels = 6;
        let mut raw = Vec::new();
        for f in 0..frames {
            for ch in 0..channels {
                let v: i16 = if ch == 0 {
                    1000 * (f as i16 + 1)
                } else if ch == 1 {
                    -1000 * (f as i16 + 1)
                } else {
                    32000 // must never appear in the output
                };
                raw.extend_from_slice(&v.to_le_bytes());
            }
        }
        convert_buffer(
            &raw,
            frames,
            channels,
            SampleKind::Int16,
            false,
            &mut r,
            &mut out,
        );
        assert!(!out.is_empty());
        assert!(
            out.iter().all(|s| s.abs() < 4000),
            "surround channels leaked: {out:?}"
        );
    }

    #[test]
    fn silent_buffers_produce_silence() {
        let mut r = Resampler::new(RATE);
        let mut out = Vec::new();
        // Deliberately non-zero bytes; the silent flag must win.
        let raw = vec![0xFFu8; 4 * 2 * 2];
        convert_buffer(&raw, 4, 2, SampleKind::Int16, true, &mut r, &mut out);
        assert!(out.iter().all(|s| *s == 0), "{out:?}");
    }

    #[test]
    fn a_short_device_buffer_stops_instead_of_reading_past_the_end() {
        let mut r = Resampler::new(RATE);
        let mut out = Vec::new();
        // Claim 100 frames but supply 2.
        let raw = vec![0u8; 2 * 2 * 2];
        convert_buffer(&raw, 100, 2, SampleKind::Int16, false, &mut r, &mut out);
        assert!(out.len() <= 2 * CHANNELS + CHANNELS);
    }

    #[test]
    fn packets_are_exactly_one_wire_frame_each() {
        let mut b = PacketBuilder::new();
        let mut packets = Vec::new();
        // Two and a half packets' worth.
        let samples = vec![7i16; FRAME_VALUES * 2 + FRAME_VALUES / 2];
        b.push(&samples, &mut packets);
        assert_eq!(packets.len(), 2, "only whole packets are emitted");
        for p in &packets {
            assert_eq!(p.pcm.len(), FRAME_VALUES * 2);
            // A sealed audio datagram must fit the MTU budget.
            assert!(
                p.pcm.len() + brolink_core::proto::AUDIO_HEADER_LEN
                    <= brolink_core::proto::MAX_PAYLOAD,
                "audio packet of {} bytes exceeds the datagram budget",
                p.pcm.len()
            );
        }
        // The remainder is carried into the next push.
        let mut more = Vec::new();
        b.push(&vec![7i16; FRAME_VALUES / 2], &mut more);
        assert_eq!(more.len(), 1);
    }

    #[test]
    fn packet_bytes_are_little_endian_pairs() {
        let mut b = PacketBuilder::new();
        let mut packets = Vec::new();
        let mut samples = vec![0i16; FRAME_VALUES];
        samples[0] = -2;
        samples[1] = 258;
        b.push(&samples, &mut packets);
        assert_eq!(packets.len(), 1);
        assert_eq!(&packets[0].pcm[0..2], &(-2i16).to_le_bytes());
        assert_eq!(&packets[0].pcm[2..4], &258i16.to_le_bytes());
    }
}
