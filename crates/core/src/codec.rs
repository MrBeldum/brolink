//! H.264 Annex-B splitting, frame grouping, and datagram fragmentation.

use crate::proto::{
    parse_video_payload, write_video_payload, VideoFlags, MAX_FRAME_BYTES, VIDEO_CHUNK,
};

/// NAL unit types we care about (H.264 Table 7-1).
const NAL_SLICE: u8 = 1;
const NAL_IDR: u8 = 5;
const NAL_AUD: u8 = 9;

/// Give up on a start code that never arrives rather than growing without bound.
const MAX_SPLITTER_BUF: usize = 4 * 1024 * 1024;

/// Find the next Annex-B start code. Returns (offset, start_code_len).
pub fn find_start_code(buf: &[u8]) -> Option<(usize, usize)> {
    if buf.len() < 3 {
        return None;
    }
    for i in 0..buf.len() - 2 {
        if buf[i] == 0 && buf[i + 1] == 0 {
            if buf[i + 2] == 1 {
                return Some((i, 3));
            }
            if i + 3 < buf.len() && buf[i + 2] == 0 && buf[i + 3] == 1 {
                return Some((i, 4));
            }
        }
    }
    None
}

/// Length of the start code at the very front of `buf`, if there is one.
fn leading_start_code(buf: &[u8]) -> Option<usize> {
    if buf.starts_with(&[0, 0, 0, 1]) {
        Some(4)
    } else if buf.starts_with(&[0, 0, 1]) {
        Some(3)
    } else {
        None
    }
}

fn is_slice(nal_type: u8) -> bool {
    nal_type == NAL_SLICE || nal_type == NAL_IDR
}

/// Read `first_mb_in_slice` — the leading `ue(v)` of a slice header.
///
/// Zero means the slice starts a new picture; anything else is a continuation
/// slice of the picture already in progress.
fn first_mb_in_slice(rbsp: &[u8]) -> Option<u32> {
    // Exp-Golomb: count leading zero bits, then read that many more bits.
    let mut leading_zeros = 0u32;
    let mut bit_index = 0usize;
    loop {
        let byte = *rbsp.get(bit_index / 8)?;
        let bit = (byte >> (7 - (bit_index % 8))) & 1;
        bit_index += 1;
        if bit == 1 {
            break;
        }
        leading_zeros += 1;
        if leading_zeros > 31 {
            return None;
        }
    }
    let mut value: u32 = 1;
    for _ in 0..leading_zeros {
        let byte = *rbsp.get(bit_index / 8)?;
        let bit = (byte >> (7 - (bit_index % 8))) & 1;
        bit_index += 1;
        value = (value << 1) | bit as u32;
    }
    Some(value - 1)
}

#[derive(Debug, Clone)]
pub struct EncodedFrame {
    pub data: Vec<u8>,
    pub keyframe: bool,
    pub timestamp_us: u64,
}

/// Accumulates Annex-B bytes from an encoder and emits access units.
///
/// The encoder is configured for one slice per picture, so a slice NAL ends the
/// access unit and can be emitted immediately — waiting for the *next* picture
/// to confirm the boundary would add a full frame of latency, which is exactly
/// what this project exists to avoid. If a multi-slice stream shows up anyway,
/// [`AnnexBSplitter::saw_multi_slice`] goes true so the caller can say so out
/// loud instead of leaving the user with a torn picture and no explanation.
pub struct AnnexBSplitter {
    buf: Vec<u8>,
    pending: Vec<u8>,
    pending_key: bool,
    multi_slice: bool,
}

impl Default for AnnexBSplitter {
    fn default() -> Self {
        Self {
            buf: Vec::with_capacity(512 * 1024),
            pending: Vec::with_capacity(256 * 1024),
            pending_key: false,
            multi_slice: false,
        }
    }
}

impl AnnexBSplitter {
    /// True once a picture split across several slices has been seen.
    pub fn saw_multi_slice(&self) -> bool {
        self.multi_slice
    }

    pub fn push(&mut self, bytes: &[u8], out: &mut Vec<EncodedFrame>, timestamp_us: u64) {
        self.buf.extend_from_slice(bytes);
        loop {
            let Some((start, sc_len)) = find_start_code(&self.buf) else {
                if self.buf.len() > MAX_SPLITTER_BUF {
                    tracing::warn!("no H.264 start code in {} bytes; resyncing", self.buf.len());
                    self.buf.clear();
                }
                break;
            };
            if start > 0 {
                // Garbage before the first start code.
                self.buf.drain(..start);
                continue;
            }
            // A NAL ends where the next start code begins.
            let Some((next_rel, _)) = find_start_code(&self.buf[sc_len..]) else {
                break;
            };
            let nal_end = sc_len + next_rel;
            let nal = self.buf[..nal_end].to_vec();
            self.buf.drain(..nal_end);
            self.consume_nal(&nal, out, timestamp_us);
        }
    }

