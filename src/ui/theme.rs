//! Small dark and light palettes. Selection, actions and voice state
//! have distinct treatments; status is always also written in text.
use crate::preferences::{Appearance, MessageDensity};
use egui::{Color32, Stroke, TextStyle, Vec2};

#[derive(Clone, Copy)]
pub(super) struct Palette {
    pub background: Color32,
    pub panel: Color32,
    pub surface: Color32,
    pub border: Color32,
    pub text: Color32,
    pub secondary: Color32,
    pub accent: Color32,
    pub on_accent: Color32,
    pub success: Color32,
    pub warning: Color32,
    pub danger: Color32,
    pub speaking_bg: Color32,
    pub avatar: Color32,
}
pub(super) fn colors(ctx: &egui::Context) -> Palette {
    if ctx.style_of(ctx.theme()).visuals.dark_mode {
        Palette {
            background: Color32::from_rgb(22, 23, 27),
            panel: Color32::from_rgb(27, 28, 33),
            surface: Color32::from_rgb(35, 37, 44),
            border: Color32::from_rgb(57, 60, 69),
            text: Color32::from_rgb(238, 241, 248),
            secondary: Color32::from_rgb(177, 183, 198),
            accent: Color32::from_rgb(153, 176, 255),
            on_accent: Color32::from_rgb(17, 24, 46),
            success: Color32::from_rgb(110, 218, 172),
            warning: Color32::from_rgb(244, 202, 121),
            danger: Color32::from_rgb(255, 151, 163),
            speaking_bg: Color32::from_rgb(31, 54, 49),
            avatar: Color32::from_rgb(46, 54, 73),
        }
    } else {
        Palette {
            background: Color32::from_rgb(250, 250, 252),
            panel: Color32::from_rgb(242, 243, 247),
            surface: Color32::from_rgb(232, 234, 240),
            border: Color32::from_rgb(130, 137, 152),
            text: Color32::from_rgb(28, 31, 40),
            secondary: Color32::from_rgb(83, 91, 108),
            accent: Color32::from_rgb(58, 82, 174),
            on_accent: Color32::from_rgb(255, 255, 255),
            success: Color32::from_rgb(24, 115, 82),
            warning: Color32::from_rgb(133, 84, 12),
            danger: Color32::from_rgb(169, 40, 65),
            speaking_bg: Color32::from_rgb(214, 239, 226),
            avatar: Color32::from_rgb(224, 230, 242),
        }
    }
}
pub(super) fn install(ctx: &egui::Context) {
    let mut fonts = fastframe_fonts::FontSetup::default()
        .weights(&[fastframe_fonts::Weight::SemiBold])
        .definitions();
    fastframe_text::detect().apply_to(&mut fonts);
    ctx.set_fonts(fonts);
    apply(ctx, Appearance::default());
}

