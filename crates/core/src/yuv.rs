//! BT.601 limited-range YUV420 <-> RGBA/BGRA conversion.
//!
//! These run per decoded frame, so they take caller-owned output buffers: at
//! 1080p60 a fresh allocation per frame is half a gigabyte a second of
//! churn for no reason.

/// Bytes needed to hold `width * height` RGBA pixels.
pub const fn rgba_len(width: usize, height: usize) -> usize {
    width * height * 4
}

/// A borrowed I420 picture: three planes and their row strides.
///
/// Strides are not the same as widths — decoders align rows — so they travel
/// with the planes rather than being inferred.
#[derive(Debug, Clone, Copy)]
pub struct Planes<'a> {
    pub y: &'a [u8],
    pub u: &'a [u8],
    pub v: &'a [u8],
    pub y_stride: usize,
    pub u_stride: usize,
    pub v_stride: usize,
}

impl<'a> Planes<'a> {
    /// Planes packed with no row padding, as most test fixtures are.
    pub fn packed(y: &'a [u8], u: &'a [u8], v: &'a [u8], width: usize) -> Self {
        let chroma = width.div_ceil(2);
        Self {
            y,
            u,
            v,
            y_stride: width,
            u_stride: chroma,
            v_stride: chroma,
        }
    }
}

/// Convert I420 planes to RGBA, resizing `out` to fit.
///
/// Rows or columns the source cannot supply are left black rather than
/// panicking: a decoder handing back a short plane is a bug worth seeing as a
/// glitch, not a crash mid-session.
pub fn convert(planes: Planes<'_>, width: usize, height: usize, out: &mut Vec<u8>) {
    out.clear();
    out.resize(rgba_len(width, height), 0);
    convert_into(planes, width, height, out);
}

/// As [`convert`], but into an already-sized slice.
pub fn convert_into(planes: Planes<'_>, width: usize, height: usize, out: &mut [u8]) {
    if width == 0 || height == 0 || out.len() < rgba_len(width, height) {
        return;
    }
    let chroma_width = width.div_ceil(2);
    for row in 0..height {
        let dst = &mut out[row * width * 4..(row + 1) * width * 4];
        let Some(y_row) = plane_row(planes.y, row, planes.y_stride, width) else {
            dst.fill(0);
            continue;
        };
        let uv_row = row / 2;
        let u_row = plane_row(planes.u, uv_row, planes.u_stride, chroma_width);
        let v_row = plane_row(planes.v, uv_row, planes.v_stride, chroma_width);
        let (Some(u_row), Some(v_row)) = (u_row, v_row) else {
            dst.fill(0);
            continue;
        };
        for col in 0..width {
            let c = y_row[col] as i32 - 16;
            let d = u_row[col / 2] as i32 - 128;
            let e = v_row[col / 2] as i32 - 128;
            let px = &mut dst[col * 4..col * 4 + 4];
            px[0] = clamp_u8((298 * c + 409 * e + 128) >> 8);
            px[1] = clamp_u8((298 * c - 100 * d - 208 * e + 128) >> 8);
            px[2] = clamp_u8((298 * c + 516 * d + 128) >> 8);
            px[3] = 255;
        }
    }
}

/// Allocating convenience wrapper. Prefer [`convert`] on a hot path.
pub fn yuv420_to_rgba(planes: Planes<'_>, width: usize, height: usize) -> Vec<u8> {
    let mut out = Vec::new();
    convert(planes, width, height, &mut out);
    out
}

fn plane_row(plane: &[u8], row: usize, stride: usize, needed: usize) -> Option<&[u8]> {
    let start = row.checked_mul(stride)?;
    let end = start.checked_add(needed)?;
    plane.get(start..end)
}

