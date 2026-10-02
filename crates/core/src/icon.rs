//! The application icon: the checked-in logo, decoded once and resized on
//! demand. Shared by the windows (via `latch_core::icon`) and by the host
//! build script, which includes this file by path, so it must not refer to
//! anything else in this crate.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use image::{Rgba, RgbaImage};

/// The 1024x1024 icon: a full-bleed ink tile with the logo knocked out in
/// white and its blue square. `scripts/make-icons.py` draws it.
const LOGO_PNG: &[u8] = include_bytes!("../assets/logo-1024.png");

/// The decoded icon. PNG inflate is the expensive part; a resize is cheap.
fn logo() -> &'static RgbaImage {
    static LOGO: OnceLock<RgbaImage> = OnceLock::new();
    LOGO.get_or_init(|| {
        image::load_from_memory(LOGO_PNG)
            .expect("bundled logo-1024.png decodes")
            .into_rgba8()
    })
}

/// Render the icon at `size` pixels square as straight (unpremultiplied) RGBA.
///
/// The tile fills the canvas. Windows (taskbar, Explorer, the window) and
/// the in-app header all draw this bitmap; macOS 26 also wants a filled
/// square and applies the rounded app-icon shape itself.
pub fn render(size: u32) -> Vec<u8> {
    // Each window's header and the window icon ask for the same size at
    // startup, before the first frame; the resize is the slow part.
    static RENDERED: Mutex<BTreeMap<u32, Vec<u8>>> = Mutex::new(BTreeMap::new());
    let mut rendered = RENDERED.lock().unwrap_or_else(|e| e.into_inner());
    rendered.entry(size).or_insert_with(|| draw(size)).clone()
}

fn draw(size: u32) -> Vec<u8> {
    if size == 0 {
        return Vec::new();
    }
    let mut out =
        image::imageops::resize(logo(), size, size, image::imageops::FilterType::Lanczos3);
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

    fn is_brand_blue(p: [u8; 4]) -> bool {
        p[3] == 255 && p[2] > 180 && p[0] < 110 && p[1] > 100 && p[1] < 180
    }

    fn is_white(p: [u8; 4]) -> bool {
        p[3] == 255 && p[0] > 240 && p[1] > 240 && p[2] > 240
    }

    #[test]
    fn renders_the_requested_size_with_an_ink_tile_and_the_white_and_blue_mark() {
        for size in [16, 256] {
            let rgba = render(size);
            assert_eq!(rgba.len(), (size * size * 4) as usize);
            // The tile fills the canvas. Corners stay opaque near-black so
            // Windows (and the in-app header) are not a transparent hole.
            let [r, g, b, a] = pixel(&rgba, size, 0, 0);
            assert!(
                r < 40 && g < 40 && b < 40 && a == 255,
                "corner {r},{g},{b},{a}"
            );
            let any = |f: fn([u8; 4]) -> bool| rgba.chunks(4).any(|p| f([p[0], p[1], p[2], p[3]]));
            assert!(any(is_brand_blue), "no blue square at {size}");
            assert!(any(is_white), "no white mark at {size}");
        }
        // The mark fills the tile: at 256 the white square's corner lies
        // well outside the inner half of the canvas.
        let size = 256u32;
        let rgba = render(size);
        let outside = (0..size)
            .flat_map(|y| (0..size).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let dx = x as i32 - 128;
                let dy = y as i32 - 128;
                dx * dx + dy * dy > 100 * 100
            })
            .any(|(x, y)| is_white(pixel(&rgba, size, x, y)));
        assert!(outside, "the mark sits in a padded hole");
    }

    #[test]
    fn degenerate_sizes_do_not_panic() {
        assert!(render(0).is_empty());
        assert_eq!(render(1).len(), 4);
        assert_eq!(render(1)[3], 255);
    }
}
