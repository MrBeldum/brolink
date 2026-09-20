//! Composite widgets the host and client screens are assembled from.
//!
//! Everything here is drawn from [`PALETTE`](crate::PALETTE) and the text
//! styles registered by [`apply`](crate::apply); none of it carries its own
//! colours, so the two apps stay in step.

use crate::theme::{self, stroke, PALETTE as P, RADIUS, RADIUS_LG};
use egui::{
    collapsing_header::CollapsingState, Align, Color32, ColorImage, CornerRadius, Frame, Id,
    InnerResponse, Label, Layout, Margin, Rect, Response, RichText, Sense, Stroke, StrokeKind,
    TextureHandle, TextureOptions, Ui, UiBuilder, Vec2, WidgetInfo, WidgetType,
};

/// Semantic colour for pills, notices and status text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Accent,
    Success,
    Danger,
    Info,
}

impl Tone {
    pub fn color(self) -> Color32 {
        match self {
            Tone::Neutral => P.muted,
            Tone::Accent => P.accent,
            Tone::Success => P.success,
            Tone::Danger => P.danger,
            Tone::Info => P.info,
        }
    }
}

// ---------------------------------------------------------------------------
// Containers
// ---------------------------------------------------------------------------

/// The frame every card is drawn with.
pub fn card_frame() -> Frame {
    Frame::new()
        .fill(P.surface)
        .stroke(stroke(1.0, P.border))
        .corner_radius(CornerRadius::same(RADIUS_LG))
        .inner_margin(Margin::same(18))
}

/// A full-width surface with a hairline border.
pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> InnerResponse<R> {
    card_frame().show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        add(ui)
    })
}

/// A card whose border carries a tone, for the one thing on screen that
/// needs attention right now (a pairing PIN, a setup problem).
pub fn toned_card<R>(ui: &mut Ui, tone: Tone, add: impl FnOnce(&mut Ui) -> R) -> InnerResponse<R> {
    card_frame()
        .stroke(stroke(1.0, tone.color().gamma_multiply(0.7)))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            add(ui)
        })
}

/// Card title with an optional one-line explanation underneath.
pub fn heading(ui: &mut Ui, title: &str, subtitle: Option<&str>) {
    ui.label(
        RichText::new(title)
            .text_style(theme::title())
            .color(P.text),
    );
    if let Some(s) = subtitle {
        ui.add_space(-2.0);
        caption(ui, s);
    }
    ui.add_space(8.0);
}

/// [`card`] with a [`heading`].
pub fn titled_card<R>(
    ui: &mut Ui,
    title: &str,
    subtitle: Option<&str>,
    add: impl FnOnce(&mut Ui) -> R,
) -> InnerResponse<R> {
    card(ui, |ui| {
        heading(ui, title, subtitle);
        add(ui)
    })
}

/// A sunken area for a log or a block of monospace text.
pub fn well<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> InnerResponse<R> {
    Frame::new()
        .fill(P.well)
        .stroke(stroke(1.0, P.border))
        .corner_radius(CornerRadius::same(RADIUS))
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            add(ui)
        })
}

/// A scrolling log in a [`well`], newest line at the bottom.
pub fn log_view(ui: &mut Ui, id: impl std::hash::Hash, lines: &[String], max_height: f32) {
    well(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 3.0;
        egui::ScrollArea::vertical()
            .id_salt(id)
            .max_height(max_height)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                if lines.is_empty() {
                    ui.label(RichText::new("Nothing yet.").monospace().color(P.faint));
                }
                for line in lines {
                    ui.label(RichText::new(line).monospace().color(P.muted));
                }
            });
    });
}

/// A card that opens and closes from its title row. `id` is global to the
/// context, not to the enclosing `Ui`; use distinct ids within an app.
pub fn collapsible<R>(
    ui: &mut Ui,
    id: impl std::hash::Hash,
    title: &str,
    default_open: bool,
    body: impl FnOnce(&mut Ui) -> R,
) -> Option<R> {
    let id = collapsible_id(id);
    card(ui, |ui| {
        let state = CollapsingState::load_with_default_open(ui.ctx(), id, default_open);
        let (_, _, body) = state
            .show_header(ui, |ui| {
                ui.label(
                    RichText::new(title)
                        .text_style(theme::title())
                        .color(P.text),
                );
            })
            .body_unindented(|ui| {
                ui.add_space(8.0);
                body(ui)
            });
        body.map(|b| b.inner)
    })
    .inner
}

