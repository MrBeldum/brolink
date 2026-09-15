//! Pixel sizes of the screens a Mac can have, and the sizes a stream asks
//! for. The Mac asks the PC for its own screen, or for a smaller size with
//! the same proportions, and a PC whose only display is virtual can only
//! switch to a size that display lists. Setup on the PC lists these.

/// The long-edge limits behind the quality choices: 1080p, 1440p and 4K.
/// "Match screen" has no limit.
pub const LIMITS: [u32; 3] = [1920, 2560, 3840];

/// Native pixel sizes of Apple displays since 2015, then the common
/// external monitors. Points times the native scale factor, whatever
/// "looks like" resolution the Mac is set to.
pub const SCREENS: &[(u32, u32)] = &[
    (2304, 1440), // MacBook 12"
    (2560, 1600), // MacBook Pro 13" 2016–2022, MacBook Air 13" 2018–2020
    (2560, 1664), // MacBook Air 13" M2 and later
    (2880, 1800), // MacBook Pro 15" 2015–2019
    (2880, 1864), // MacBook Air 15"
    (3024, 1964), // MacBook Pro 14"
    (3072, 1920), // MacBook Pro 16" 2019
    (3456, 2234), // MacBook Pro 16" 2021 and later
    (4096, 2304), // iMac 21.5" 4K
    (4480, 2520), // iMac 24"
    (5120, 2880), // iMac 27" 5K, Studio Display
    (6016, 3384), // Pro Display XDR
    (1920, 1080),
    (1920, 1200),
    (2560, 1080),
    (2560, 1440),
    (3440, 1440),
    (3840, 1600),
    (3840, 2160),
    (5120, 1440),
    (5120, 2160),
];

/// `native` scaled down so its long edge is at most `limit`, proportions
/// kept and both sides even (video codecs need that). Never scaled up:
/// a screen smaller than the limit is asked for as it is.
pub fn fit(limit: u32, native: (u32, u32)) -> (u32, u32) {
    let (w, h) = (native.0.max(2), native.1.max(2));
    let scale = (limit as f64 / w.max(h) as f64).min(1.0);
    let even = |v: u32| ((v as f64 * scale).round() as u32 / 2 * 2).max(2);
    (even(w), even(h))
}

/// Every size a Mac may ask for: each screen as it is and at each limit,
/// without repeats, largest first.
pub fn stream_modes() -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = SCREENS
        .iter()
        .flat_map(|&s| {
            LIMITS
                .iter()
                .map(move |&l| fit(l, s))
                .chain(std::iter::once(fit(u32::MAX, s)))
        })
        .collect();
    out.sort_by(|a, b| (b.0 * b.1).cmp(&(a.0 * a.1)).then(b.cmp(a)));
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_keeps_proportions_rounds_even_and_never_enlarges() {
        assert_eq!(fit(1920, (3024, 1964)), (1920, 1246));
        assert_eq!(fit(2560, (3024, 1964)), (2560, 1662));
        assert_eq!(fit(3840, (3024, 1964)), (3024, 1964));
        assert_eq!(fit(u32::MAX, (3024, 1964)), (3024, 1964));
        assert_eq!(fit(1920, (3840, 2160)), (1920, 1080));
        assert_eq!(fit(1920, (1280, 800)), (1280, 800), "never scaled up");
        assert_eq!(
            fit(1920, (0, 0)),
            (2, 2),
            "a degenerate screen is not a panic"
        );
        for &s in SCREENS {
            for l in LIMITS {
                let (w, h) = fit(l, s);
                assert!(w.max(h) <= l && w % 2 == 0 && h % 2 == 0, "{s:?} at {l}");
            }
        }
    }

    #[test]
    fn stream_modes_cover_every_screen_at_every_quality_without_repeats() {
        let modes = stream_modes();
        assert!(modes.contains(&(3024, 1964)));
        assert!(modes.contains(&(1920, 1246)));
        assert!(modes.contains(&(2560, 1662)));
        assert!(modes.contains(&(1920, 1080)));
        let mut sorted = modes.clone();
        sorted.dedup();
        assert_eq!(sorted, modes, "no repeats");
        assert_eq!(modes[0], (6016, 3384), "largest first");
        assert!(
            modes.len() < 90,
            "{} modes is more than a display driver should be asked to list",
            modes.len()
        );
    }
}
