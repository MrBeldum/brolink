//! Palette, typography and widget styling.
//!
//! One dark theme, deliberately: the client spends its life next to a black
//! video surface, and the host is a status window people glance at. Everything
//! is derived from the handful of colours in [`Palette`], so a retune is a
//! change to one struct.

use egui::{
    epaint::Shadow, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId,
    FontTweak, Margin, Stroke, TextStyle, Vec2, Visuals,
};
use std::sync::Arc;

/// The colours the two apps are built from.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    /// Window background.
    pub bg: Color32,
    /// Cards and panels that sit on the background.
    pub surface: Color32,
    /// Inputs and buttons that sit on a card.
    pub raised: Color32,
    /// The same, hovered.
    pub raised_hover: Color32,
    /// Hairlines around cards and controls.
    pub border: Color32,
    /// Hairlines that should read a little louder (hovered controls).
    pub border_strong: Color32,
    /// Sunken areas: text boxes, the log.
    pub well: Color32,
    /// Primary text.
    pub text: Color32,
    /// Secondary text: hints, captions, metadata.
    pub muted: Color32,
    /// Tertiary text: disabled labels, placeholders.
    pub faint: Color32,
    /// Brand accent. Foreground colour: links, warnings, PIN digits, status
    /// pills and selection. Readable at 4.5:1 on both `bg` and `surface`.
    pub accent: Color32,
    /// The accent as a solid fill (the primary button, the selected segment).
    /// White text on this fill holds 4.5:1 in idle, hover and pressed states.
    pub accent_fill: Color32,
    /// Secondary brand colour from the logo (pink). Decorative only — never
    /// carries text: white on this fill is 2.87:1.
    pub accent2: Color32,
    /// Text drawn on top of the accent fill.
    pub on_accent: Color32,
    pub success: Color32,
    pub danger: Color32,
    pub info: Color32,
}

/// Logo-derived dark palette (`logo.webp`: violet / pink / cyan on near-black).
///
/// Locked tokens: bg `#07080c`, surface `#101219`, accent2 `#f05fd6`,
/// info `#22d3ee`, on_accent white. The logo violet is split into two roles:
/// `accent` (`#8d5ef6`, the `#8b5cf6` logo violet lightened) is foreground
/// text and reads 4.5:1 on both `bg` and `surface`; `accent_fill` (`#8257f6`,
/// the logo violet darkened) is the solid button fill whose white text holds
/// 4.5:1 in every state. One violet cannot do both at 4.5:1 — the luminance
/// windows are disjoint, so the roles are separate tokens.
pub const PALETTE: Palette = Palette {
    bg: Color32::from_rgb(7, 8, 12),        // #07080c
    surface: Color32::from_rgb(16, 18, 25), // #101219
    raised: Color32::from_rgb(25, 28, 38),
    raised_hover: Color32::from_rgb(34, 38, 50),
    border: Color32::from_rgb(30, 34, 46),
    border_strong: Color32::from_rgb(54, 58, 78),
    well: Color32::from_rgb(5, 6, 10),
    text: Color32::from_rgb(232, 236, 242),
    muted: Color32::from_rgb(143, 154, 172),
    faint: Color32::from_rgb(92, 102, 118),
    accent: Color32::from_rgb(141, 94, 246),      // #8d5ef6
    accent_fill: Color32::from_rgb(130, 87, 246), // #8257f6
    accent2: Color32::from_rgb(240, 95, 214),     // #f05fd6
    on_accent: Color32::WHITE,
    success: Color32::from_rgb(63, 185, 80),
    danger: Color32::from_rgb(248, 81, 73),
    info: Color32::from_rgb(34, 211, 238), // #22d3ee
};

/// Font families registered by [`apply`], for text that needs a specific
/// weight. Everything else goes through [`FontFamily::Proportional`], which
/// resolves to Inter Regular.
pub const MEDIUM: &str = "Inter-Medium";
pub const SEMIBOLD: &str = "Inter-SemiBold";

/// Named text styles beyond egui's built-ins.
pub const TITLE: &str = "title";
pub const DISPLAY: &str = "display";
pub const CAPTION: &str = "caption";

pub fn medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(MEDIUM.into()))
}

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}

pub fn title() -> TextStyle {
    TextStyle::Name(TITLE.into())
}

