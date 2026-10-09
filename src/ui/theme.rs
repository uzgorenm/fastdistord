//! A small, high-contrast dark palette. Selection, actions and voice state
//! have distinct treatments; status is always also written in text.
use egui::{Color32, Stroke, TextStyle, Vec2};

pub(super) const BACKGROUND: Color32 = Color32::from_rgb(22, 23, 27);
pub(super) const PANEL: Color32 = Color32::from_rgb(27, 28, 33);
pub(super) const SURFACE: Color32 = Color32::from_rgb(35, 37, 44);
pub(super) const BORDER: Color32 = Color32::from_rgb(57, 60, 69);
pub(super) const TEXT: Color32 = Color32::from_rgb(238, 241, 248);
pub(super) const SECONDARY: Color32 = Color32::from_rgb(177, 183, 198);
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
    style.visuals.selection.bg_fill = Color32::from_rgb(47, 55, 79);
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
    style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(43, 46, 55);
    style.visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(43, 46, 55);
    style.visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    style.visuals.widgets.active.bg_fill = Color32::from_rgb(55, 63, 87);
    style.visuals.widgets.active.fg_stroke = Stroke::new(1.0, TEXT);
    style.visuals.widgets.active.bg_stroke = Stroke::new(1.5, ACCENT);
    style.visuals.window_corner_radius = egui::CornerRadius::same(12);
    style.visuals.menu_corner_radius = egui::CornerRadius::same(10);
    for widget in [
        &mut style.visuals.widgets.noninteractive,
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
        &mut style.visuals.widgets.open,
    ] {
        widget.corner_radius = egui::CornerRadius::same(8);
    }
    style.spacing.item_spacing = Vec2::new(10.0, 8.0);
    style.spacing.button_padding = Vec2::new(12.0, 8.0);
    style.spacing.interact_size = Vec2::new(36.0, 34.0);
    style.spacing.slider_width = 105.0;
    style
        .text_styles
        .insert(TextStyle::Body, egui::FontId::proportional(14.0));
    style
        .text_styles
        .insert(TextStyle::Button, egui::FontId::proportional(14.0));
    style
        .text_styles
        .insert(TextStyle::Small, egui::FontId::proportional(12.0));
    style.text_styles.insert(
        TextStyle::Heading,
        fastframe_fonts::Weight::SemiBold.font_id(20.0),
    );
    // Input interactions repaint; decorative animation does not create an
    // idle redraw loop or keep a sleeping voice client active.
    style.animation_time = 0.0;
    ctx.set_theme(egui::Theme::Dark);
    ctx.set_style_of(egui::Theme::Dark, style);
}
