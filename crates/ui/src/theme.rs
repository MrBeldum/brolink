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
    /// Brand accent. Reserved for the one primary action on a screen, the
    /// selected state, and "live" indicators.
    pub accent: Color32,
    /// Text drawn on top of the accent.
    pub on_accent: Color32,
    pub success: Color32,
    pub danger: Color32,
    pub info: Color32,
}

pub const PALETTE: Palette = Palette {
    bg: Color32::from_rgb(11, 14, 19),
    surface: Color32::from_rgb(20, 25, 33),
    raised: Color32::from_rgb(29, 36, 46),
    raised_hover: Color32::from_rgb(38, 46, 58),
    border: Color32::from_rgb(34, 42, 54),
    border_strong: Color32::from_rgb(58, 69, 86),
    well: Color32::from_rgb(9, 12, 17),
    text: Color32::from_rgb(232, 236, 242),
    muted: Color32::from_rgb(143, 154, 172),
    faint: Color32::from_rgb(92, 102, 118),
    accent: Color32::from_rgb(245, 165, 36),
    on_accent: Color32::from_rgb(24, 17, 4),
    success: Color32::from_rgb(63, 185, 80),
    danger: Color32::from_rgb(248, 81, 73),
    info: Color32::from_rgb(88, 166, 255),
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
