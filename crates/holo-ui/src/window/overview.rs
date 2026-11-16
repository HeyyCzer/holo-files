//! "Visão geral": per-root progress, on-demand downloads and the op journal.

use std::time::Duration;

use eframe::egui::{self, RichText};
use holo_state::{OpRecord, OpState};

use super::{App, Page, icons::Icon, theme, widgets};
use crate::{actions, snapshot::RootView, snapshot::op_target};

const MAX_ROWS: usize = 500;

#[derive(PartialEq, Clone, Copy, Default)]
enum Filter {
    #[default]
    All,
    Pending,
    Held,
    Failed,
}

#[derive(Default)]
pub struct State {
    filter: Filter,
    confirm_release: bool,
    confirm_discard: bool,
}

pub fn page(app: &mut App, ui: &mut egui::Ui) {
    widgets::page_title(ui, "Visão geral", "O que está sendo sincronizado agora.");
    let Some(s) = &app.snap else { return };
    let (daemon, paused, no_roots) = (s.daemon, s.paused, s.roots.is_empty());

    if app.accounts.is_empty() || no_roots {
        onboarding(app, ui);
        ui.add_space(12.0);
    }
    if paused {
        widgets::info(
            ui,
            "Sincronização pausada: nada é enviado nem baixado, mas arquivos sob demanda continuam abrindo.",
        );
        ui.add_space(6.0);
    }
    if !daemon && !no_roots {
        widgets::warning(
            ui,
            "O daemon está parado: nenhuma alteração é sincronizada e arquivos só na nuvem não abrem. Use \"Iniciar daemon\" no topo.",
        );
        ui.add_space(6.0);
    }

    if let Some(s) = &app.snap {
        for r in &s.roots {
            root_card(ui, r, daemon);
            ui.add_space(8.0);
        }
    }
    if !no_roots {
        ui.add_space(8.0);
        journal(app, ui);
    }
}

/// First-run checklist: connect an account, then add a folder.
fn onboarding(app: &mut App, ui: &mut egui::Ui) {
    let has_account = !app.accounts.is_empty();
    widgets::card(ui, |ui| {
        ui.label(RichText::new("Primeiros passos").size(16.0).strong());
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            widgets::step_number(ui, 1, has_account);
            ui.label("Conecte uma conta do OneDrive");
            if !has_account && ui.add(widgets::primary("Conectar conta")).clicked() {
                app.page = Page::Accounts;
                app.accounts_ui.open_wizard();
            }
        });
        ui.horizontal(|ui| {
            widgets::step_number(ui, 2, false);
            ui.label("Escolha uma pasta local para sincronizar");
            if has_account && ui.add(widgets::primary("Adicionar pasta")).clicked() {
                app.page = Page::Roots;
                app.roots.open_form();
            }
        });
        ui.horizontal(|ui| {
            widgets::step_number(ui, 3, false);
            ui.label("Inicie o daemon: os arquivos aparecem no Explorer e abrem sob demanda");
        });
    });
}

fn root_card(ui: &mut egui::Ui, r: &RootView, daemon: bool) {
    widgets::card(ui, |ui| {
        ui.horizontal(|ui| {
            widgets::icon_badge(ui, Icon::Folder, theme::CYAN, 34.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.label(RichText::new(&r.root.local_path).strong().size(15.0));
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    Icon::Cloud.show(ui, 12.0, theme::WEAK);
                    ui.label(
                        RichText::new(&r.root.remote_path)
                            .small()
                            .color(theme::WEAK),
                    );
                });
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(widgets::button(Icon::FolderOpen, "Abrir")).clicked() {
                    actions::open(std::path::Path::new(&r.root.local_path));
                }
                if r.root.cursor.is_none() {
                    widgets::pill(ui, theme::WARN, "sincronização inicial pendente");
                } else if !r.root.enabled {
                    widgets::pill(ui, theme::WEAK, "desativada");
                }
            });
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            widgets::stat(ui, "itens", r.items, None);
            widgets::stat(ui, "na fila", r.counts.pending, Some(theme::CYAN));
            widgets::stat(ui, "retidas", r.counts.held, Some(theme::WARN));
            widgets::stat(ui, "falhas", r.counts.failed, Some(theme::DANGER));
        });
        if !daemon {
            return;
        }
        if let Some(p) = &r.pull {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(p).color(theme::WEAK));
            });
        }
        let pr = &r.progress;
        let workers = if pr.auto {
            format!(
                "{} de no máx. {} (auto: {})",
                pr.workers, pr.max_workers, pr.workers_reason
            )
        } else {
            pr.workers.to_string()
        };
        ui.label(
            RichText::new(format!(
                "{} concluídas desde o início{} · workers {workers}",
                pr.done,
                if pr.failed > 0 {
                    format!(", {} falharam", pr.failed)
                } else {
                    String::new()
                }
            ))
            .small()
            .color(theme::WEAK),
        );
        if !pr.running.is_empty() {
            ui.add_space(4.0);
            let now = chrono::Utc::now().timestamp_millis();
            egui::Grid::new(("running", &r.root.id))
                .striped(true)
                .spacing([14.0, 4.0])
                .show(ui, |ui| {
                    for op in &pr.running {
                        ui.label(RichText::new(&op.kind).monospace().color(theme::CYAN));
                        ui.label(widgets::fmt_age(Duration::from_millis(
                            (now - op.started_ms).max(0) as u64,
                        )));
                        ui.label(&op.path);
                        ui.end_row();
                    }
                });
        }
        hydrations(ui, r);
    });
}