fn collapsible_id(id: impl std::hash::Hash) -> Id {
    Id::new("brolink.collapsible").with(id)
}

/// Centre the content in a column no wider than `max_width`.
pub fn content_column<R>(ui: &mut Ui, max_width: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let avail = ui.available_width();
    let w = (avail - 40.0).max(0.0).min(max_width);
    let pad = ((avail - w) / 2.0).max(0.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.add_space(pad);
        ui.vertical(|ui| {
            ui.set_width(w);
            add(ui)
        })
        .inner
    })
    .inner
}

/// The bar across the top of a window.
pub fn top_bar<R>(ctx: &egui::Context, id: impl Into<Id>, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::TopBottomPanel::top(id)
        .frame(
            Frame::new()
                .fill(P.bg)
                .inner_margin(Margin::symmetric(20, 12)),
        )
        .show_separator_line(true)
        .show(ctx, add)
        .inner
}

/// The bar across the bottom of a window: version, ports, other small print.
pub fn bottom_bar<R>(ctx: &egui::Context, id: impl Into<Id>, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::TopBottomPanel::bottom(id)
        .frame(
            Frame::new()
                .fill(P.bg)
                .inner_margin(Margin::symmetric(20, 8)),
        )
        .show_separator_line(true)
        .show(ctx, |ui| {
            ui.style_mut().override_text_style = Some(theme::caption());
            ui.visuals_mut().override_text_color = Some(P.faint);
            add(ui)
        })
        .inner
}

/// The translucent frame the stream overlay sits in.
pub fn overlay_frame() -> Frame {
    Frame::new()
        .fill(Color32::from_black_alpha(196))
        .stroke(stroke(1.0, Color32::from_white_alpha(26)))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(12, 8))
}

// ---------------------------------------------------------------------------
// Text
// ---------------------------------------------------------------------------

/// Secondary text: hints, help, metadata.
pub fn caption(ui: &mut Ui, text: impl Into<String>) -> Response {
    ui.label(
        RichText::new(text)
            .text_style(theme::caption())
            .color(P.muted),
    )
}

/// Body text in the muted colour.
pub fn muted(ui: &mut Ui, text: impl Into<String>) -> Response {
    ui.label(RichText::new(text).color(P.muted))
}

/// Slightly heavier body text, for the name in a list row.
pub fn strong(ui: &mut Ui, text: impl Into<String>) -> Response {
    ui.label(RichText::new(text).font(theme::medium(14.0)).color(P.text))
}

/// A large, letter-spaced code such as a pairing PIN.
pub fn display_digits(ui: &mut Ui, digits: &str) -> Response {
    ui.label(
        RichText::new(digits)
            .text_style(theme::display())
            .extra_letter_spacing(6.0)
            .color(P.accent),
    )
}

/// Vertically centred, muted text for an empty list. Wraps: a line that
/// ran past the card would widen everything laid out after it.
pub fn empty_state(ui: &mut Ui, text: &str, busy: bool) {
    ui.horizontal_wrapped(|ui| {
        if busy {
            ui.add(spinner(14.0, P.faint));
        }
        ui.label(RichText::new(text).color(P.muted));
    });
}

/// An inline message tinted by tone. Errors, warnings, confirmations.
pub fn notice(ui: &mut Ui, tone: Tone, text: &str) -> Response {
    let color = tone.color();
    Frame::new()
        .fill(color.gamma_multiply(0.12))
        .stroke(stroke(1.0, color.gamma_multiply(0.5)))
        .corner_radius(CornerRadius::same(RADIUS))
        .inner_margin(Margin::symmetric(12, 9))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("●").size(9.0).color(color));
                ui.label(RichText::new(text).color(P.text));
            });
        })
        .response
}

// ---------------------------------------------------------------------------
// Key / value
// ---------------------------------------------------------------------------

/// Aligned key/value rows. A value wraps inside the card: an `egui::Grid`
/// extends instead, and one row wider than the card widens every widget
/// laid out after it (the card, the rows below, the toggles off-screen).
pub fn kv_grid(ui: &mut Ui, rows: &[(&str, String)]) {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let key_w = rows
        .iter()
        .map(|(k, _)| {
            ui.painter()
                .layout_no_wrap((*k).to_owned(), font.clone(), Color32::PLACEHOLDER)
                .size()
                .x
        })
        .fold(0.0_f32, f32::max);
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(18.0, 6.0);
        for (k, v) in rows {
            ui.horizontal_top(|ui| {
                let key = ui.label(RichText::new(*k).color(P.muted));
                ui.add_space((key_w - key.rect.width()).max(0.0));
                ui.add(Label::new(RichText::new(v).color(P.text)).wrap());
            });
        }
    });
}

