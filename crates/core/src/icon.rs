//! The application icon: the checked-in logo, decoded once and resized on
//! demand. Shared by the windows (via `brolink_core::icon`) and by the host
//! build script, which includes this file by path, so it must not refer to
//! anything else in this crate.

use std::sync::OnceLock;

/// The 1024x1024 logo, padded to square on the theme's near-black.
const LOGO_PNG: &[u8] = include_bytes!("../assets/logo-1024.png");

/// The decoded logo. PNG inflate is the expensive part; a resize is cheap.
fn logo() -> &'static image::RgbaImage {
    static LOGO: OnceLock<image::RgbaImage> = OnceLock::new();
    LOGO.get_or_init(|| {
        image::load_from_memory(LOGO_PNG)
            .expect("bundled logo-1024.png decodes")
            .into_rgba8()
    })
}

/// Render the icon at `size` pixels square as straight (unpremultiplied) RGBA.
pub fn render(size: u32) -> Vec<u8> {
    image::imageops::resize(logo(), size, size, image::imageops::FilterType::Lanczos3).into_raw()
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
    fn renders_the_requested_size_with_dark_padding_and_the_violet_mark() {
        for size in [16, 256] {
            let rgba = render(size);
            assert_eq!(rgba.len(), (size * size * 4) as usize);
            // The corners are the opaque near-black pad, not the old
            // transparent rounded tile.
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
    }

    #[test]
    fn degenerate_sizes_do_not_panic() {
        assert!(render(0).is_empty());
        assert_eq!(render(1).len(), 4);
        assert_eq!(render(1)[3], 255);
    }
}
