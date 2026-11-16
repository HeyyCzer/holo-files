//! "Atividade": recently synced files, newest first, grouped by day (like the
//! OneDrive client's activity list). Read from the `activity` table the daemon fills.

use std::path::PathBuf;

use chrono::{DateTime, Local, Utc};
use eframe::egui::{self, Color32, RichText};
use holo_state::{ActivityKind, ActivityRecord};

use super::{App, icons::Icon, theme, widgets};
use crate::actions;

/// Entries loaded for the page.
pub const LOAD: usize = 400;

#[derive(PartialEq, Clone, Copy, Default)]
enum Filter {
    #[default]
    All,
    Sent,
    Received,
    Deleted,
    Errors,
}

#[derive(Default)]
pub struct State {
    filter: Filter,
    search: String,
}

struct Look {
    icon: Icon,
    color: Color32,
    verb: &'static str,
}

fn look(kind: Option<ActivityKind>) -> Look {
    use ActivityKind::*;
    let (icon, color, verb) = match kind {
        Some(Uploaded) => (Icon::Upload, theme::CYAN, "Enviado para a nuvem"),
        Some(FolderCreated) => (Icon::FolderPlus, theme::CYAN, "Pasta criada na nuvem"),
        Some(Moved) => (Icon::Edit, theme::BLUE, "Movido ou renomeado"),
        Some(Deleted) => (Icon::Trash, theme::WARN, "Excluído (também na nuvem)"),
        Some(CloudAdded) => (Icon::Cloud, theme::MINT, "Novo, vindo da nuvem"),
        Some(CloudChanged) => (Icon::Sync, theme::MINT, "Atualizado pela nuvem"),
        Some(CloudDeleted) => (Icon::Trash, theme::WARN, "Excluído na nuvem"),
        Some(Downloaded) => (Icon::Download, theme::BLUE, "Baixado ao abrir"),
        None => (Icon::Info, theme::WEAK, "Atividade"),
    };
    Look { icon, color, verb }
}

fn matches(filter: Filter, a: &ActivityRecord) -> bool {
    use ActivityKind::*;
    match filter {
        Filter::All => true,
        Filter::Errors => a.error.is_some(),
        Filter::Sent => matches!(a.kind, Some(Uploaded | FolderCreated | Moved)),
        Filter::Received => matches!(a.kind, Some(CloudAdded | CloudChanged | Downloaded)),
        Filter::Deleted => matches!(a.kind, Some(Deleted | CloudDeleted)),
    }
}

fn ago(at: DateTime<Utc>) -> String {
    let s = (Utc::now() - at).num_seconds().max(0);
    match s {
        0..60 => "agora".into(),
        60..3600 => format!("há {} min", s / 60),
        3600..86400 => format!("há {} h", s / 3600),
        _ => at.with_timezone(&Local).format("%H:%M").to_string(),
    }
}

fn day_label(at: DateTime<Utc>) -> String {
    let d = at.with_timezone(&Local).date_naive();
    let today = Local::now().date_naive();
    match (today - d).num_days() {
        0 => "Hoje".into(),
        1 => "Ontem".into(),
        _ => d.format("%d/%m/%Y").to_string(),
    }
}