    fn consume_nal(&mut self, nal: &[u8], out: &mut Vec<EncodedFrame>, timestamp_us: u64) {
        let Some(sc) = leading_start_code(nal) else {
            return;
        };
        let Some(&header) = nal.get(sc) else {
            return;
        };
        let nal_type = header & 0x1F;

        // An access unit delimiter closes whatever picture came before it.
        if nal_type == NAL_AUD && !self.pending.is_empty() {
            self.emit(out, timestamp_us);
        }

        if is_slice(nal_type) {
            match first_mb_in_slice(&nal[sc + 1..]) {
                Some(0) | None => {}
                Some(_) => {
                    if !self.multi_slice {
                        self.multi_slice = true;
                        tracing::warn!(
                            "encoder is producing multi-slice pictures; expect tearing \
                             (configure the encoder for one slice per frame)"
                        );
                    }
                }
            }
        }

        if nal_type == NAL_IDR {
            self.pending_key = true;
        }

        // Guard against a runaway access unit (a stream that never yields a
        // slice) pinning memory forever.
        if self.pending.len() + nal.len() > MAX_FRAME_BYTES {
            tracing::warn!(
                "dropping oversized access unit ({} bytes)",
                self.pending.len()
            );
            self.pending.clear();
            self.pending_key = false;
            return;
        }
        self.pending.extend_from_slice(nal);

        if is_slice(nal_type) {
            self.emit(out, timestamp_us);
        }
    }

    fn emit(&mut self, out: &mut Vec<EncodedFrame>, timestamp_us: u64) {
        if self.pending.is_empty() {
            return;
        }
        out.push(EncodedFrame {
            data: std::mem::take(&mut self.pending),
            keyframe: self.pending_key,
            timestamp_us,
        });
        self.pending_key = false;
    }

    /// Emit whatever is buffered. Call once the encoder's output has ended.
    pub fn flush(&mut self, out: &mut Vec<EncodedFrame>, timestamp_us: u64) {
        if leading_start_code(&self.buf).is_some_and(|sc| self.buf.len() > sc) {
            let nal = std::mem::take(&mut self.buf);
            self.consume_nal(&nal, out, timestamp_us);
        }
        self.emit(out, timestamp_us);
    }
}

/// Split a frame into MTU-sized video payloads (without the outer packet header).
pub fn fragment_frame(frame_id: u32, timestamp_us: u64, data: &[u8]) -> Vec<Vec<u8>> {
    if data.is_empty() {
        return Vec::new();
    }
    let chunks: Vec<&[u8]> = data.chunks(VIDEO_CHUNK).collect();
    let n = chunks.len() as u16;
    chunks
        .into_iter()
        .enumerate()
        .map(|(i, c)| write_video_payload(frame_id, i as u16, n, timestamp_us, c))
        .collect()
}

/// Reassembles fragmented frames, newest-frame-wins.
#[derive(Default)]
pub struct FrameAssembler {
    current_id: Option<u32>,
    /// The most recent frame handed to the decoder, so late duplicates of it
    /// are ignored instead of restarting assembly and inflating the loss count.
    last_completed: Option<u32>,
    parts: Vec<Option<Vec<u8>>>,
    received: u16,
    expected: u16,
    timestamp_us: u64,
    pub dropped: u64,
    pub assembled: u64,
}

/// Wrapping-aware "is `a` newer than `b`".
fn is_newer(a: u32, b: u32) -> bool {
    a != b && a.wrapping_sub(b) < u32::MAX / 2
}