// ---------------------------------------------------------------------------
// Buttons
// ---------------------------------------------------------------------------

fn text_button(
    ui: &mut Ui,
    label: &str,
    font: egui::FontId,
    pad: Vec2,
    paint: impl FnOnce(&Ui, &Response, egui::Rect) -> Color32,
) -> Response {
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font, Color32::PLACEHOLDER);
    let mut size = galley.size() + 2.0 * pad;
    size.y = size.y.max(ui.spacing().interact_size.y);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), label));
    if ui.is_rect_visible(rect) {
        let fg = paint(ui, &response, rect);
        let pos = rect.center() - galley.size() / 2.0;
        ui.painter().galley(pos, galley, fg);
        focus_ring(ui, &response, RADIUS);
    }
    response
}

/// Keep keyboard navigation visible for controls with custom painting.
fn focus_ring(ui: &Ui, response: &Response, radius: u8) {
    if response.has_focus() && ui.is_enabled() {
        ui.painter().rect_stroke(
            response.rect.shrink(2.0),
            CornerRadius::same(radius.saturating_sub(2)),
            stroke(2.0, P.text),
            StrokeKind::Inside,
        );
    }
}

/// The one accent-filled button on a screen.
pub fn primary_button(ui: &mut Ui, label: &str) -> Response {
    filled_button(ui, label, P.accent_fill, P.on_accent)
}

/// A filled button in a tone other than the accent: red for the confirm
/// step of a destructive action.
pub fn toned_button(ui: &mut Ui, label: &str, tone: Tone) -> Response {
    filled_button(ui, label, tone.color(), P.text)
}

fn filled_button(ui: &mut Ui, label: &str, color: Color32, fg: Color32) -> Response {
    let pad = ui.spacing().button_padding + Vec2::new(4.0, 1.0);
    text_button(ui, label, theme::medium(14.0), pad, |ui, r, rect| {
        let enabled = ui.is_enabled();
        let (fill, border, text) = if !enabled {
            (P.raised, P.border_strong.gamma_multiply(0.6), P.faint)
        } else if r.is_pointer_button_down_on() {
            (
                lerp_color(color, Color32::BLACK, 0.12),
                Color32::TRANSPARENT,
                fg,
            )
        } else if r.hovered() {
            (
                lerp_color(color, Color32::BLACK, 0.10),
                Color32::TRANSPARENT,
                fg,
            )
        } else {
            (color, Color32::TRANSPARENT, fg)
        };
        ui.painter().rect(
            rect,
            CornerRadius::same(RADIUS),
            fill,
            stroke(1.0, border),
            StrokeKind::Inside,
        );
        text
    })
}

