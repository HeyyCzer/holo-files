//! Small building blocks shared by the pages.

use std::time::Duration;

use eframe::egui::{self, Color32, CornerRadius, Margin, RichText, Stroke};

use super::{icons::Icon, theme};

/// Rounded card with a subtle border.
pub fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(theme::CARD)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .corner_radius(CornerRadius::same(theme::RADIUS + 2))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// Callout box: tinted background with a coloured left edge feel.
pub fn callout(ui: &mut egui::Ui, color: Color32, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(color.linear_multiply(0.10))
        .stroke(Stroke::new(1.0, color.linear_multiply(0.45)))
        .corner_radius(CornerRadius::same(theme::RADIUS))
        .inner_margin(Margin::symmetric(12, 9))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        });
}

/// Callout with a leading icon and wrapped text.
pub fn notice(ui: &mut egui::Ui, icon: Icon, color: Color32, text_color: Color32, text: String) {
    callout(ui, color, |ui| {
        ui.horizontal_top(|ui| {
            icon.show(ui, 16.0, color);
            ui.add(egui::Label::new(RichText::new(text).color(text_color)).wrap());
        });
    });
}

pub fn warning(ui: &mut egui::Ui, text: impl Into<String>) {
    notice(ui, Icon::Warning, theme::WARN, theme::WARN, text.into());
}

pub fn danger(ui: &mut egui::Ui, text: impl Into<String>) {
    notice(ui, Icon::Danger, theme::DANGER, theme::DANGER, text.into());
}

pub fn info(ui: &mut egui::Ui, text: impl Into<String>) {
    notice(ui, Icon::Info, theme::CYAN, theme::TEXT, text.into());
}

/// Page title plus an optional one-line description.
pub fn page_title(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    ui.label(RichText::new(title).heading().strong().color(theme::TEXT));
    if !subtitle.is_empty() {
        ui.label(RichText::new(subtitle).color(theme::WEAK));
    }
    ui.add_space(10.0);
}

/// Uppercase small caption above a group of fields.
pub fn section(ui: &mut egui::Ui, title: &str) {
    ui.label(
        RichText::new(title.to_uppercase())
            .small()
            .strong()
            .color(theme::WEAK),
    );
    ui.add_space(2.0);
}

const ON_ACCENT: Color32 = Color32::from_rgb(0x06, 0x1a, 0x22);

/// Button with its own colours that still reacts to hover and press. (`Button::fill`
/// would freeze the background: egui then skips its hover/active visuals.)
pub struct Styled {
    icon: Option<Icon>,
    text: RichText,
    color: Color32,
    fill: Color32,
    stroke: Stroke,
}

impl egui::Widget for Styled {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        let Self {
            icon,
            text,
            color,
            fill,
            stroke,
        } = self;
        ui.scope(|ui| {
            let (hover, press) = if fill.a() == 0 {
                (color.linear_multiply(0.14), color.linear_multiply(0.24))
            } else {
                (
                    fill.lerp_to_gamma(Color32::WHITE, 0.14),
                    fill.lerp_to_gamma(Color32::BLACK, 0.12),
                )
            };
            let hover_stroke = if stroke.width > 0.0 {
                Stroke::new(stroke.width, color.linear_multiply(0.9))
            } else {
                Stroke::NONE
            };
            let w = &mut ui.visuals_mut().widgets;
            for (v, bg, st) in [
                (&mut w.inactive, fill, stroke),
                (&mut w.hovered, hover, hover_stroke),
                (&mut w.active, press, hover_stroke),
            ] {
                v.bg_fill = bg;
                v.weak_bg_fill = bg;
                v.bg_stroke = st;
                v.expansion = 0.0;
            }
            let text = text.color(color);
            let b = match icon {
                Some(i) => egui::Button::new((i.image(15.0, color), text)),
                None => egui::Button::new(text),
            };
            ui.add(b).on_hover_cursor(egui::CursorIcon::PointingHand)
        })
        .inner
    }
}

fn styled(
    icon: Option<Icon>,
    text: RichText,
    color: Color32,
    fill: Color32,
    stroke: Stroke,
) -> Styled {
    Styled {
        icon,
        text,
        color,
        fill,
        stroke,
    }
}

pub fn primary(text: impl Into<String>) -> Styled {
    styled(
        None,
        RichText::new(text.into()).strong(),
        ON_ACCENT,
        theme::CYAN,
        Stroke::NONE,
    )
}

pub fn primary_icon(icon: Icon, text: impl Into<String>) -> Styled {
    styled(
        Some(icon),
        RichText::new(text.into()).strong(),
        ON_ACCENT,
        theme::CYAN,
        Stroke::NONE,
    )
}

pub fn danger_button(text: impl Into<String>) -> Styled {
    styled(
        Some(Icon::Trash),
        RichText::new(text.into()).strong(),
        Color32::WHITE,
        theme::DANGER.linear_multiply(0.85),
        Stroke::NONE,
    )
}