impl FrameAssembler {
    pub fn push(&mut self, payload: &[u8]) -> Option<EncodedFrame> {
        let Ok((frame_id, idx, count, ts, data)) = parse_video_payload(payload) else {
            return None;
        };
        if count == 0 || idx >= count {
            return None;
        }
        // A straggler for a frame we already delivered, or for one older than
        // the frame in progress: nothing useful left to do with it.
        if self
            .last_completed
            .is_some_and(|done| !is_newer(frame_id, done))
        {
            return None;
        }
        if self.current_id.is_some_and(|cur| is_newer(cur, frame_id)) {
            return None;
        }

        if self.current_id != Some(frame_id) {
            if self.current_id.is_some() && self.received < self.expected {
                // Abandoning a partial frame: that is a real dropped frame.
                self.dropped += 1;
            }
            self.current_id = Some(frame_id);
            self.parts.clear();
            self.parts.resize(count as usize, None);
            self.received = 0;
            self.expected = count;
            self.timestamp_us = ts;
        }
        if self.parts.len() as u16 != count {
            // Fragment count disagrees with the rest of this frame; treat the
            // frame as unrecoverable rather than assembling a corrupt buffer.
            return None;
        }
        if self.parts[idx as usize].is_none() {
            self.parts[idx as usize] = Some(data.to_vec());
            self.received += 1;
        }
        if self.received < self.expected {
            return None;
        }

        let total: usize = self.parts.iter().flatten().map(|p| p.len()).sum();
        let mut frame = Vec::with_capacity(total);
        for p in self.parts.iter_mut() {
            if let Some(chunk) = p.take() {
                frame.extend_from_slice(&chunk);
            }
        }
        self.current_id = None;
        self.last_completed = Some(frame_id);
        self.received = 0;
        self.expected = 0;
        self.assembled += 1;
        let keyframe = is_keyframe(&frame);
        Some(EncodedFrame {
            data: frame,
            keyframe,
            timestamp_us: self.timestamp_us,
        })
    }

    /// Fraction of frames lost, 0.0..=100.0.
    pub fn loss_pct(&self) -> f32 {
        let total = self.assembled + self.dropped;
        if total == 0 {
            0.0
        } else {
            100.0 * self.dropped as f32 / total as f32
        }
    }
}

pub fn is_keyframe(annexb: &[u8]) -> bool {
    let mut rest = annexb;
    while let Some((off, sc)) = find_start_code(rest) {
        let nal = &rest[off + sc..];
        let Some(&header) = nal.first() else {
            break;
        };
        if header & 0x1F == NAL_IDR {
            return true;
        }
        rest = nal;
    }
    false
}

