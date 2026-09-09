//! Decoded frames and the decoders that make them. Frames are NV12 (a Y plane
//! and an interleaved UV plane at half resolution), which is what
//! VideoToolbox produces and what the renderer's shader expects.

use anyhow::Result;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(not(target_os = "macos"))]
mod openh264;
#[cfg(target_os = "macos")]
mod videotoolbox;

pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub y: Vec<u8>,
    pub y_stride: usize,
    pub uv: Vec<u8>,
    pub uv_stride: usize,
    pub full_range: bool,
}

/// The newest decoded frame, replaced rather than queued: a renderer that
/// falls behind shows the latest picture instead of catching up on old ones.
#[derive(Default)]
pub struct FrameSlot {
    latest: Mutex<Option<Frame>>,
    seq: AtomicU64,
}

impl FrameSlot {
    pub fn publish(&self, frame: Frame) {
        *self.latest.lock() = Some(frame);
        self.seq.fetch_add(1, Ordering::Release);
    }

    pub fn take(&self) -> Option<Frame> {
        self.latest.lock().take()
    }

    /// Increments on every published frame; cheap to poll.
    pub fn seq(&self) -> u64 {
        self.seq.load(Ordering::Acquire)
    }

    pub fn clear(&self) {
        *self.latest.lock() = None;
    }
}

pub trait Decoder: Send {
    /// One access unit in Annex B. `None` when the decoder needs more data.
    fn decode(&mut self, annexb: &[u8], idr: bool) -> Result<Option<Frame>>;
    fn name(&self) -> &'static str;
}

/// Bit mask of `VIDEO_FORMAT_*` values this platform can decode.
pub fn supported_formats() -> i32 {
    #[cfg(target_os = "macos")]
    {
        crate::ffi::VIDEO_FORMAT_H264 | crate::ffi::VIDEO_FORMAT_H265
    }
    #[cfg(not(target_os = "macos"))]
    {
        crate::ffi::VIDEO_FORMAT_H264
    }
}

/// `CAPABILITY_*` bits for this platform's decoder.
///
/// VideoToolbox decodes synchronously in a few milliseconds, so frames can
/// be handed to it straight from the receive thread (no queue, one hop
/// less of latency), and its HEVC decoder keeps enough reference frames
/// for the host to repair a lost frame by referencing an older one
/// instead of sending a whole keyframe, which on a slow link is the
/// difference between a hiccup and a two-second freeze. H.264 reference
/// invalidation is left off: Moonlight's own Mac client does the same.
pub fn capabilities() -> i32 {
    #[cfg(target_os = "macos")]
    {
        crate::ffi::CAPABILITY_DIRECT_SUBMIT
            | crate::ffi::CAPABILITY_REFERENCE_FRAME_INVALIDATION_HEVC
    }
    #[cfg(not(target_os = "macos"))]
    {
        0
    }
}

pub fn new_decoder(format: i32, width: u32, height: u32) -> Result<Box<dyn Decoder>> {
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(videotoolbox::VideoToolbox::new(
            format, width, height,
        )?))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (width, height);
        Ok(Box::new(openh264::OpenH264::new(format)?))
    }
}

/// Split an Annex B byte stream into NAL units without their start codes.
pub fn nal_units(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(starts.len());
    for (k, &s) in starts.iter().enumerate() {
        let mut end = starts.get(k + 1).map(|&n| n - 3).unwrap_or(data.len());
        // A 4-byte start code leaves a trailing zero on the previous NAL.
        while end > s && data[end - 1] == 0 {
            end -= 1;
        }
        if end > s {
            out.push(&data[s..end]);
        }
    }
    out
}

/// Interleave I420 chroma planes into one NV12 UV plane.
pub fn interleave_uv(
    u: &[u8],
    v: &[u8],
    stride: usize,
    width: usize,
    height: usize,
    out: &mut Vec<u8>,
) {
    out.clear();
    out.reserve(width * 2 * height);
    for row in 0..height {
        let (ur, vr) = (&u[row * stride..], &v[row * stride..]);
        for x in 0..width {
            out.push(ur[x]);
            out.push(vr[x]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nal_units_are_split_on_both_start_code_lengths() {
        let data = [
            0, 0, 0, 1, 0x67, 1, 2, // SPS with a 4-byte start code
            0, 0, 1, 0x68, 3, // PPS with a 3-byte start code
            0, 0, 0, 1, 0x65, 4, 5, 6, 0, 0, // slice; trailing zeros belong to nothing
        ];
        let nals = nal_units(&data);
        assert_eq!(nals.len(), 3);
        assert_eq!(nals[0], &[0x67, 1, 2]);
        assert_eq!(nals[1], &[0x68, 3]);
        assert_eq!(nals[2], &[0x65, 4, 5, 6]);
        assert!(nal_units(&[1, 2, 3]).is_empty());
    }

    #[test]
    fn uv_interleaving_pairs_samples() {
        let u = [1u8, 2, 0, 3, 4, 0];
        let v = [5u8, 6, 0, 7, 8, 0];
        let mut out = Vec::new();
        interleave_uv(&u, &v, 3, 2, 2, &mut out);
        assert_eq!(out, vec![1, 5, 2, 6, 3, 7, 4, 8]);
    }

    #[test]
    fn slot_keeps_only_the_newest() {
        let slot = FrameSlot::default();
        let f = |w| Frame {
            width: w,
            height: 1,
            y: vec![],
            y_stride: 0,
            uv: vec![],
            uv_stride: 0,
            full_range: false,
        };
        slot.publish(f(1));
        slot.publish(f(2));
        assert_eq!(slot.seq(), 2);
        assert_eq!(slot.take().unwrap().width, 2);
        assert!(slot.take().is_none());
    }
}