/// A menu button with a painted chevron; the bundled fonts carry no
/// triangle glyph.
pub fn menu_button<R>(
    ui: &mut Ui,
    label: &str,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> Option<R> {
    let r = ui.menu_button(format!("{label}     "), add_contents);
    let rect = r.response.rect;
    let c = egui::pos2(rect.right() - 13.0, rect.center().y - 1.0);
    let col = ui.style().interact(&r.response).fg_stroke.color;
    let s = stroke(1.5, col);
    ui.painter().line_segment(
        [egui::pos2(c.x - 4.0, c.y - 2.0), egui::pos2(c.x, c.y + 2.0)],
        s,
    );
    ui.painter().line_segment(
        [egui::pos2(c.x, c.y + 2.0), egui::pos2(c.x + 4.0, c.y - 2.0)],
        s,
    );
    r.inner
}

/// A quiet button: text only until hovered.
pub fn ghost_button(ui: &mut Ui, label: &str) -> Response {
    ghost(ui, label, P.muted, P.text)
}

/// A quiet button for a destructive action; turns red on hover.
pub fn danger_button(ui: &mut Ui, label: &str) -> Response {
    ghost(ui, label, P.muted, P.danger)
}

fn ghost(ui: &mut Ui, label: &str, idle: Color32, hot: Color32) -> Response {
    let pad = Vec2::new(10.0, 6.0);
    text_button(ui, label, theme::medium(13.5), pad, |ui, r, rect| {
        let enabled = ui.is_enabled();
        if enabled && (r.hovered() || r.is_pointer_button_down_on()) {
            ui.painter().rect(
                rect,
                CornerRadius::same(RADIUS),
                P.raised_hover,
                Stroke::NONE,
                StrokeKind::Inside,
            );
            hot
        } else if enabled {
            idle
        } else {
            P.faint
        }
    })
}

/// A row of mutually exclusive choices; the selected one is filled with the
/// accent. Returns true when the selection changed.
///
/// Sized and allocated as one block, so it sits correctly in any parent
/// layout (including the right-to-left side of a [`setting_row`]).
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut Ui,
    options: &[(T, &str)],
    selected: &mut T,
) -> bool {
    let font = theme::medium(13.5);
    let pad_x = 14.0;
    let h = ui.spacing().interact_size.y;
    let galleys: Vec<_> = options
        .iter()
        .map(|(_, label)| {
            ui.painter()
                .layout_no_wrap((*label).to_owned(), font.clone(), Color32::PLACEHOLDER)
        })
        .collect();
    let widths: Vec<f32> = galleys.iter().map(|g| g.size().x + 2.0 * pad_x).collect();
    let total = Vec2::new(widths.iter().sum(), h);
    let (rect, block) = ui.allocate_exact_size(total, Sense::hover());
    let visible = ui.is_rect_visible(rect);
    let n = options.len();
    let mut changed = false;
    let mut x = rect.left();
    for (i, ((value, label), galley)) in options.iter().zip(galleys).enumerate() {
        let seg = egui::Rect::from_min_size(egui::pos2(x, rect.top()), Vec2::new(widths[i], h));
        x += widths[i];
        let resp = ui.interact(seg, block.id.with(i), Sense::click());
        resp.widget_info(|| {
            WidgetInfo::selected(
                WidgetType::RadioButton,
                ui.is_enabled(),
                *selected == *value,
                *label,
            )
        });
        if resp.clicked() && *selected != *value {
            *selected = *value;
            changed = true;
        }
        if !visible {
            continue;
        }
        let on = *selected == *value;
        let r = RADIUS;
        let corner = CornerRadius {
            nw: if i == 0 { r } else { 0 },
            sw: if i == 0 { r } else { 0 },
            ne: if i + 1 == n { r } else { 0 },
            se: if i + 1 == n { r } else { 0 },
        };
        let (fill, fg) = if on {
            (P.accent_fill, P.on_accent)
        } else if resp.hovered() {
            (P.raised_hover, P.text)
        } else {
            (P.raised, P.muted)
        };
        ui.painter()
            .rect(seg, corner, fill, Stroke::NONE, StrokeKind::Inside);
        ui.painter()
            .galley(seg.center() - galley.size() / 2.0, galley, fg);
        focus_ring(ui, &resp, RADIUS);
    }
    if visible {
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(RADIUS),
            stroke(1.0, P.border_strong.gamma_multiply(0.6)),
            StrokeKind::Inside,
        );
    }
    changed
}

/// An on/off switch. Returns the response; `changed()` fires on toggle.
pub fn toggle(ui: &mut Ui, on: &mut bool) -> Response {
    labelled_toggle(ui, on, "")
}

fn labelled_toggle(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let size = Vec2::new(40.0, 22.0);
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response
        .widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, ui.is_enabled(), *on, label));
    if ui.is_rect_visible(rect) {
        let how_on = ui.ctx().animate_bool_responsive(response.id, *on);
        let enabled = ui.is_enabled();
        let off = if response.hovered() {
            P.raised_hover
        } else {
            P.raised
        };
        let mut track = lerp_color(off, P.accent_fill, how_on);
        let mut knob = lerp_color(P.muted, P.on_accent, how_on);
        if !enabled {
            track = track.gamma_multiply(0.5);
            knob = knob.gamma_multiply(0.5);
        }
        let radius = rect.height() / 2.0;
        ui.painter().rect(
            rect,
            CornerRadius::same(radius as u8),
            track,
            stroke(1.0, P.border_strong.gamma_multiply(0.6 * (1.0 - how_on))),
            StrokeKind::Inside,
        );
        let x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), how_on);
        ui.painter()
            .circle_filled(egui::pos2(x, rect.center().y), radius - 4.0, knob);
        focus_ring(ui, &response, radius as u8);
    }
    response
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(
        l(a.r(), b.r()),
        l(a.g(), b.g()),
        l(a.b(), b.b()),
        l(a.a(), b.a()),
    )
}