/// Normal button with an icon.
pub fn button(icon: Icon, text: impl Into<String>) -> Styled {
    styled(
        Some(icon),
        RichText::new(text.into()),
        theme::TEXT,
        theme::CARD_HI,
        Stroke::new(1.0, theme::BORDER),
    )
}

/// Outline button tinted with `color` (secondary destructive actions).
pub fn outline(text: impl Into<String>, color: Color32) -> Styled {
    styled(
        None,
        RichText::new(text.into()),
        color,
        Color32::TRANSPARENT,
        Stroke::new(1.0, color.linear_multiply(0.6)),
    )
}

pub fn outline_icon(icon: Icon, text: impl Into<String>, color: Color32) -> Styled {
    styled(
        Some(icon),
        RichText::new(text.into()),
        color,
        Color32::TRANSPARENT,
        Stroke::new(1.0, color.linear_multiply(0.6)),
    )
}

/// Small borderless icon-only button.
pub fn icon_button(ui: &mut egui::Ui, icon: Icon, hint: &str) -> bool {
    ui.add(egui::Button::new(icon.image(14.0, theme::WEAK)).frame(false))
        .on_hover_text(hint)
        .clicked()
}

/// Round tinted icon (list rows, cards).
pub fn icon_badge(ui: &mut egui::Ui, icon: Icon, color: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    ui.painter().rect_filled(
        rect,
        CornerRadius::same((size / 3.5) as u8),
        color.linear_multiply(0.16),
    );
    icon.paint(ui, rect.shrink(size * 0.24), color);
}

/// Rounded pill: coloured dot + label.
pub fn pill(ui: &mut egui::Ui, color: Color32, text: &str) {
    egui::Frame::new()
        .fill(color.linear_multiply(0.14))
        .stroke(Stroke::new(1.0, color.linear_multiply(0.5)))
        .corner_radius(CornerRadius::same(255))
        .inner_margin(Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                dot(ui, color, 4.0);
                ui.label(RichText::new(text).strong().color(color));
            });
        });
}

pub fn dot(ui: &mut egui::Ui, color: Color32, radius: f32) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(radius * 2.0, radius * 2.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), radius, color);
}

/// Big number with a caption below it.
pub fn stat(ui: &mut egui::Ui, name: &str, n: u64, color: Option<Color32>) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let c = color.filter(|_| n > 0).unwrap_or(theme::TEXT);
        ui.label(RichText::new(n.to_string()).size(19.0).strong().color(c));
        ui.label(RichText::new(name).small().color(theme::WEAK));
    });
    ui.add_space(18.0);
}

/// A value the user copies into another app (e.g. the Azure portal). Shows
/// "Copiado" with a check for a moment after a click.
pub fn copy_chip(ui: &mut egui::Ui, value: &str) {
    const FEEDBACK: f64 = 1.5;
    let id = egui::Id::unique(("copy-chip", value));
    let now = ui.input(|i| i.time);
    let copied_at: Option<f64> = ui.data(|d| d.get_temp(id));
    let copied = copied_at.is_some_and(|t| now - t < FEEDBACK);
    let (icon, text, color) = if copied {
        (
            Icon::Check,
            RichText::new("Copiado!").monospace(),
            theme::MINT,
        )
    } else {
        (Icon::Copy, RichText::new(value).monospace(), theme::MINT)
    };
    let r = ui
        .add(styled(
            Some(icon),
            text,
            color,
            theme::INPUT,
            Stroke::new(1.0, theme::BORDER),
        ))
        .on_hover_text("Clique para copiar");
    if r.clicked() {
        ui.ctx().copy_text(value.to_owned());
        ui.data_mut(|d| d.insert_temp(id, now));
    }
    if copied {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs_f64(FEEDBACK));
    }
}

/// Numbered step marker for guides.
pub fn step_number(ui: &mut egui::Ui, n: usize, done: bool) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::hover());
    let p = ui.painter();
    if done {
        p.circle_filled(rect.center(), 12.0, theme::MINT);
        Icon::Check.paint(ui, rect.shrink(6.0), theme::BG);
    } else {
        p.circle_filled(rect.center(), 12.0, theme::CYAN.linear_multiply(0.2));
        p.circle_stroke(rect.center(), 11.5, Stroke::new(1.0, theme::CYAN));
        p.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            n.to_string(),
            egui::FontId::proportional(13.0),
            theme::CYAN,
        );
    }
}

pub fn fmt_size(b: u64) -> String {
    const K: f64 = 1024.0;
    let b = b as f64;
    if b < K {
        format!("{b} B")
    } else if b < K * K {
        format!("{:.0} KiB", b / K)
    } else if b < K * K * K {
        format!("{:.1} MiB", b / K / K)
    } else {
        format!("{:.2} GiB", b / K / K / K)
    }
}

pub fn fmt_age(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m{:02}s", s / 60, s % 60),
        _ => format!("{}h{:02}m", s / 3600, s / 60 % 60),
    }
}