#[inline]
fn clamp_u8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// Packed BGRA8 -> I420 (BT.601 limited).
pub fn bgra_to_i420(
    bgra: &[u8],
    width: usize,
    height: usize,
    stride: usize,
) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let cw = width.div_ceil(2);
    let ch = height.div_ceil(2);
    let mut y = vec![0u8; width * height];
    let mut u = vec![128u8; cw * ch];
    let mut v = vec![128u8; cw * ch];
    for row in 0..height {
        for col in 0..width {
            let p = row * stride + col * 4;
            let Some(px) = bgra.get(p..p + 4) else {
                continue;
            };
            let (b, g, r) = (px[0] as i32, px[1] as i32, px[2] as i32);
            y[row * width + col] = clamp_u8(((66 * r + 129 * g + 25 * b + 128) >> 8) + 16);
            if row % 2 == 0 && col % 2 == 0 {
                let uv_i = (row / 2) * cw + (col / 2);
                u[uv_i] = clamp_u8(((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128);
                v[uv_i] = clamp_u8(((112 * r - 94 * g - 18 * b + 128) >> 8) + 128);
            }
        }
    }
    (y, u, v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_gray() {
        let (w, h) = (16usize, 16usize);
        let bgra = [80u8, 80, 80, 255].repeat(w * h);
        let (y, u, v) = bgra_to_i420(&bgra, w, h, w * 4);
        let rgba = yuv420_to_rgba(Planes::packed(&y, &u, &v, w), w, h);
        assert!(rgba[0].abs_diff(80) < 20);
        assert!(rgba[1].abs_diff(80) < 20);
        assert!(rgba[2].abs_diff(80) < 20);
        assert_eq!(rgba[3], 255);
    }

    #[test]
    fn roundtrip_primaries_stay_recognisable() {
        let (w, h) = (8usize, 8usize);
        // BGRA: pure red, then pure blue, then pure green.
        for (bgra_px, expect) in [
            ([0u8, 0, 255, 255], (true, false, false)),
            ([255u8, 0, 0, 255], (false, false, true)),
            ([0u8, 255, 0, 255], (false, true, false)),
        ] {
            let bgra = bgra_px.repeat(w * h);
            let (y, u, v) = bgra_to_i420(&bgra, w, h, w * 4);
            let rgba = yuv420_to_rgba(Planes::packed(&y, &u, &v, w), w, h);
            let (r, g, b) = (rgba[0], rgba[1], rgba[2]);
            let (want_r, want_g, want_b) = expect;
            assert_eq!(
                r > 160,
                want_r,
                "red channel for {bgra_px:?} -> {r},{g},{b}"
            );
            assert_eq!(
                g > 160,
                want_g,
                "green channel for {bgra_px:?} -> {r},{g},{b}"
            );
            assert_eq!(
                b > 160,
                want_b,
                "blue channel for {bgra_px:?} -> {r},{g},{b}"
            );
        }
    }

    #[test]
    fn convert_reuses_the_output_buffer() {
        let (w, h) = (32usize, 16usize);
        let y = vec![128u8; w * h];
        let u = vec![128u8; (w / 2) * (h / 2)];
        let v = vec![128u8; (w / 2) * (h / 2)];
        let planes = Planes::packed(&y, &u, &v, w);
        let mut out = Vec::new();
        convert(planes, w, h, &mut out);
        let first_ptr = out.as_ptr();
        assert_eq!(out.len(), rgba_len(w, h));
        // A second frame of the same size must not reallocate.
        convert(planes, w, h, &mut out);
        assert_eq!(out.as_ptr(), first_ptr);
        assert_eq!(out.len(), rgba_len(w, h));
    }

    #[test]
    fn short_planes_do_not_panic() {
        let (w, h) = (64usize, 64usize);
        let mut out = vec![0u8; rgba_len(w, h)];
        // Every plane far too small for the claimed dimensions.
        convert_into(
            Planes::packed(&[0u8; 4], &[0u8; 2], &[0u8; 2], w),
            w,
            h,
            &mut out,
        );
        // Empty planes.
        convert_into(Planes::packed(&[], &[], &[], w), w, h, &mut out);
        // An undersized destination is refused rather than partially written.
        let (y, u, v) = (vec![0u8; 4096], vec![0u8; 1024], vec![0u8; 1024]);
        let mut tiny = vec![7u8; 8];
        convert_into(Planes::packed(&y, &u, &v, w), w, h, &mut tiny);
        assert_eq!(tiny, vec![7u8; 8]);
    }

    #[test]
    fn odd_dimensions_do_not_panic() {
        let (w, h) = (17usize, 9usize);
        let bgra = [40u8, 90, 200, 255].repeat(w * h);
        let (y, u, v) = bgra_to_i420(&bgra, w, h, w * 4);
        let rgba = yuv420_to_rgba(Planes::packed(&y, &u, &v, w), w, h);
        assert_eq!(rgba.len(), rgba_len(w, h));
    }

    #[test]
    fn zero_sized_frames_are_a_no_op() {
        let mut out = Vec::new();
        convert(Planes::packed(&[], &[], &[], 0), 0, 0, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn honours_a_stride_larger_than_the_width() {
        let (w, h) = (4usize, 2usize);
        let stride = 8usize;
        // Rows padded to `stride`; padding bytes are deliberately bright so a
        // stride bug would show up as wrong output.
        let mut y = vec![255u8; stride * h];
        for row in 0..h {
            for col in 0..w {
                y[row * stride + col] = 16; // black
            }
        }
        let c_stride = 4;
        let u = vec![128u8; c_stride * h.div_ceil(2)];
        let v = vec![128u8; c_stride * h.div_ceil(2)];
        let rgba = yuv420_to_rgba(
            Planes {
                y: &y,
                u: &u,
                v: &v,
                y_stride: stride,
                u_stride: c_stride,
                v_stride: c_stride,
            },
            w,
            h,
        );
        assert_eq!(rgba.len(), rgba_len(w, h));
        assert!(rgba
            .chunks(4)
            .all(|px| px[0] < 8 && px[1] < 8 && px[2] < 8 && px[3] == 255));
    }
}