// ---------------------------------------------------------------------------
// Settings rows
// ---------------------------------------------------------------------------

/// A row with text on the left and controls on the right. The controls are
/// laid out first, so the text gets exactly the width they leave and wraps
/// there instead of running underneath them. When they leave too little,
/// the text takes the full width below the controls.
fn split_row<R>(
    ui: &mut Ui,
    min_height: f32,
    right: impl FnOnce(&mut Ui) -> R,
    left: impl FnOnce(&mut Ui),
) -> R {
    let avail = ui.available_rect_before_wrap();
    let row = Rect::from_min_size(avail.min, Vec2::new(avail.width(), min_height));
    let mut right_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(row)
            .layout(Layout::right_to_left(Align::Center)),
    );
    right_ui.spacing_mut().item_spacing.x = 6.0;
    let r = right(&mut right_ui);
    let used = right_ui.min_rect();
    let taken = if used.is_positive() {
        row.max.x - used.min.x + 14.0
    } else {
        0.0
    };
    let beside = row.width() - taken;
    let text_rect = if beside >= MIN_TEXT_BESIDE_CONTROLS || taken == 0.0 {
        Rect::from_min_size(row.min, Vec2::new(beside, min_height))
    } else {
        Rect::from_min_size(
            egui::pos2(row.min.x, used.max.y + 6.0),
            Vec2::new(row.width(), min_height),
        )
    };
    let mut left_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(text_rect)
            .layout(Layout::top_down(Align::Min)),
    );
    left_ui.spacing_mut().item_spacing.y = 1.0;
    left(&mut left_ui);
    let bottom = left_ui
        .min_rect()
        .max
        .y
        .max(used.max.y)
        .max(row.min.y + min_height);
    ui.allocate_rect(
        Rect::from_min_max(row.min, egui::pos2(row.max.x, bottom)),
        Sense::hover(),
    );
    r
}

/// Below this, the text goes under the controls instead of beside them.
const MIN_TEXT_BESIDE_CONTROLS: f32 = 160.0;

/// Label (and optional hint) on the left, a control on the right.
pub fn setting_row<R>(
    ui: &mut Ui,
    label: &str,
    hint: Option<&str>,
    control: impl FnOnce(&mut Ui) -> R,
) -> R {
    split_row(ui, 30.0, control, |ui| {
        ui.label(RichText::new(label).color(P.text));
        if let Some(h) = hint {
            caption(ui, h);
        }
    })
}

/// Third-party notices, as bundled at build time. Both windows show these.
pub const NOTICES: &str = include_str!("../../../NOTICE");

/// The Settings "Open source" row: a button that unfolds [`NOTICES`] in a
/// scrolling well beneath the row.
pub fn open_source_row(ui: &mut Ui, open: &mut bool) {
    setting_row(
        ui,
        "Open source",
        Some("BroLink is GPL-3.0 and builds on other free software."),
        |ui| {
            if ghost_button(
                ui,
                if *open {
                    "Hide notices"
                } else {
                    "Show notices"
                },
            )
            .clicked()
            {
                *open = !*open;
            }
        },
    );
    if *open {
        well(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("open_source_notices")
                .min_scrolled_height(160.0)
                .max_height(240.0)
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(NOTICES)
                            .text_style(theme::caption())
                            .color(P.muted),
                    );
                });
        });
    }
}

/// A [`setting_row`] whose control is a [`toggle`]. Returns true on change.
pub fn toggle_row(ui: &mut Ui, on: &mut bool, label: &str, hint: Option<&str>) -> bool {
    setting_row(ui, label, hint, |ui| {
        labelled_toggle(ui, on, label).changed()
    })
}

// ---------------------------------------------------------------------------
// Lists
// ---------------------------------------------------------------------------

/// A row in a list of PCs: name and detail on the left, actions on the
/// right. Add the primary action first; it lands rightmost. The detail
/// stays on one line and is cut with an ellipsis rather than wrapping.
pub fn list_row(ui: &mut Ui, title: &str, detail: &str, actions: impl FnOnce(&mut Ui)) {
    split_row(ui, 40.0, actions, |ui| {
        strong(ui, title);
        ui.add(
            Label::new(
                RichText::new(detail)
                    .text_style(theme::caption())
                    .color(P.muted),
            )
            .truncate(),
        )
        .on_hover_text(detail);
    });
}

