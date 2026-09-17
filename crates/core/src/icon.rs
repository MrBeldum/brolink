//! The application icon: the checked-in logo, decoded once and resized on
//! demand. Shared by the windows (via `brolink_core::icon`) and by the host
//! build script, which includes this file by path, so it must not refer to
//! anything else in this crate.

use std::sync::OnceLock;

use image::{Rgba, RgbaImage};

/// The 1024x1024 logo, a rounded tile on a near-black pad.
const LOGO_PNG: &[u8] = include_bytes!("../assets/logo-1024.png");

/// The decoded logo. PNG inflate is the expensive part; a resize is cheap.
fn logo() -> &'static RgbaImage {
    static LOGO: OnceLock<RgbaImage> = OnceLock::new();
    LOGO.get_or_init(|| {
        image::load_from_memory(LOGO_PNG)
            .expect("bundled logo-1024.png decodes")
            .into_rgba8()
    })
}

/// Bounding box of the rounded tile: where the pad colour ends on the
/// middle row and column.
fn tile_bounds(im: &RgbaImage) -> (u32, u32, u32, u32) {
    let pad = *im.get_pixel(0, 0);
    let mid_x = im.width() / 2;
    let mid_y = im.height() / 2;
    let mut x0 = 0;
    let mut x1 = im.width();
    for x in 0..im.width() {
        if *im.get_pixel(x, mid_y) != pad {
            x0 = x;
            break;
        }
    }
    for x in (0..im.width()).rev() {
        if *im.get_pixel(x, mid_y) != pad {
            x1 = x + 1;
            break;
        }
    }
    let mut y0 = 0;
    let mut y1 = im.height();
    for y in 0..im.height() {
        if *im.get_pixel(mid_x, y) != pad {
            y0 = y;
            break;
        }
    }
    for y in (0..im.height()).rev() {
        if *im.get_pixel(mid_x, y) != pad {
            y1 = y + 1;
            break;
        }
    }
    (x0, y0, x1, y1)
}

/// Render the icon at `size` pixels square as straight (unpremultiplied) RGBA.
///
/// The tile fills the canvas. Windows (taskbar, Explorer, the window) and
/// the in-app header all draw this bitmap; macOS 26 also wants a filled
/// square and applies the rounded app-icon shape itself. Pre-padding left
/// a square plate around the mark.
pub fn render(size: u32) -> Vec<u8> {
    if size == 0 {
        return Vec::new();
    }
    let logo = logo();
    let (x0, y0, x1, y1) = tile_bounds(logo);
    let cropped = image::imageops::crop_imm(logo, x0, y0, x1 - x0, y1 - y0).to_image();
    let mut out =
        image::imageops::resize(&cropped, size, size, image::imageops::FilterType::Lanczos3);
    // Opaque fill so the OS, not our alpha, defines the shape.
    for p in out.pixels_mut() {
        let Rgba([r, g, b, a]) = *p;
        if a < 255 {
            let a = u16::from(a);
            *p = Rgba([
                (u16::from(r) * a / 255) as u8,
                (u16::from(g) * a / 255) as u8,
                (u16::from(b) * a / 255) as u8,
                255,
            ]);
        }
    }
    out.into_raw()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(rgba: &[u8], size: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * size + x) * 4) as usize;
        [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
    }

    fn is_brand_violet(p: [u8; 4]) -> bool {
        p[3] == 255 && p[2] > 140 && p[0] > 80 && p[1] + 40 < p[2]
    }

    #[test]
    fn renders_the_requested_size_with_opaque_corners_and_the_violet_mark() {
        for size in [16, 256] {
            let rgba = render(size);
            assert_eq!(rgba.len(), (size * size * 4) as usize);
            // The tile fills the canvas. Corners stay opaque near-black so
            // Windows (and the in-app header) are not a transparent hole.
            let [r, g, b, a] = pixel(&rgba, size, 0, 0);
            assert!(
                r < 16 && g < 16 && b < 20 && a == 255,
                "corner {r},{g},{b},{a}"
            );
            assert!(
                rgba.chunks(4)
                    .any(|p| is_brand_violet([p[0], p[1], p[2], p[3]])),
                "no violet at {size}"
            );
        }
        // Cropping the pad makes the mark fill the tile: at 256, violet
        // reaches outside the inner half. The old padded render did not.
        let size = 256u32;
        let rgba = render(size);
        let outside = (0..size)
            .flat_map(|y| (0..size).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let dx = x as i32 - 128;
                let dy = y as i32 - 128;
                dx * dx + dy * dy > 100 * 100
            })
            .any(|(x, y)| is_brand_violet(pixel(&rgba, size, x, y)));
        assert!(outside, "mark still sits in a padded hole");
    }

    #[test]
    fn degenerate_sizes_do_not_panic() {
        assert!(render(0).is_empty());
        assert_eq!(render(1).len(), 4);
        assert_eq!(render(1)[3], 255);
    }
}
