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
    fn decode(&mut self, annexb: &[u8], _idr: bool) -> Result<Option<Frame>> {
        let Some(yuv) = self.dec.decode(annexb)? else {
            return Ok(None);
        };
        let (w, h) = yuv.dimensions();
        let (ys, us, _) = yuv.strides();
        let mut y = Vec::with_capacity(w * h);
        for row in yuv.y().chunks(ys).take(h) {
            y.extend_from_slice(&row[..w]);
        }
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        interleave_uv(yuv.u(), yuv.v(), us, cw, ch, &mut self.uv);
        Ok(Some(Frame {
            width: w as u32,
            height: h as u32,
            y,
            y_stride: w,
            uv: self.uv.clone(),
            uv_stride: cw * 2,
            full_range: false,
        }))
    }

    fn name(&self) -> &'static str {
        "OpenH264 (software)"
    }
}
