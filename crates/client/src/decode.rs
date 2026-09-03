//! H.264 decode via OpenH264, into reusable RGBA buffers.

use anyhow::{anyhow, Result};
use brolink_core::codec::EncodedFrame;
use brolink_core::yuv;
use openh264::decoder::Decoder;
use openh264::formats::YUVSource;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

pub struct DecodedPicture {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
    pub decode_ms: f32,
}

/// Holds the most recently decoded picture for the UI thread.
///
/// Video is a "latest frame wins" stream: queueing pictures would let a busy UI
/// thread build an unbounded backlog of 8 MB frames (an out-of-memory crash at
/// 1080p60) and then display stale ones. The UI takes the newest frame and
/// hands its old buffer back, so steady-state decoding allocates nothing.
#[derive(Default)]
pub struct VideoSink {
    slot: Mutex<Option<DecodedPicture>>,
    /// A buffer on its way back to the decoder.
    spare: Mutex<Option<Vec<u8>>>,
    frames: AtomicU64,
}

impl VideoSink {
    /// Store a picture, returning the buffer of the one it replaced.
    pub fn put(&self, pic: DecodedPicture) -> Option<Vec<u8>> {
        let previous = self.slot.lock().replace(pic);
        self.frames.fetch_add(1, Ordering::Relaxed);
        previous.map(|p| p.rgba)
    }

    /// Take the newest picture, if one arrived since the last call.
    pub fn take(&self) -> Option<DecodedPicture> {
        self.slot.lock().take()
    }

    /// Total pictures decoded since the sink was created.
    pub fn frame_count(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    /// Give a buffer back after the UI has uploaded it.
    ///
    /// Only one is held: the decoder needs exactly one buffer in flight, and
    /// keeping more would just be a pool of 8 MB allocations nobody reads.
    pub fn recycle(&self, rgba: Vec<u8>) {
        let mut spare = self.spare.lock();
        if spare
            .as_ref()
            .is_none_or(|s| s.capacity() < rgba.capacity())
        {
            *spare = Some(rgba);
        }
    }

    /// Reclaim a recycled buffer, for the decoder to write the next frame into.
    pub fn take_spare(&self) -> Option<Vec<u8>> {
        self.spare.lock().take()
    }
}

pub struct H264Decoder {
    inner: Decoder,
    /// Buffer handed back by the sink, ready for the next frame.
    spare: Vec<u8>,
}

impl H264Decoder {
    pub fn new() -> Result<Self> {
        let inner = Decoder::new().map_err(|e| anyhow!("openh264 decoder: {e:?}"))?;
        Ok(Self {
            inner,
            spare: Vec::new(),
        })
    }

    /// Decode one access unit. `None` means the decoder needs more data — normal
    /// while waiting for the first keyframe.
    pub fn decode(&mut self, frame: &EncodedFrame) -> Result<Option<DecodedPicture>> {
        let t0 = Instant::now();
        let decoded = self
            .inner
            .decode(&frame.data)
            .map_err(|e| anyhow!("openh264 decode: {e:?}"))?;
        let Some(yuv_frame) = decoded else {
            return Ok(None);
        };
        let (width, height) = yuv_frame.dimensions();
        if width == 0 || height == 0 {
            return Ok(None);
        }
        let (y_stride, u_stride, v_stride) = yuv_frame.strides();
        let mut rgba = std::mem::take(&mut self.spare);
        yuv::convert(
            yuv::Planes {
                y: yuv_frame.y(),
                u: yuv_frame.u(),
                v: yuv_frame.v(),
                y_stride,
                u_stride,
                v_stride,
            },
            width,
            height,
            &mut rgba,
        );
        Ok(Some(DecodedPicture {
            width,
            height,
            rgba,
            decode_ms: t0.elapsed().as_secs_f32() * 1000.0,
        }))
    }

    /// Accept a buffer for the next decode to write into.
    pub fn reuse(&mut self, buf: Vec<u8>) {
        if buf.capacity() > self.spare.capacity() {
            self.spare = buf;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pic(w: usize, h: usize) -> DecodedPicture {
        DecodedPicture {
            width: w,
            height: h,
            rgba: vec![0u8; w * h * 4],
            decode_ms: 1.0,
        }
    }

    #[test]
    fn sink_keeps_only_the_newest_frame() {
        let sink = VideoSink::default();
        assert!(sink.take().is_none());
        assert_eq!(sink.frame_count(), 0);

        assert!(sink.put(pic(2, 2)).is_none(), "nothing to recycle yet");
        // The second frame displaces the first and hands its buffer back.
        let recycled = sink.put(pic(4, 4)).expect("previous buffer returned");
        assert_eq!(recycled.len(), 2 * 2 * 4);
        assert_eq!(sink.frame_count(), 2);

        let got = sink.take().expect("a frame is waiting");
        assert_eq!((got.width, got.height), (4, 4), "the newest frame wins");
        assert!(sink.take().is_none(), "taking twice yields nothing");
        // Frames decoded is a running total, not a queue depth.
        assert_eq!(sink.frame_count(), 2);
    }

    #[test]
    fn a_displayed_frames_buffer_makes_it_back_to_the_decoder() {
        let sink = VideoSink::default();
        assert!(sink.take_spare().is_none(), "nothing recycled yet");

        sink.put(pic(4, 4));
        let shown = sink.take().expect("a frame is waiting");
        sink.recycle(shown.rgba);

        let reclaimed = sink.take_spare().expect("the buffer came back");
        assert!(reclaimed.capacity() >= 4 * 4 * 4);
        assert!(sink.take_spare().is_none(), "only one is ever held");
    }

    #[test]
    fn recycling_keeps_the_largest_buffer() {
        let sink = VideoSink::default();
        sink.recycle(vec![0u8; 4000]);
        // A smaller buffer must not displace one big enough for the stream.
        sink.recycle(vec![0u8; 10]);
        let kept = sink.take_spare().expect("a buffer is held");
        assert!(kept.capacity() >= 4000, "kept {} bytes", kept.capacity());
    }

    #[test]
    fn many_frames_without_a_reader_do_not_accumulate() {
        let sink = VideoSink::default();
        for _ in 0..1000 {
            sink.put(pic(8, 8));
        }
        assert_eq!(sink.frame_count(), 1000);
        assert!(sink.take().is_some());
        assert!(sink.take().is_none(), "at most one frame is ever held");
    }

    #[test]
    fn decoder_prefers_the_larger_spare_buffer() {
        let mut d = H264Decoder::new().expect("openh264 decoder");
        d.reuse(vec![0u8; 100]);
        assert!(d.spare.capacity() >= 100);
        // A smaller buffer must not replace a bigger one.
        d.reuse(Vec::with_capacity(10));
        assert!(d.spare.capacity() >= 100);
        d.reuse(vec![0u8; 5000]);
        assert!(d.spare.capacity() >= 5000);
    }

    #[test]
    fn decoding_junk_is_an_error_not_a_panic() {
        let mut d = H264Decoder::new().expect("openh264 decoder");
        let frame = EncodedFrame {
            data: vec![0xDE, 0xAD, 0xBE, 0xEF],
            keyframe: false,
            timestamp_us: 0,
        };
        // Either an error or "need more data" is fine; a panic is not.
        let _ = d.decode(&frame);
    }

    #[test]
    fn decoding_an_empty_frame_is_handled() {
        let mut d = H264Decoder::new().expect("openh264 decoder");
        let frame = EncodedFrame {
            data: Vec::new(),
            keyframe: false,
            timestamp_us: 0,
        };
        let _ = d.decode(&frame);
    }
}
