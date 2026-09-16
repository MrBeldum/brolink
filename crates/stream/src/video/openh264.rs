//! Software H.264 through Cisco's OpenH264. Used on Windows and Linux, where
//! BroLink is a development and testing client rather than the product.

use super::{interleave_uv, Decoder, Frame};
use anyhow::{bail, Result};
use openh264::formats::YUVSource;

pub struct OpenH264 {
    dec: openh264::decoder::Decoder,
    uv: Vec<u8>,
}

impl OpenH264 {
    pub fn new(format: i32) -> Result<Self> {
        if format & crate::ffi::VIDEO_FORMAT_MASK_H264 == 0 {
            bail!("only H.264 can be decoded in software here");
        }
        Ok(Self {
            dec: openh264::decoder::Decoder::new()?,
            uv: Vec::new(),
        })
    }
}

impl Decoder for OpenH264 {
    fn decode(&mut self, annexb: &[u8], _idr: bool, spare: Option<Frame>) -> Result<Option<Frame>> {
        let Some(yuv) = self.dec.decode(annexb)? else {
            return Ok(None);
        };
        let (w, h) = yuv.dimensions();
        let (ys, us, _) = yuv.strides();
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        interleave_uv(yuv.u(), yuv.v(), us, cw, ch, &mut self.uv);
        let mut frame = spare.unwrap_or_default();
        frame.width = w as u32;
        frame.height = h as u32;
        frame.y_stride = w;
        frame.uv_stride = cw * 2;
        frame.full_range = false;
        frame.y.clear();
        for row in yuv.y().chunks(ys).take(h) {
            frame.y.extend_from_slice(&row[..w]);
        }
        frame.uv.clear();
        frame.uv.extend_from_slice(&self.uv);
        Ok(Some(frame))
    }

    fn name(&self) -> &'static str {
        "OpenH264 (software)"
    }
}