/// Cloud-only files being downloaded because an app opened them, plus the last few
/// finished (failures show why the app got nothing).
fn hydrations(ui: &mut egui::Ui, r: &RootView) {
    let h = &r.hydrate;
    if h.active.is_empty() && h.recent.is_empty() {
        return;
    }
    ui.add_space(8.0);
    widgets::section(ui, "Downloads sob demanda");
    let now = chrono::Utc::now().timestamp_millis();
    egui::Grid::new(("hydrate", &r.root.id))
        .striped(true)
        .spacing([14.0, 4.0])
        .show(ui, |ui| {
            for a in &h.active {
                let frac = if a.bytes > 0 {
                    a.done as f32 / a.bytes as f32
                } else {
                    0.0
                };
                ui.add(
                    egui::ProgressBar::new(frac)
                        .desired_width(140.0)
                        .fill(theme::CYAN.linear_multiply(0.7))
                        .text(format!(
                            "{} / {}",
                            widgets::fmt_size(a.done),
                            widgets::fmt_size(a.bytes)
                        )),
                );
                ui.label(widgets::fmt_age(Duration::from_millis(
                    (now - a.started_ms).max(0) as u64,
                )));
                ui.label(a.process.as_deref().unwrap_or("?"));
                ui.label(&a.path);
                ui.end_row();
            }
            for f in &h.recent {
                match &f.error {
                    None => ui.horizontal(|ui| {
                        Icon::CheckCircle.show(ui, 13.0, theme::MINT);
                        ui.label(RichText::new(widgets::fmt_size(f.bytes)).color(theme::MINT));
                    }),
                    Some(_) => ui.horizontal(|ui| {
                        Icon::XCircle.show(ui, 13.0, theme::DANGER);
                        ui.label(RichText::new("falhou").color(theme::DANGER));
                    }),
                };
                ui.label(
                    RichText::new(format!(
                        "há {}",
                        widgets::fmt_age(
                            Duration::from_millis((now - f.finished_ms).max(0) as u64)
                        )
                    ))
                    .color(theme::WEAK),
                );
                ui.label(f.process.as_deref().unwrap_or("?"));
                let path = ui.label(&f.path);
                if let Some(e) = &f.error {
                    path.on_hover_text(e);
                    ui.label(RichText::new(e).color(theme::WEAK));
                }
                ui.end_row();
            }
        });
}