/// Hairline between list rows.
pub fn row_separator(ui: &mut Ui) {
    ui.add_space(4.0);
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 1.0), Sense::hover());
    ui.painter()
        .hline(rect.x_range(), rect.center().y, stroke(1.0, P.border));
    ui.add_space(4.0);
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

/// A rounded chip with a coloured dot: READY, STREAMING, and so on.
pub fn status_pill(ui: &mut Ui, label: &str, tone: Tone) -> Response {
    let color = tone.color();
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), theme::medium(12.5), color);
    let dot = 7.0;
    let gap = 7.0;
    let pad = Vec2::new(10.0, 5.0);
    let size = Vec2::new(
        galley.size().x + dot + gap + 2.0 * pad.x,
        (galley.size().y + 2.0 * pad.y).max(26.0),
    );
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter().rect(
            rect,
            CornerRadius::same(13),
            color.gamma_multiply(0.14),
            stroke(1.0, color.gamma_multiply(0.45)),
            StrokeKind::Inside,
        );
        ui.painter().circle_filled(
            egui::pos2(rect.left() + pad.x + dot / 2.0, rect.center().y),
            dot / 2.0,
            color,
        );
        ui.painter().galley(
            egui::pos2(
                rect.left() + pad.x + dot + gap,
                rect.center().y - galley.size().y / 2.0,
            ),
            galley,
            color,
        );
    }
    response
}

/// A small coloured dot followed by text, for inline status lines.
pub fn dot_label(ui: &mut Ui, tone: Tone, text: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(RichText::new("●").size(9.0).color(tone.color()));
        ui.label(RichText::new(text).color(P.text));
    });
}

// ---------------------------------------------------------------------------
// Brand
// ---------------------------------------------------------------------------

/// The app icon as a texture, for the window header.
pub struct Brand {
    tex: TextureHandle,
}

impl Brand {
    pub fn new(ctx: &egui::Context) -> Self {
        let px = 64;
        let img =
            ColorImage::from_rgba_unmultiplied([px, px], &brolink_core::icon::render(px as u32));
        Self {
            tex: ctx.load_texture("brolink-brand", img, TextureOptions::LINEAR),
        }
    }

    /// Icon and product name on the left; `right` is laid out from the
    /// right edge.
    pub fn header(&self, ui: &mut Ui, title: &str, right: impl FnOnce(&mut Ui)) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            ui.add(egui::Image::new((self.tex.id(), Vec2::splat(28.0))));
            ui.label(
                RichText::new(title)
                    .font(theme::semibold(17.0))
                    .color(P.text),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), right);
        });
    }
}

/// A paced activity indicator: unlike egui's immediate-repaint spinner this
/// stays bounded when the low-latency stream renderer has vsync disabled.
pub fn spinner(size: f32, color: egui::Color32) -> impl egui::Widget {
    move |ui: &mut egui::Ui| {
        let (rect, response) =
            ui.allocate_exact_size(egui::Vec2::splat(size), egui::Sense::hover());
        if ui.is_rect_visible(rect) {
            let time = ui.input(|i| i.time) as f32;
            let points = (0..=20)
                .map(|i| {
                    let angle = time * 4.0 + i as f32 * std::f32::consts::PI / 15.0;
                    rect.center() + egui::vec2(angle.cos(), angle.sin()) * (size * 0.4)
                })
                .collect();
            ui.painter()
                .add(egui::Shape::line(points, egui::Stroke::new(2.0_f32, color)));
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(33));
        }
        response
    }
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

    fn contrast(fg: Color32, bg: Color32) -> f64 {
        let lum = |c: Color32| 0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b());
        let (a, b) = (lum(fg), lum(bg));
        let (hi, lo) = if a > b { (a, b) } else { (b, a) };
        (hi + 0.05) / (lo + 0.05)
    }

    #[test]
    fn accent_button_text_meets_aa_in_all_states() {
        let idle = P.accent_fill;
        let hovered = lerp_color(idle, Color32::BLACK, 0.10);
        let pressed = lerp_color(idle, Color32::BLACK, 0.12);
        assert!(contrast(P.on_accent, idle) >= 4.5);
        assert!(contrast(P.on_accent, hovered) >= 4.5);
        assert!(contrast(P.on_accent, pressed) >= 4.5);
        // Hover must darken, never lighten: lightening drops the ratio below AA.
        assert!(contrast(P.on_accent, lerp_color(idle, Color32::WHITE, 0.10)) < 4.5);
    }
}