pub(super) fn size(ctx: &egui::Context, base: f32) -> f32 {
    base * ctx.style_of(ctx.theme()).text_styles[&TextStyle::Body].size / 14.0
}
pub(super) fn compact(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<Appearance>(egui::Id::new("appearance")))
        .unwrap_or_default()
        .density
        == MessageDensity::Compact
}
pub(super) fn apply(ctx: &egui::Context, mut appearance: Appearance) {
    appearance = appearance.normalize();
    let id = egui::Id::new("appearance");
    if ctx.data(|d| d.get_temp::<Appearance>(id)) == Some(appearance) {
        return;
    }
    ctx.data_mut(|d| d.insert_temp(id, appearance));
    let theme = if appearance.light_theme {
        egui::Theme::Light
    } else {
        egui::Theme::Dark
    };
    ctx.set_theme(theme);
    let mut style = egui::Style {
        visuals: if appearance.light_theme {
            egui::Visuals::light()
        } else {
            egui::Visuals::dark()
        },
        ..Default::default()
    };
    ctx.set_style_of(theme, style.clone());
    let palette = colors(ctx);
    style.visuals.override_text_color = Some(palette.text);
    style.visuals.panel_fill = palette.background;
    style.visuals.window_fill = palette.panel;
    style.visuals.extreme_bg_color = palette.background;
    style.visuals.faint_bg_color = palette.surface;
    style.visuals.selection.bg_fill = if appearance.light_theme {
        Color32::from_rgb(219, 226, 249)
    } else {
        Color32::from_rgb(47, 55, 79)
    };
    style.visuals.selection.stroke = Stroke::new(1.0, palette.accent);
    style.visuals.hyperlink_color = palette.accent;
    style.visuals.warn_fg_color = palette.warning;
    style.visuals.error_fg_color = palette.danger;
    style.visuals.widgets.noninteractive.bg_fill = palette.panel;
    style.visuals.widgets.noninteractive.weak_bg_fill = palette.panel;
    style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, palette.border);
    style.visuals.widgets.inactive.bg_fill = palette.surface;
    style.visuals.widgets.inactive.weak_bg_fill = palette.surface;
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, palette.border);
    style.visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, palette.text);
    style.visuals.widgets.hovered.bg_fill = if appearance.light_theme {
        Color32::from_rgb(223, 228, 240)
    } else {
        Color32::from_rgb(43, 46, 55)
    };
    style.visuals.widgets.hovered.weak_bg_fill = if appearance.light_theme {
        Color32::from_rgb(223, 228, 240)
    } else {
        Color32::from_rgb(43, 46, 55)
    };
    style.visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, palette.text);
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, palette.accent);
    style.visuals.widgets.active.bg_fill = if appearance.light_theme {
        Color32::from_rgb(211, 219, 238)
    } else {
        Color32::from_rgb(55, 63, 87)
    };
    style.visuals.widgets.active.fg_stroke = Stroke::new(1.0, palette.text);
    style.visuals.widgets.active.bg_stroke = Stroke::new(1.5, palette.accent);
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
    style.spacing.item_spacing = if appearance.density == MessageDensity::Compact {
        Vec2::new(8.0, 5.0)
    } else {
        Vec2::new(10.0, 8.0)
    };
    style.spacing.button_padding = if appearance.density == MessageDensity::Compact {
        Vec2::new(10.0, 6.0)
    } else {
        Vec2::new(12.0, 8.0)
    };
    style.spacing.interact_size = Vec2::new(36.0, 34.0);
    style.spacing.slider_width = 105.0;
    style.text_styles.insert(
        TextStyle::Body,
        egui::FontId::proportional(appearance.text_size),
    );
    style.text_styles.insert(
        TextStyle::Button,
        egui::FontId::proportional(appearance.text_size),
    );
    style.text_styles.insert(
        TextStyle::Small,
        egui::FontId::proportional(appearance.text_size * 12.0 / 14.0),
    );
    style.text_styles.insert(
        TextStyle::Heading,
        fastframe_fonts::Weight::SemiBold.font_id(appearance.text_size * 20.0 / 14.0),
    );
    // Input interactions repaint; decorative animation does not create an
    // idle redraw loop or keep a sleeping voice client active.
    style.animation_time = 0.0;
    ctx.set_style_of(theme, style);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn appearance_is_isolated_between_windows_and_normalizes_size() {
        let a = egui::Context::default();
        let b = egui::Context::default();
        apply(
            &a,
            Appearance {
                light_theme: true,
                text_size: 20.0,
                density: MessageDensity::Comfortable,
            },
        );
        apply(&b, Appearance::default());
        assert_eq!(a.theme(), egui::Theme::Light);
        assert_eq!(b.theme(), egui::Theme::Dark);
        assert_eq!(size(&a, 14.0), 20.0);
        assert_eq!(size(&b, 14.0), 14.0);
        assert!(!compact(&a));
        assert!(compact(&b));
        apply(
            &a,
            Appearance {
                text_size: f32::NAN,
                ..Appearance::default()
            },
        );
        assert_eq!(size(&a, 14.0), 14.0);
    }
    fn contrast(a: Color32, b: Color32) -> f32 {
        fn luminance(c: Color32) -> f32 {
            let rgb = [c.r(), c.g(), c.b()].map(|v| {
                let v = f32::from(v) / 255.0;
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            });
            rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722
        }
        let a = luminance(a);
        let b = luminance(b);
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }
    #[test]
    fn both_palettes_keep_text_and_status_readable() {
        let ctx = egui::Context::default();
        for light_theme in [false, true] {
            apply(
                &ctx,
                Appearance {
                    light_theme,
                    ..Appearance::default()
                },
            );
            let p = colors(&ctx);
            for foreground in [p.text, p.secondary, p.danger, p.warning, p.success] {
                for background in [p.background, p.panel, p.surface] {
                    assert!(
                        contrast(foreground, background) >= 4.5,
                        "{foreground:?} on {background:?}"
                    );
                }
            }
            assert!(contrast(p.on_accent, p.accent) >= 4.5);
        }
    }
}