pub fn keyframe_flag(key: bool) -> u8 {
    if key {
        VideoFlags::KEYFRAME.bits()
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAL_SEI: u8 = 6;
    const NAL_SPS: u8 = 7;
    const NAL_PPS: u8 = 8;

    fn nal(ty: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![0, 0, 0, 1, ty];
        v.extend_from_slice(payload);
        v
    }

    /// A slice header whose `first_mb_in_slice` is `n`, as exp-Golomb bits.
    fn slice_nal(ty: u8, first_mb: u32) -> Vec<u8> {
        let v = first_mb + 1;
        let bits = 32 - v.leading_zeros();
        let mut out: Vec<u8> = Vec::new();
        let mut acc = 0u32;
        let mut nbits = 0u32;
        // (bits - 1) zeros, then the value itself.
        for _ in 0..bits - 1 {
            acc <<= 1;
            nbits += 1;
        }
        for i in (0..bits).rev() {
            acc = (acc << 1) | ((v >> i) & 1);
            nbits += 1;
        }
        acc <<= 8 - (nbits % 8) % 8;
        let byte_len = nbits.div_ceil(8);
        for i in (0..byte_len).rev() {
            out.push(((acc >> (i * 8)) & 0xFF) as u8);
        }
        nal(ty, &out)
    }

    #[test]
    fn parses_first_mb_in_slice() {
        // ue(0) = "1", ue(1) = "010", ue(2) = "011", ue(3) = "00100"
        assert_eq!(first_mb_in_slice(&[0b1000_0000]), Some(0));
        assert_eq!(first_mb_in_slice(&[0b0100_0000]), Some(1));
        assert_eq!(first_mb_in_slice(&[0b0110_0000]), Some(2));
        assert_eq!(first_mb_in_slice(&[0b0010_0000]), Some(3));
        assert_eq!(first_mb_in_slice(&[]), None);
        assert_eq!(first_mb_in_slice(&[0, 0, 0, 0, 0]), None);
    }

    #[test]
    fn slice_nal_helper_roundtrips() {
        for n in [0u32, 1, 2, 3, 7, 40, 8100] {
            let built = slice_nal(NAL_SLICE, n);
            assert_eq!(first_mb_in_slice(&built[5..]), Some(n), "first_mb {n}");
        }
    }

    #[test]
    fn split_two_frames() {
        let mut s = AnnexBSplitter::default();
        let mut stream = Vec::new();
        stream.extend(nal(NAL_SPS, b"sps"));
        stream.extend(nal(NAL_PPS, b"pps"));
        stream.extend(slice_nal(NAL_IDR, 0));
        stream.extend(slice_nal(NAL_SLICE, 0));
        let mut out = Vec::new();
        s.push(&stream, &mut out, 1);
        s.flush(&mut out, 1);
        assert_eq!(out.len(), 2);
        assert!(out[0].keyframe, "SPS+PPS+IDR is the keyframe");
        assert!(!out[1].keyframe);
        assert!(!s.saw_multi_slice());
    }

    #[test]
    fn keyframe_carries_its_parameter_sets() {
        let mut s = AnnexBSplitter::default();
        let mut stream = Vec::new();
        stream.extend(nal(NAL_SPS, b"sps"));
        stream.extend(nal(NAL_PPS, b"pps"));
        stream.extend(slice_nal(NAL_IDR, 0));
        let mut out = Vec::new();
        s.push(&stream, &mut out, 1);
        s.flush(&mut out, 1);
        assert_eq!(out.len(), 1);
        // The decoder needs SPS and PPS in the same access unit as the IDR.
        assert!(out[0]
            .data
            .windows(3)
            .any(|w| w == [0, 0, 1] || w == [0, 0, 0]));
        assert!(is_keyframe(&out[0].data));
        assert_eq!(out[0].data.len(), stream.len());
    }

    #[test]
    fn aud_closes_the_previous_picture() {
        let mut s = AnnexBSplitter::default();
        let mut stream = Vec::new();
        stream.extend(nal(NAL_AUD, b"\x10"));
        stream.extend(nal(NAL_SPS, b"sps"));
        stream.extend(slice_nal(NAL_IDR, 0));
        stream.extend(nal(NAL_AUD, b"\x10"));
        stream.extend(slice_nal(NAL_SLICE, 0));
        let mut out = Vec::new();
        s.push(&stream, &mut out, 7);
        s.flush(&mut out, 7);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn multi_slice_pictures_are_detected_and_reported() {
        let mut s = AnnexBSplitter::default();
        let mut stream = Vec::new();
        stream.extend(slice_nal(NAL_SLICE, 0));
        stream.extend(slice_nal(NAL_SLICE, 120));
        let mut out = Vec::new();
        s.push(&stream, &mut out, 1);
        s.flush(&mut out, 1);
        assert!(
            s.saw_multi_slice(),
            "a continuation slice must be flagged so the encoder config can be fixed"
        );
    }

    #[test]
    fn split_survives_being_fed_one_byte_at_a_time() {
        let mut stream = Vec::new();
        stream.extend(nal(NAL_SPS, b"sps"));
        stream.extend(slice_nal(NAL_IDR, 0));
        stream.extend(slice_nal(NAL_SLICE, 0));
        stream.extend(slice_nal(NAL_SLICE, 0));

        let mut s = AnnexBSplitter::default();
        let mut out = Vec::new();
        for b in &stream {
            s.push(&[*b], &mut out, 1);
        }
        s.flush(&mut out, 1);
        assert_eq!(out.len(), 3);
        assert!(out[0].keyframe);

        // Byte-at-a-time must produce exactly the same frames as one big push.
        let mut s2 = AnnexBSplitter::default();
        let mut out2 = Vec::new();
        s2.push(&stream, &mut out2, 1);
        s2.flush(&mut out2, 1);
        let a: Vec<&Vec<u8>> = out.iter().map(|f| &f.data).collect();
        let b: Vec<&Vec<u8>> = out2.iter().map(|f| &f.data).collect();
        assert_eq!(a, b);
    }

    #[test]
    fn three_byte_start_codes_work_too() {
        let mut s = AnnexBSplitter::default();
        let mut stream = vec![0, 0, 1, NAL_SPS, b's'];
        stream.extend([0, 0, 1, NAL_IDR, 0b1000_0000]);
        stream.extend([0, 0, 1, NAL_SLICE, 0b1000_0000]);
        let mut out = Vec::new();
        s.push(&stream, &mut out, 1);
        s.flush(&mut out, 1);
        assert_eq!(out.len(), 2);
        assert!(out[0].keyframe);
    }

    #[test]
    fn leading_garbage_is_discarded() {
        let mut s = AnnexBSplitter::default();
        let mut stream = vec![0xDE, 0xAD, 0xBE, 0xEF];
        stream.extend(slice_nal(NAL_IDR, 0));
        stream.extend(slice_nal(NAL_SLICE, 0));
        let mut out = Vec::new();
        s.push(&stream, &mut out, 1);
        s.flush(&mut out, 1);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn fragment_and_assemble() {
        let data: Vec<u8> = (0..5000).map(|i| i as u8).collect();
        let frags = fragment_frame(3, 99, &data);
        assert!(frags.len() > 1);
        let mut a = FrameAssembler::default();
        let mut got = None;
        for f in &frags {
            if let Some(frame) = a.push(f) {
                got = Some(frame);
            }
        }
        let got = got.unwrap();
        assert_eq!(got.data, data);
        assert_eq!(got.timestamp_us, 99);
        assert_eq!(a.assembled, 1);
        assert_eq!(a.dropped, 0);
    }

    #[test]
    fn out_of_order_fragments_still_assemble() {
        let data: Vec<u8> = (0..5000).map(|i| i as u8).collect();
        let mut frags = fragment_frame(3, 99, &data);
        frags.reverse();
        let mut a = FrameAssembler::default();
        let got = frags.iter().find_map(|f| a.push(f)).unwrap();
        assert_eq!(got.data, data);
        assert_eq!(a.dropped, 0);
    }

    #[test]
    fn duplicate_fragments_after_completion_do_not_count_as_loss() {
        let data: Vec<u8> = (0..5000).map(|i| i as u8).collect();
        let frags = fragment_frame(3, 99, &data);
        let mut a = FrameAssembler::default();
        for f in &frags {
            a.push(f);
        }
        assert_eq!(a.assembled, 1);
        // A retransmit or a duplicated datagram arrives after we finished.
        for f in &frags {
            assert!(a.push(f).is_none());
        }
        assert_eq!(a.dropped, 0, "duplicates are not losses");
        assert_eq!(a.assembled, 1);
        assert_eq!(a.loss_pct(), 0.0);

        // The next real frame still assembles normally.
        let next = fragment_frame(4, 100, &data);
        let got = next.iter().find_map(|f| a.push(f)).unwrap();
        assert_eq!(got.data, data);
    }

    #[test]
    fn an_incomplete_frame_counts_as_dropped_once() {
        let data: Vec<u8> = (0..5000).map(|i| i as u8).collect();
        let partial = fragment_frame(1, 1, &data);
        let complete = fragment_frame(2, 2, &data);
        let mut a = FrameAssembler::default();
        // Only the first fragment of frame 1 arrives.
        assert!(a.push(&partial[0]).is_none());
        for f in &complete {
            a.push(f);
        }
        assert_eq!(a.dropped, 1);
        assert_eq!(a.assembled, 1);
        assert!((a.loss_pct() - 50.0).abs() < 0.01);
    }

    #[test]
    fn stale_fragments_from_an_older_frame_are_ignored() {
        let data: Vec<u8> = (0..5000).map(|i| i as u8).collect();
        let old = fragment_frame(1, 1, &data);
        let new = fragment_frame(2, 2, &data);
        let mut a = FrameAssembler::default();
        assert!(a.push(&new[0]).is_none());
        // A fragment of frame 1 shows up late; it must not reset frame 2.
        assert!(a.push(&old[0]).is_none());
        let got = new[1..].iter().find_map(|f| a.push(f)).unwrap();
        assert_eq!(got.data, data, "frame 2 must survive the stale fragment");
    }

    #[test]
    fn frame_ids_wrap_without_stalling() {
        let data: Vec<u8> = (0..3000).map(|i| i as u8).collect();
        let mut a = FrameAssembler::default();
        for id in [u32::MAX - 1, u32::MAX, 0, 1] {
            let frags = fragment_frame(id, id as u64, &data);
            let got = frags.iter().find_map(|f| a.push(f));
            assert!(got.is_some(), "frame {id} must assemble across the wrap");
        }
        assert_eq!(a.assembled, 4);
        assert_eq!(a.dropped, 0);
    }

    #[test]
    fn malformed_fragments_are_rejected() {
        let mut a = FrameAssembler::default();
        assert!(a.push(&[]).is_none());
        assert!(a.push(&[0u8; 8]).is_none());
        // frag_count = 0
        assert!(a.push(&write_video_payload(1, 0, 0, 0, b"x")).is_none());
        // idx >= count
        assert!(a.push(&write_video_payload(1, 5, 2, 0, b"x")).is_none());
    }

    #[test]
    fn is_keyframe_detects_idr_after_parameter_sets() {
        let mut stream = Vec::new();
        stream.extend(nal(NAL_SPS, b"sps"));
        stream.extend(nal(NAL_PPS, b"pps"));
        stream.extend(nal(NAL_SEI, b"sei"));
        stream.extend(slice_nal(NAL_IDR, 0));
        assert!(is_keyframe(&stream));

        let mut p_only = Vec::new();
        p_only.extend(slice_nal(NAL_SLICE, 0));
        assert!(!is_keyframe(&p_only));
        assert!(!is_keyframe(&[]));
        assert!(!is_keyframe(&[0, 0, 1]));
    }
}