pub fn page(app: &mut App, ui: &mut egui::Ui) {
    widgets::page_title(
        ui,
        "Atividade recente",
        "Arquivos enviados, baixados e alterados pela nuvem, do mais recente ao mais antigo.",
    );

    ui.horizontal(|ui| {
        for (f, name) in [
            (Filter::All, "Tudo"),
            (Filter::Sent, "Enviados"),
            (Filter::Received, "Recebidos"),
            (Filter::Deleted, "Excluídos"),
            (Filter::Errors, "Erros"),
        ] {
            ui.selectable_value(&mut app.activity_ui.filter, f, name);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut app.activity_ui.search)
                    .hint_text("Filtrar por nome…")
                    .desired_width(220.0),
            );
            Icon::Search.show(ui, 14.0, theme::WEAK);
        });
    });
    ui.add_space(8.0);

    let roots: Vec<(String, String)> = app
        .snap
        .as_ref()
        .map(|s| {
            s.roots
                .iter()
                .map(|r| (r.root.id.clone(), r.root.local_path.clone()))
                .collect()
        })
        .unwrap_or_default();
    let several = roots.len() > 1;
    let needle = app.activity_ui.search.trim().to_lowercase();
    let filter = app.activity_ui.filter;
    let rows: Vec<&ActivityRecord> = app
        .activity
        .iter()
        .filter(|a| matches(filter, a))
        .filter(|a| needle.is_empty() || a.path.to_lowercase().contains(&needle))
        .collect();

    if rows.is_empty() {
        widgets::card(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(10.0);
                widgets::icon_badge(ui, Icon::Sync, theme::CYAN, 48.0);
                ui.add_space(4.0);
                if app.activity.is_empty() {
                    ui.label(RichText::new("Nenhuma atividade ainda.").strong());
                    ui.label(
                        RichText::new(
                            "Envios, downloads sob demanda e mudanças vindas da nuvem aparecem aqui assim que acontecerem.",
                        )
                        .color(theme::WEAK),
                    );
                } else {
                    ui.label(RichText::new("Nada com esse filtro.").color(theme::WEAK));
                }
                ui.add_space(10.0);
            });
        });
        return;
    }

    let mut day = String::new();
    for a in rows {
        let d = day_label(a.at);
        if d != day {
            if !day.is_empty() {
                ui.add_space(6.0);
            }
            widgets::section(ui, &d);
            day = d;
        }
        let root = roots.iter().find(|(id, _)| *id == a.root_id);
        row(ui, a, root.map(|(_, p)| p.as_str()), several);
        ui.add_space(4.0);
    }
    if app.activity.len() >= LOAD {
        ui.label(
            RichText::new(format!("Mostrando as {LOAD} mais recentes."))
                .small()
                .color(theme::WEAK),
        );
    }
}

fn row(ui: &mut egui::Ui, a: &ActivityRecord, root: Option<&str>, several_roots: bool) {
    let l = look(a.kind);
    let color = if a.error.is_some() {
        theme::DANGER
    } else {
        l.color
    };
    let (dir, name) = match a.path.rsplit_once('/') {
        Some((d, n)) => (d, n),
        None => ("", a.path.as_str()),
    };
    let local: Option<PathBuf> = root.map(|r| PathBuf::from(r).join(a.path.replace('/', "\\")));

    egui::Frame::new()
        .fill(theme::CARD)
        .corner_radius(egui::CornerRadius::same(theme::RADIUS))
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                widgets::icon_badge(
                    ui,
                    if a.error.is_some() {
                        Icon::Danger
                    } else {
                        l.icon
                    },
                    color,
                    34.0,
                );
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.add(
                        egui::Label::new(RichText::new(name).strong().color(theme::TEXT))
                            .truncate(),
                    );
                    let mut sub = String::from(if a.error.is_some() { "Falhou" } else { l.verb });
                    if let Some(b) = a.bytes.filter(|b| *b > 0) {
                        sub.push_str(&format!(" · {}", widgets::fmt_size(b)));
                    }
                    if let (Some(ActivityKind::Downloaded), Some(app)) = (a.kind, &a.detail) {
                        sub.push_str(&format!(" · por {app}"));
                    }
                    let mut place = if dir.is_empty() {
                        "/".to_owned()
                    } else {
                        format!("/{dir}")
                    };
                    if several_roots && let Some(r) = root {
                        place = format!("{r}{place}");
                    }
                    sub.push_str(&format!(" · em {place}"));
                    ui.add(
                        egui::Label::new(RichText::new(sub).small().color(theme::WEAK)).truncate(),
                    );
                    if let (Some(ActivityKind::CloudChanged | ActivityKind::Moved), Some(from)) =
                        (a.kind, &a.detail)
                    {
                        ui.label(
                            RichText::new(format!("antes: {from}"))
                                .small()
                                .color(theme::WEAK),
                        );
                    }
                    if let Some(e) = &a.error {
                        ui.add(
                            egui::Label::new(RichText::new(e).small().color(theme::DANGER)).wrap(),
                        );
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let gone = matches!(
                        a.kind,
                        Some(ActivityKind::Deleted | ActivityKind::CloudDeleted)
                    );
                    if let Some(p) = local.as_ref().filter(|_| !gone)
                        && widgets::icon_button(ui, Icon::FolderOpen, "Mostrar no Explorer")
                    {
                        actions::reveal(p);
                    }
                    ui.label(RichText::new(ago(a.at)).small().color(theme::WEAK))
                        .on_hover_text(
                            a.at.with_timezone(&Local)
                                .format("%d/%m/%Y %H:%M:%S")
                                .to_string(),
                        );
                });
            });
        });
}
