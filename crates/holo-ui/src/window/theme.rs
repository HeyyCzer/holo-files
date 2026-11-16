//! Dark theme built around the app icon's mint → cyan → blue gradient.

use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Margin, Shadow, Stroke, TextStyle, Vec2,
};

pub const BG: Color32 = Color32::from_rgb(0x12, 0x15, 0x1b);
pub const SIDEBAR: Color32 = Color32::from_rgb(0x0d, 0x10, 0x15);
pub const CARD: Color32 = Color32::from_rgb(0x1a, 0x1e, 0x26);
pub const CARD_HI: Color32 = Color32::from_rgb(0x22, 0x27, 0x31);
pub const INPUT: Color32 = Color32::from_rgb(0x0f, 0x12, 0x17);
pub const BORDER: Color32 = Color32::from_rgb(0x2a, 0x30, 0x3b);
pub const TEXT: Color32 = Color32::from_rgb(0xe4, 0xe7, 0xed);
pub const WEAK: Color32 = Color32::from_rgb(0x8a, 0x93, 0xa3);

pub const MINT: Color32 = Color32::from_rgb(0x34, 0xd3, 0x99);
pub const CYAN: Color32 = Color32::from_rgb(0x06, 0xb6, 0xd4);
pub const BLUE: Color32 = Color32::from_rgb(0x3b, 0x82, 0xf6);
pub const WARN: Color32 = Color32::from_rgb(0xe3, 0xa0, 0x3a);
pub const DANGER: Color32 = Color32::from_rgb(0xe5, 0x53, 0x4b);

pub const RADIUS: u8 = 8;

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = CARD;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.window_corner_radius = CornerRadius::same(12);
    v.window_shadow = Shadow {
        offset: [0, 8],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(120),
    };
    v.menu_corner_radius = CornerRadius::same(RADIUS);
    v.extreme_bg_color = INPUT;
    v.text_edit_bg_color = Some(INPUT);
    v.faint_bg_color = Color32::from_rgb(0x1f, 0x24, 0x2d);
    v.code_bg_color = INPUT;
    v.hyperlink_color = CYAN;
    v.warn_fg_color = WARN;
    v.error_fg_color = DANGER;
    v.selection.bg_fill = CYAN.linear_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, CYAN);
    v.override_text_color = None;

    let r = CornerRadius::same(6);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = CARD;
    w.noninteractive.weak_bg_fill = CARD;
    w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    w.noninteractive.corner_radius = r;
    w.inactive.bg_fill = CARD_HI;
    w.inactive.weak_bg_fill = CARD_HI;
    w.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    w.inactive.corner_radius = r;
    w.hovered.bg_fill = Color32::from_rgb(0x2b, 0x31, 0x3d);
    w.hovered.weak_bg_fill = Color32::from_rgb(0x2b, 0x31, 0x3d);
    w.hovered.bg_stroke = Stroke::new(1.0, CYAN.linear_multiply(0.6));
    w.hovered.fg_stroke = Stroke::new(1.5, Color32::WHITE);
    w.hovered.corner_radius = r;
    w.active.bg_fill = Color32::from_rgb(0x33, 0x3a, 0x48);
    w.active.weak_bg_fill = Color32::from_rgb(0x33, 0x3a, 0x48);
    w.active.bg_stroke = Stroke::new(1.0, CYAN);
    w.active.fg_stroke = Stroke::new(1.5, Color32::WHITE);
    w.active.corner_radius = r;
    w.open = w.active;
    ctx.set_visuals_of(egui::Theme::Dark, v);

    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = Vec2::new(8.0, 7.0);
        s.spacing.button_padding = Vec2::new(12.0, 5.0);
        s.spacing.interact_size.y = 26.0;
        s.spacing.window_margin = Margin::same(18);
        s.spacing.combo_width = 220.0;
        s.spacing.text_edit_width = 320.0;
        use FontFamily::{Monospace, Proportional};
        s.text_styles = [
            (TextStyle::Heading, FontId::new(21.0, Proportional)),
            (TextStyle::Body, FontId::new(14.0, Proportional)),
            (TextStyle::Button, FontId::new(14.0, Proportional)),
            (TextStyle::Small, FontId::new(11.5, Proportional)),
            (TextStyle::Monospace, FontId::new(13.0, Monospace)),
        ]
        .into();
    });
}
