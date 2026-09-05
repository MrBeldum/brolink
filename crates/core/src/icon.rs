//! The application icon, rendered procedurally so neither app needs an image
//! decoder or an asset pipeline: a dark rounded tile with two gold links.

/// Render the icon at `size` pixels square as straight (unpremultiplied) RGBA.
pub fn render(size: u32) -> Vec<u8> {
    let s = size.max(8) as f32;
    let mut out = vec![0u8; (size * size * 4) as usize];
    // Supersample so the edges are smooth at small sizes.
    const SS: u32 = 3;
    let bg = [16.0, 20.0, 27.0];
    let gold = [245.0, 165.0, 36.0];
    let gold_dark = [190.0, 120.0, 20.0];

    for y in 0..size {
        for x in 0..size {
            let mut acc = [0.0f32; 4];
            for sy in 0..SS {
                for sx in 0..SS {
                    let px = x as f32 + (sx as f32 + 0.5) / SS as f32;
                    let py = y as f32 + (sy as f32 + 0.5) / SS as f32;
                    let (rgb, a) = sample(px / s, py / s, bg, gold, gold_dark);
                    acc[0] += rgb[0] * a;
                    acc[1] += rgb[1] * a;
                    acc[2] += rgb[2] * a;
                    acc[3] += a;
                }
            }
            let n = (SS * SS) as f32;
            let a = acc[3] / n;
            let i = ((y * size + x) * 4) as usize;
            if a > 0.0 {
                out[i] = (acc[0] / acc[3]).round().clamp(0.0, 255.0) as u8;
                out[i + 1] = (acc[1] / acc[3]).round().clamp(0.0, 255.0) as u8;
                out[i + 2] = (acc[2] / acc[3]).round().clamp(0.0, 255.0) as u8;
            }
            out[i + 3] = (a * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

/// Colour and coverage at a point in unit coordinates.
fn sample(u: f32, v: f32, bg: [f32; 3], gold: [f32; 3], gold_dark: [f32; 3]) -> ([f32; 3], f32) {
    // Rounded square tile.
    let r = 0.22;
    let cx = (u - 0.5).abs();
    let cy = (v - 0.5).abs();
    let inside_tile = {
        let qx = (cx - (0.5 - r)).max(0.0);
        let qy = (cy - (0.5 - r)).max(0.0);
        (qx * qx + qy * qy).sqrt() <= r
    };
    if !inside_tile {
        return ([0.0; 3], 0.0);
    }

    // Two interlocking rings, offset left and right, tilted a little.
    let ring = |x0: f32, y0: f32, outer: f32, inner: f32| {
        let dx = u - x0;
        let dy = v - y0;
        // Rotate 30 degrees so the pair reads as a chain link.
        let (sn, cs) = (0.5f32, 0.866f32);
        let rx = dx * cs + dy * sn;
        let ry = -dx * sn + dy * cs;
        // Slightly elliptical for a link rather than a circle.
        let d = ((rx / 1.25).powi(2) + ry.powi(2)).sqrt();
        d <= outer && d >= inner
    };
    let left = ring(0.40, 0.55, 0.22, 0.14);
    let right = ring(0.60, 0.45, 0.22, 0.14);
    if left && right {
        (gold_dark, 1.0)
    } else if left || right {
        (gold, 1.0)
    } else {
        (bg, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_the_requested_size_with_transparent_corners_and_a_gold_mark() {
        let size = 64;
        let rgba = render(size);
        assert_eq!(rgba.len(), (size * size * 4) as usize);
        // The very corner lies outside the rounded tile.
        assert_eq!(rgba[3], 0, "top-left corner is transparent");
        // The centre is opaque.
        let c = ((size / 2) * size + size / 2) as usize * 4;
        assert_eq!(rgba[c + 3], 255);
        // Something gold is drawn somewhere.
        assert!(rgba
            .chunks(4)
            .any(|p| p[3] == 255 && p[0] > 200 && p[1] > 120 && p[2] < 80));
    }

    #[test]
    fn tiny_sizes_do_not_panic() {
        for s in [1, 2, 8, 16] {
            let _ = render(s);
        }
    }
}