pub fn display() -> TextStyle {
    TextStyle::Name(DISPLAY.into())
}

pub fn caption() -> TextStyle {
    TextStyle::Name(CAPTION.into())
}

/// A stroke with an explicitly `f32` width, so float literals do not fall
/// back through `f64` (a warning under `-D warnings`).
pub fn stroke(width: f32, color: Color32) -> Stroke {
    Stroke::new(width, color)
}

/// Corner radius used for cards and other large containers.
pub const RADIUS_LG: u8 = 12;
/// Corner radius used for controls.
pub const RADIUS: u8 = 8;

/// Install the fonts and style on a context. Call once at startup.
pub fn apply(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    let mut style = (*ctx.style()).clone();
    style.visuals = visuals();
    style.text_styles = [
        (TextStyle::Small, FontId::proportional(12.0)),
        (TextStyle::Body, FontId::proportional(14.0)),
        (TextStyle::Button, medium(14.0)),
        (TextStyle::Heading, semibold(20.0)),
        (TextStyle::Monospace, FontId::monospace(13.0)),
        (title(), semibold(15.0)),
        (display(), semibold(40.0)),
        (caption(), FontId::proportional(12.5)),
    ]
    .into();
    let sp = &mut style.spacing;
    sp.item_spacing = Vec2::new(8.0, 8.0);
    sp.button_padding = Vec2::new(14.0, 7.0);
    sp.interact_size = Vec2::new(40.0, 30.0);
    sp.indent = 18.0;
    sp.slider_width = 220.0;
    sp.slider_rail_height = 6.0;
    sp.text_edit_width = 320.0;
    sp.combo_width = 140.0;
    sp.icon_width = 16.0;
    sp.icon_width_inner = 10.0;
    sp.icon_spacing = 6.0;
    sp.window_margin = Margin::same(14);
    sp.menu_margin = Margin::same(8);
    sp.tooltip_width = 360.0;
    sp.scroll.bar_width = 8.0;
    sp.scroll.floating = true;
    sp.scroll.bar_inner_margin = 4.0;
    style.url_in_tooltip = true;
    ctx.set_style(style);
}

fn fonts() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    // Inter is bundled so the two apps look the same on every machine; the
    // egui defaults stay in the family lists as fallbacks for symbols and
    // emoji Inter does not carry.
    fonts.font_data.insert(
        "Inter".into(),
        Arc::new(tweaked(FontData::from_static(include_bytes!(
            "../assets/Inter-Regular.ttf"
        )))),
    );
    fonts.font_data.insert(
        MEDIUM.into(),
        Arc::new(tweaked(FontData::from_static(include_bytes!(
            "../assets/Inter-Medium.ttf"
        )))),
    );
    fonts.font_data.insert(
        SEMIBOLD.into(),
        Arc::new(tweaked(FontData::from_static(include_bytes!(
            "../assets/Inter-SemiBold.ttf"
        )))),
    );
    let fallbacks: Vec<String> = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let with = |name: &str| {
        let mut v = vec![name.to_string()];
        v.extend(fallbacks.iter().cloned());
        v
    };
    fonts
        .families
        .insert(FontFamily::Proportional, with("Inter"));
    fonts
        .families
        .insert(FontFamily::Name(MEDIUM.into()), with(MEDIUM));
    fonts
        .families
        .insert(FontFamily::Name(SEMIBOLD.into()), with(SEMIBOLD));
    fonts
}

/// Inter sits a touch high on its line at egui's default metrics; nudge it
/// so text centres in buttons and pills.
fn tweaked(mut data: FontData) -> FontData {
    data.tweak = FontTweak {
        y_offset_factor: 0.02,
        ..Default::default()
    };
    data
}