fn journal(app: &mut App, ui: &mut egui::Ui) {
    let t = app.snap.as_ref().map(|s| s.totals()).unwrap_or_default();
    ui.horizontal(|ui| {
        ui.label(RichText::new("Fila de operações").size(16.0).strong());
        ui.add_space(8.0);
        for (f, name) in [
            (Filter::All, "todas"),
            (Filter::Pending, "pendentes"),
            (Filter::Held, "retidas"),
            (Filter::Failed, "falhas"),
        ] {
            ui.selectable_value(&mut app.overview.filter, f, name);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add_enabled(t.failed > 0, widgets::button(Icon::Retry, "Repetir falhas"))
                .clicked()
            {
                let r = actions::retry_failed(&app.ctx);
                app.report(r, "operação(ões) recolocadas na fila");
            }
            if ui
                .add_enabled(
                    t.held > 0,
                    widgets::outline("Descartar retidas", theme::DANGER),
                )
                .on_hover_text(
                    "Não executa as exclusões retidas pela proteção de exclusão em massa",
                )
                .clicked()
            {
                app.overview.confirm_discard = true;
            }
            if ui
                .add_enabled(t.held > 0, widgets::outline("Liberar retidas", theme::WARN))
                .on_hover_text("Confirma e executa as operações retidas (ex.: exclusões em massa)")
                .clicked()
            {
                app.overview.confirm_release = true;
            }
        });
    });
    if t.held > 0 {
        ui.add_space(4.0);
        widgets::warning(
            ui,
            format!(
                "{} operação(ões) retidas pela proteção contra exclusão em massa. Confira a lista antes de liberar.",
                t.held
            ),
        );
    }
    ui.add_space(6.0);

    let filter = app.overview.filter;
    let rows: Vec<&OpRecord> = app
        .ops
        .iter()
        .filter(|o| match filter {
            Filter::All => true,
            Filter::Pending => o.state == OpState::Pending,
            Filter::Held => o.state == OpState::Held,
            Filter::Failed => o.state == OpState::Failed,
        })
        .take(MAX_ROWS)
        .collect();
    widgets::card(ui, |ui| {
        if rows.is_empty() {
            ui.horizontal(|ui| {
                Icon::CheckCircle.show(ui, 15.0, theme::MINT);
                ui.label(RichText::new("Nada na fila: tudo enviado.").color(theme::WEAK));
            });
            return;
        }
        let now = chrono::Utc::now();
        egui::Grid::new("ops")
            .striped(true)
            .num_columns(5)
            .spacing([14.0, 5.0])
            .show(ui, |ui| {
                for h in ["#", "estado", "tipo", "caminho", "detalhe"] {
                    ui.label(RichText::new(h).small().strong().color(theme::WEAK));
                }
                ui.end_row();
                for o in rows {
                    ui.label(RichText::new(o.id.to_string()).color(theme::WEAK));
                    let (state, color) = match o.state {
                        OpState::Pending => ("pendente", theme::CYAN),
                        OpState::Held => ("retida", theme::WARN),
                        OpState::Failed => ("falha", theme::DANGER),
                    };
                    ui.label(RichText::new(state).color(color));
                    ui.label(RichText::new(&o.kind).monospace());
                    ui.label(op_target(o));
                    let mut detail = String::new();
                    if o.state == OpState::Pending && o.not_before > now {
                        let secs = (o.not_before - now).num_seconds().max(0) as u64;
                        detail.push_str(&format!(
                            "em {} · ",
                            widgets::fmt_age(Duration::from_secs(secs))
                        ));
                    }
                    if o.attempts > 0 {
                        detail.push_str(&format!("{}× · ", o.attempts));
                    }
                    if let Some(e) = &o.last_error {
                        detail.push_str(e);
                    }
                    ui.label(RichText::new(detail.trim_end_matches(" · ")).color(theme::WEAK));
                    ui.end_row();
                }
            });
        if app.ops.len() > MAX_ROWS {
            ui.label(
                RichText::new(format!("… mostrando {MAX_ROWS} de {}", app.ops.len()))
                    .color(theme::WEAK),
            );
        }
    });
}

/// Confirmations for the held-ops buttons.
pub fn modals(app: &mut App, ctx: &egui::Context) {
    let held = app.snap.as_ref().map_or(0, |s| s.totals().held);
    if app.overview.confirm_release {
        let r = egui::Modal::new(egui::Id::unique("confirm-release")).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.label(RichText::new("Liberar operações retidas?").size(17.0).strong());
            ui.add_space(6.0);
            widgets::warning(
                ui,
                format!(
                    "{held} operação(ões) serão executadas. Exclusões retidas apagam os itens na nuvem (vão para a lixeira do OneDrive)."
                ),
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.add(widgets::outline("Liberar e executar", theme::WARN)).clicked() {
                    let r = actions::release_held(&app.ctx);
                    app.report(r, "operação(ões) liberadas");
                    app.overview.confirm_release = false;
                }
                if ui.button("Cancelar").clicked() {
                    app.overview.confirm_release = false;
                }
            });
        });
        if r.should_close() {
            app.overview.confirm_release = false;
        }
    }
    if app.overview.confirm_discard {
        let r = egui::Modal::new(egui::Id::unique("confirm-discard")).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.label(RichText::new("Descartar operações retidas?").size(17.0).strong());
            ui.add_space(6.0);
            ui.label(format!(
                "{held} operação(ões) retidas serão removidas da fila sem executar: nada é apagado na nuvem."
            ));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.add(widgets::danger_button("Descartar")).clicked() {
                    let r = actions::discard_held(&app.ctx);
                    app.report(r, "operação(ões) retidas descartadas");
                    app.overview.confirm_discard = false;
                }
                if ui.button("Cancelar").clicked() {
                    app.overview.confirm_discard = false;
                }
            });
        });
        if r.should_close() {
            app.overview.confirm_discard = false;
        }
    }
}
