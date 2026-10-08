//! A small, high-contrast dark palette. Selection, actions and voice state
//! have distinct treatments; status is always also written in text.
use egui::{Color32, Stroke, TextStyle, Vec2};

pub(super) const BACKGROUND: Color32 = Color32::from_rgb(18, 21, 28);
pub(super) const PANEL: Color32 = Color32::from_rgb(24, 28, 36);
pub(super) const SURFACE: Color32 = Color32::from_rgb(29, 34, 44);
pub(super) const BORDER: Color32 = Color32::from_rgb(48, 55, 70);
pub(super) const TEXT: Color32 = Color32::from_rgb(238, 241, 248);
pub(super) const SECONDARY: Color32 = Color32::from_rgb(162, 173, 192);
pub(super) const ACCENT: Color32 = Color32::from_rgb(153, 176, 255);
pub(super) const ON_ACCENT: Color32 = Color32::from_rgb(17, 24, 46);
pub(super) const SUCCESS: Color32 = Color32::from_rgb(110, 218, 172);
pub(super) const WARNING: Color32 = Color32::from_rgb(244, 202, 121);
pub(super) const DANGER: Color32 = Color32::from_rgb(255, 151, 163);
pub(super) const SPEAKING_BG: Color32 = Color32::from_rgb(31, 54, 49);
pub(super) const AVATAR: Color32 = Color32::from_rgb(46, 54, 73);

pub(super) fn install(ctx: &egui::Context) {
    let mut fonts = fastframe_fonts::FontSetup::default()
        .weights(&[fastframe_fonts::Weight::SemiBold])
        .definitions();
    fastframe_text::detect().apply_to(&mut fonts);
    ctx.set_fonts(fonts);
    let mut style = egui::Style {
        visuals: egui::Visuals::dark(),
        ..Default::default()
    };
    style.visuals.override_text_color = Some(TEXT);
    style.visuals.panel_fill = BACKGROUND;
    style.visuals.window_fill = PANEL;
    style.visuals.extreme_bg_color = BACKGROUND;
    style.visuals.faint_bg_color = SURFACE;
    style.visuals.selection.bg_fill = Color32::from_rgb(51, 65, 105);
    style.visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    style.visuals.hyperlink_color = ACCENT;
    style.visuals.warn_fg_color = WARNING;
    style.visuals.error_fg_color = DANGER;
    style.visuals.widgets.noninteractive.bg_fill = PANEL;
    style.visuals.widgets.noninteractive.weak_bg_fill = PANEL;
    style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    style.visuals.widgets.inactive.bg_fill = SURFACE;
    style.visuals.widgets.inactive.weak_bg_fill = SURFACE;
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    style.visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(45, 52, 68);
    style.visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(45, 52, 68);
    style.visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    style.visuals.widgets.active.bg_fill = Color32::from_rgb(58, 70, 99);
    style.visuals.widgets.active.fg_stroke = Stroke::new(1.0, TEXT);
    style.visuals.widgets.active.bg_stroke = Stroke::new(1.5, ACCENT);
    style.spacing.item_spacing = Vec2::new(8.0, 6.0);
    style.spacing.button_padding = Vec2::new(10.0, 6.0);
    style.spacing.interact_size = Vec2::new(34.0, 30.0);
    style.spacing.slider_width = 105.0;
    style
        .text_styles
        .insert(TextStyle::Body, egui::FontId::proportional(13.0));
    style
        .text_styles
        .insert(TextStyle::Button, egui::FontId::proportional(13.0));
    style
        .text_styles
        .insert(TextStyle::Small, egui::FontId::proportional(11.0));
    style.text_styles.insert(
        TextStyle::Heading,
        fastframe_fonts::Weight::SemiBold.font_id(22.0),
    );
    // Input interactions repaint; decorative animation does not create an
    // idle redraw loop or keep a sleeping voice client active.
    style.animation_time = 0.0;
    ctx.set_theme(egui::Theme::Dark);
    ctx.set_style_of(egui::Theme::Dark, style);
}