fn visuals() -> Visuals {
    let p = PALETTE;
    let mut v = Visuals::dark();
    v.override_text_color = None;
    v.panel_fill = p.bg;
    v.window_fill = p.surface;
    v.window_stroke = stroke(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(RADIUS_LG);
    v.window_shadow = Shadow {
        offset: [0, 8],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(120),
    };
    v.popup_shadow = Shadow {
        offset: [0, 6],
        blur: 18,
        spread: 0,
        color: Color32::from_black_alpha(110),
    };
    v.menu_corner_radius = CornerRadius::same(10);
    v.extreme_bg_color = p.well;
    v.faint_bg_color = p.raised;
    v.code_bg_color = p.well;
    v.hyperlink_color = p.accent;
    v.warn_fg_color = p.accent;
    v.error_fg_color = p.danger;
    v.button_frame = true;
    v.collapsing_header_frame = false;
    v.indent_has_left_vline = false;
    v.striped = false;
    v.slider_trailing_fill = true;
    v.handle_shape = egui::style::HandleShape::Circle;
    v.selection.bg_fill = p.accent.gamma_multiply(0.35);
    v.selection.stroke = stroke(1.0, p.accent);
    v.text_cursor.stroke = stroke(2.0, p.accent);

    let w = &mut v.widgets;
    let r = CornerRadius::same(RADIUS);
    w.noninteractive.bg_fill = p.surface;
    w.noninteractive.weak_bg_fill = p.surface;
    w.noninteractive.bg_stroke = stroke(1.0, p.border);
    w.noninteractive.fg_stroke = stroke(1.0, p.text);
    w.noninteractive.corner_radius = r;

    w.inactive.bg_fill = p.raised;
    w.inactive.weak_bg_fill = p.raised;
    w.inactive.bg_stroke = stroke(1.0, p.border_strong.gamma_multiply(0.6));
    w.inactive.fg_stroke = stroke(1.0, p.text);
    w.inactive.corner_radius = r;
    w.inactive.expansion = 0.0;

    w.hovered.bg_fill = p.raised_hover;
    w.hovered.weak_bg_fill = p.raised_hover;
    w.hovered.bg_stroke = stroke(1.0, p.border_strong);
    w.hovered.fg_stroke = stroke(1.5, p.text);
    w.hovered.corner_radius = r;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = p.border_strong;
    w.active.weak_bg_fill = p.border_strong;
    w.active.bg_stroke = stroke(1.0, p.border_strong);
    w.active.fg_stroke = stroke(1.5, p.text);
    w.active.corner_radius = r;
    w.active.expansion = 0.0;

    w.open.bg_fill = p.raised_hover;
    w.open.weak_bg_fill = p.raised_hover;
    w.open.bg_stroke = stroke(1.0, p.border_strong);
    w.open.fg_stroke = stroke(1.0, p.text);
    w.open.corner_radius = r;
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lin(c: u8) -> f64 {
        let s = f64::from(c) / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    }

    fn lum(c: Color32) -> f64 {
        0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
    }

    fn contrast(fg: Color32, bg: Color32) -> f64 {
        let (a, b) = (lum(fg), lum(bg));
        let (hi, lo) = if a > b { (a, b) } else { (b, a) };
        (hi + 0.05) / (lo + 0.05)
    }

    #[test]
    fn logo_tokens_match_specified_hex() {
        let p = PALETTE;
        assert_eq!(p.bg, Color32::from_rgb(7, 8, 12));
        assert_eq!(p.surface, Color32::from_rgb(16, 18, 25));
        assert_eq!(p.accent, Color32::from_rgb(141, 94, 246));
        assert_eq!(p.accent_fill, Color32::from_rgb(130, 87, 246));
        assert_eq!(p.accent2, Color32::from_rgb(240, 95, 214));
        assert_eq!(p.info, Color32::from_rgb(34, 211, 238));
        assert_eq!(p.on_accent, Color32::WHITE);
        assert_ne!(p.well, p.bg);
    }

    #[test]
    fn text_on_surfaces_meets_aa() {
        let p = PALETTE;
        assert!(contrast(p.text, p.bg) >= 7.0);
        assert!(contrast(p.text, p.surface) >= 7.0);
        assert!(contrast(p.muted, p.bg) >= 4.5);
        assert!(contrast(p.info, p.bg) >= 4.5);
        assert!(contrast(p.accent2, p.bg) >= 4.5);
        // Foreground accent (links, warnings, pills) must read on both.
        assert!(contrast(p.accent, p.bg) >= 4.5);
        assert!(contrast(p.accent, p.surface) >= 4.5);
        // White button text on the fill must read at 4.5:1.
        assert!(contrast(p.on_accent, p.accent_fill) >= 4.5);
    }
}
