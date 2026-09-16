//! Shared controls for the lobby and the live session.
use crate::config::{Codec, Preset, Quality, Resolution, StreamSettings};
use brolink_ui::{self as ui, Tone};

pub fn stream_controls(ui: &mut egui::Ui, s: &mut StreamSettings, native: (u32, u32)) -> bool {
    let before = s.clone();
    ui::caption(ui, "QUICK PROFILES");
    ui.horizontal_wrapped(|ui| {
        for preset in Preset::ALL {
            if ui::ghost_button(ui, preset.label())
                .on_hover_text(preset.describe())
                .clicked()
            {
                s.apply_preset(preset);
            }
        }
        if ui::ghost_button(ui, "Reset").clicked() {
            *s = StreamSettings {
                app: s.app.clone(),
                fullscreen: s.fullscreen,
                ..Default::default()
            };
        }
    });
    ui::row_separator(ui);
    ui::setting_row(ui, "Picture quality", Some("Recommended starts at 35 Mbps. A relay or a long round trip never lowers your frame rate or caps your bitrate."), |ui| {
        if ui::segmented(ui, &[(Quality::Auto, "Recommended"), (Quality::Custom, "Manual")], &mut s.quality) && s.quality == Quality::Auto { s.resolution = Resolution::Native; }
    });
    ui::row_separator(ui);
    ui::setting_row(ui, "Resolution", Some("Match screen is this display’s own pixel size, so the picture fills it exactly. 1080p, 1440p and 4K are the standard 16:9 sizes; on a display of another shape they leave a bar above and below."), |ui| {
        let mut resolution = s.resolution;
        egui::ComboBox::from_id_salt("stream-resolution").selected_text(resolution.describe(native)).show_ui(ui, |ui| {
            for r in [Resolution::Native, Resolution::P1080, Resolution::P1440, Resolution::P2160] { ui.selectable_value(&mut resolution, r, r.describe(native)); }
        });
        if resolution != s.resolution { s.resolution = resolution; s.quality = Quality::Custom; }
    });
    ui::row_separator(ui);
    ui::setting_row(ui, "Frame rate", Some("60 fps is a good starting point. Higher rates need a matching host display and more bandwidth."), |ui| {
        egui::ComboBox::from_id_salt("stream-fps").selected_text(format!("{} fps", s.fps)).show_ui(ui, |ui| {
            for fps in [30, 60, 90, 120, 144, 165, 240] { ui.selectable_value(&mut s.fps, fps, format!("{fps} fps")); }
        });
    });
    ui::row_separator(ui);
    ui::setting_row(ui, "Bitrate target", Some("Received bitrate varies with screen activity. A still desktop can use very little bandwidth."), |ui| {
        let mut mbps = if s.quality == Quality::Auto { 35 } else { s.bitrate_kbps / 1000 };
        ui.spacing_mut().slider_width = ui.available_width().min(200.0);
        if ui.add(egui::Slider::new(&mut mbps, 2..=150).suffix(" Mbps")).changed() { s.quality = Quality::Custom; s.bitrate_kbps = mbps * 1000; }
    });
    ui::row_separator(ui);
    ui::setting_row(ui, "Video format", Some("HEVC gives sharper detail per megabit. Auto uses HEVC when supported; H.264 improves compatibility."), |ui| {
        ui::segmented(ui, &[(Codec::Auto, "Auto"), (Codec::Hevc, "HEVC"), (Codec::H264, "H.264")], &mut s.codec);
    });
    let effective = crate::path::effective(s, &crate::path::Path::default());
    let (w, h) = effective.resolution.pixels(native);
    ui.add_space(12.0);
    ui::status_pill(
        ui,
        &format!(
            "{w} × {h}  ·  {} fps  ·  {} Mbps target",
            effective.fps,
            effective.bitrate_kbps / 1000
        ),
        Tone::Info,
    );
    *s != before
}
