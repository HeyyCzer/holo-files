//! "Pastas": add and remove sync roots (through `holo add-root` / `holo remove-root`).

use std::{path::PathBuf, sync::mpsc};

use eframe::egui::{self, RichText};
use holo_state::AccountRecord;

use super::{After, App, Page, icons::Icon, theme, widgets};
use crate::actions;

#[derive(Default)]
pub struct State {
    form: Option<AddForm>,
    remove: Option<Remove>,
}

impl State {
    pub fn open_form(&mut self) {
        if self.form.is_none() {
            self.form = Some(AddForm::default());
        }
    }
}

struct AddForm {
    account: Option<String>,
    local: String,
    remote: String,
    create: bool,
    /// Last validated local path and what we found.
    checked: Option<(String, Result<LocalCheck, String>)>,
    picker: Option<mpsc::Receiver<Option<PathBuf>>>,
    /// Confirmation dialog for risky choices is open.
    confirm: bool,
}

impl Default for AddForm {
    fn default() -> Self {
        Self {
            account: None,
            local: String::new(),
            remote: String::new(),
            create: true,
            checked: None,
            picker: None,
            confirm: false,
        }
    }
}

#[derive(Clone)]
struct LocalCheck {
    path: String,
    exists: bool,
    non_empty: bool,
}

/// Two-step removal: understand the consequences, then type the folder name.
struct Remove {
    id: String,
    local_path: String,
    name: String,
    unsent: u64,
    step: u8,
    understood: bool,
    typed: String,
}

fn account_label(a: &AccountRecord) -> String {
    match &a.email {
        Some(e) => format!("{} · {e}", a.provider),
        None => format!("{} · {}", a.provider, a.display_name),
    }
}

/// `/Documents/x` form the CLI expects ("" → drive root).
fn normalize_remote(r: &str) -> String {
    let r = r.trim().replace('\\', "/");
    let r = r.trim_matches('/');
    if r.is_empty() {
        "/".into()
    } else {
        format!("/{r}")
    }
}

pub fn page(app: &mut App, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            widgets::page_title(
                ui,
                "Pastas sincronizadas",
                "Cada pasta local espelha uma pasta da nuvem; os arquivos só baixam quando você abre.",
            );
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
            if app.roots.form.is_none()
                && ui
                    .add_enabled(!app.busy(), widgets::primary_icon(Icon::FolderPlus, "Adicionar pasta"))
                    .clicked()
            {
                app.roots.open_form();
            }
        });
    });

    if app.roots.form.is_some() {
        add_form(app, ui);
        ui.add_space(12.0);
    }

    let roots: Vec<_> = app
        .snap
        .as_ref()
        .map(|s| {
            s.roots
                .iter()
                .map(|r| (r.root.clone(), r.items, r.counts))
                .collect()
        })
        .unwrap_or_default();
    if roots.is_empty() && app.roots.form.is_none() {
        widgets::card(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(10.0);
                widgets::icon_badge(ui, Icon::FolderOpen, theme::CYAN, 48.0);
                ui.label(RichText::new("Nenhuma pasta sincronizada ainda.").strong());
                ui.label(RichText::new("Adicione uma pasta para começar.").color(theme::WEAK));
                ui.add_space(10.0);
            });
        });
    }
    for (root, items, counts) in roots {
        widgets::card(ui, |ui| {
            ui.horizontal(|ui| {
                widgets::icon_badge(ui, Icon::Folder, theme::CYAN, 38.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.label(RichText::new(&root.local_path).strong().size(15.0));
                    let account = app
                        .accounts
                        .iter()
                        .find(|a| a.id == root.account_id)
                        .map(account_label)
                        .unwrap_or_else(|| root.account_id.clone());
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        Icon::Cloud.show(ui, 12.0, theme::WEAK);
                        ui.label(
                            RichText::new(format!("{}   ·   {account}", root.remote_path))
                                .small()
                                .color(theme::WEAK),
                        );
                    });
                    ui.label(
                        RichText::new(format!("{items} itens · id {}", root.id))
                            .small()
                            .color(theme::WEAK),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = if root.enabled {
                        "Remover…"
                    } else {
                        "Tentar remover de novo…"
                    };
                    if ui
                        .add_enabled(
                            !app.busy(),
                            widgets::outline_icon(Icon::Trash, label, theme::DANGER),
                        )
                        .clicked()
                    {
                        let name = std::path::Path::new(&root.local_path)
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| root.local_path.clone());
                        app.roots.remove = Some(Remove {
                            id: root.id.clone(),
                            local_path: root.local_path.clone(),
                            name,
                            unsent: counts.pending + counts.held + counts.failed,
                            step: 1,
                            understood: false,
                            typed: String::new(),
                        });
                    }
                    if ui.add(widgets::button(Icon::FolderOpen, "Abrir")).clicked() {
                        actions::open(std::path::Path::new(&root.local_path));
                    }
                    if !root.enabled {
                        widgets::pill(ui, theme::WARN, "desativada");
                    }
                });
            });
            if !root.enabled {
                ui.add_space(4.0);
                widgets::warning(
                    ui,
                    "Uma remoção anterior não terminou (o Windows não liberou a pasta). A pasta está desativada; tente remover de novo.",
                );
            }
        });
        ui.add_space(8.0);
    }
}

fn add_form(app: &mut App, ui: &mut egui::Ui) {
    let accounts = app.accounts.clone();
    let daemon = app.snap.as_ref().is_some_and(|s| s.daemon);
    let busy = app.busy();
    let Some(f) = app.roots.form.as_mut() else {
        return;
    };

    // Folder picker result.
    if let Some(rx) = &f.picker
        && let Ok(picked) = rx.try_recv()
    {
        f.picker = None;
        if let Some(p) = picked {
            f.local = p.to_string_lossy().into_owned();
        }
    }
    if f.account.is_none() && accounts.len() == 1 {
        f.account = Some(accounts[0].id.clone());
    }

    // Validate the local folder whenever it changes.
    let local = f.local.trim().to_owned();
    if !local.is_empty() && f.checked.as_ref().is_none_or(|(p, _)| *p != local) {
        let r = holo_cli::cmd::roots::validate_local(&app.ctx, std::path::Path::new(&local))
            .map(|p| LocalCheck {
                path: p.to_string_lossy().into_owned(),
                exists: p.is_dir(),
                non_empty: std::fs::read_dir(&p).is_ok_and(|mut d| d.next().is_some()),
            })
            .map_err(|e| format!("{e:#}"));
        f.checked = Some((local.clone(), r));
    }
    let check = f
        .checked
        .as_ref()
        .filter(|(p, _)| !local.is_empty() && *p == local)
        .map(|(_, r)| r.clone());

    let mut close = false;
    let mut submit = false;
    widgets::card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Nova pasta sincronizada").size(16.0).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::icon_button(ui, Icon::X, "Fechar") {
                    close = true;
                }
            });
        });
        ui.add_space(8.0);

        if accounts.is_empty() {
            widgets::warning(ui, "Conecte uma conta antes de adicionar uma pasta.");
            if ui.add(widgets::primary("Conectar conta")).clicked() {
                app.page = Page::Accounts;
                app.accounts_ui.open_wizard();
            }
            return;
        }

        egui::Grid::new("add-root")
            .num_columns(2)
            .spacing([14.0, 10.0])
            .show(ui, |ui| {
                ui.label("Conta");
                let selected = f
                    .account
                    .as_ref()
                    .and_then(|id| accounts.iter().find(|a| &a.id == id))
                    .map(account_label)
                    .unwrap_or_else(|| "Escolha…".into());
                egui::ComboBox::from_id_salt("account")
                    .width(320.0)
                    .selected_text(selected)
                    .show_ui(ui, |ui| {
                        for a in &accounts {
                            ui.selectable_value(
                                &mut f.account,
                                Some(a.id.clone()),
                                account_label(a),
                            );
                        }
                    });
                ui.end_row();

                ui.label("Pasta local");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut f.local)
                            .hint_text(r"D:\OneDrive\Documentos")
                            .desired_width(320.0),
                    );
                    let picking = f.picker.is_some();
                    if ui
                        .add_enabled(!picking, widgets::button(Icon::Search, "Procurar…"))
                        .clicked()
                    {
                        let (tx, rx) = mpsc::channel();
                        let start = f.local.clone();
                        let repaint = ui.ctx().clone();
                        std::thread::spawn(move || {
                            let _ = tx.send(actions::pick_folder(&start));
                            repaint.request_repaint();
                        });
                        f.picker = Some(rx);
                    }
                });
                ui.end_row();

                ui.label("Pasta na nuvem");
                ui.vertical(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut f.remote)
                            .hint_text("/Documentos   (vazio = OneDrive inteiro)")
                            .desired_width(320.0),
                    );
                    ui.checkbox(
                        &mut f.create,
                        "Criar na nuvem se não existir (com as pastas acima dela)",
                    );
                });
                ui.end_row();
            });

        ui.add_space(8.0);
        let remote = normalize_remote(&f.remote);
        match &check {
            Some(Err(e)) => widgets::danger(ui, e.clone()),
            Some(Ok(c)) => {
                if !c.exists {
                    widgets::info(ui, format!("A pasta {} será criada.", c.path));
                } else if c.non_empty {
                    widgets::warning(
                        ui,
                        "A pasta local não está vazia: os arquivos dela serão mesclados com a nuvem. Iguais são vinculados; diferentes ficam como cópias de conflito e são enviados.",
                    );
                }
            }
            None => {}
        }
        if remote == "/" {
            ui.add_space(4.0);
            widgets::warning(
                ui,
                "Você vai sincronizar o OneDrive inteiro. Recomendado começar com uma pasta de teste (ex.: /holo-teste).",
            );
        }
        if !daemon {
            ui.add_space(4.0);
            widgets::info(
                ui,
                "O daemon está parado: a sincronização começa quando você iniciá-lo.",
            );
        }
        ui.add_space(10.0);
        let ready = f.account.is_some() && matches!(check, Some(Ok(_)));
        ui.horizontal(|ui| {
            if ui
                .add_enabled(ready && !busy, widgets::primary("Adicionar pasta"))
                .clicked()
            {
                let risky = remote == "/" || matches!(&check, Some(Ok(c)) if c.non_empty);
                if risky {
                    f.confirm = true;
                } else {
                    submit = true;
                }
            }
            if ui.button("Cancelar").clicked() {
                close = true;
            }
        });
    });

    if close {
        app.roots.form = None;
    } else if submit {
        start_add(app);
    }
}

fn start_add(app: &mut App) {
    let Some(f) = app.roots.form.as_mut() else {
        return;
    };
    f.confirm = false;
    let Some(account) = f.account.clone() else {
        return;
    };
    let local = f.local.trim().to_owned();
    let remote = normalize_remote(&f.remote);
    let mut args = vec![
        "add-root".into(),
        local.clone(),
        "--remote".into(),
        remote.clone(),
        "--account".into(),
        account,
    ];
    if f.create {
        args.push("--create".into());
    }
    let daemon = app.snap.as_ref().is_some_and(|s| s.daemon);
    let ok = if daemon {
        format!(
            "Pasta adicionada: {local} (nuvem: {remote}). O daemon começa a sincronizar em instantes."
        )
    } else {
        format!("Pasta adicionada: {local} (nuvem: {remote}). Inicie o daemon para sincronizar.")
    };
    let after: After = Box::new(|app, r| {
        if r.is_ok() {
            app.roots.form = None;
        }
    });
    app.start_holo("Adicionando pasta…", args, Some(ok), Some(after));
}

pub fn modals(app: &mut App, ctx: &egui::Context) {
    confirm_add(app, ctx);
    confirm_remove(app, ctx);
}

fn confirm_add(app: &mut App, ctx: &egui::Context) {
    let Some(f) = app.roots.form.as_ref().filter(|f| f.confirm) else {
        return;
    };
    let remote = normalize_remote(&f.remote);
    let non_empty = f
        .checked
        .as_ref()
        .is_some_and(|(_, r)| matches!(r, Ok(c) if c.non_empty));
    let local = f.local.trim().to_owned();
    let mut go = false;
    let mut cancel = false;
    let r = egui::Modal::new(egui::Id::unique("confirm-add")).show(ctx, |ui| {
        ui.set_width(480.0);
        ui.label(RichText::new("Confirmar nova pasta").size(17.0).strong());
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            Icon::Folder.show(ui, 14.0, theme::CYAN);
            ui.label(RichText::new(&local).monospace());
            Icon::Sync.show(ui, 14.0, theme::WEAK);
            Icon::Cloud.show(ui, 14.0, theme::CYAN);
            ui.label(RichText::new(&remote).monospace());
        });
        ui.add_space(8.0);
        if remote == "/" {
            widgets::warning(
                ui,
                "O OneDrive inteiro aparecerá nesta pasta. A primeira listagem pode demorar em drives grandes.",
            );
            ui.add_space(4.0);
        }
        if non_empty {
            widgets::warning(
                ui,
                "Os arquivos que já estão nesta pasta serão enviados para a nuvem (ou mantidos como cópias de conflito se diferirem).",
            );
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.add(widgets::primary("Sim, adicionar")).clicked() {
                go = true;
            }
            if ui.button("Voltar").clicked() {
                cancel = true;
            }
        });
    });
    if go {
        start_add(app);
    } else if (cancel || r.should_close())
        && let Some(f) = app.roots.form.as_mut()
    {
        f.confirm = false;
    }
}

fn confirm_remove(app: &mut App, ctx: &egui::Context) {
    let busy = app.busy();
    let Some(rm) = app.roots.remove.as_mut() else {
        return;
    };
    let mut close = false;
    let mut go = false;
    let r = egui::Modal::new(egui::Id::unique("confirm-remove")).show(ctx, |ui| {
        ui.set_width(500.0);
        ui.label(RichText::new("Remover pasta sincronizada?").size(17.0).strong());
        ui.label(RichText::new(&rm.local_path).monospace().color(theme::WEAK));
        ui.add_space(8.0);
        if rm.step == 1 {
            ui.label("O que acontece:");
            for line in [
                "A pasta deixa de sincronizar e sai do painel de navegação do Explorer.",
                "Arquivos que estão só na nuvem SOMEM desta pasta (continuam na nuvem).",
                "Arquivos já baixados ficam como arquivos normais, sem sincronizar.",
                "Nada é apagado da nuvem.",
            ] {
                ui.label(format!("  •  {line}"));
            }
            ui.add_space(6.0);
            if rm.unsent > 0 {
                widgets::danger(
                    ui,
                    format!(
                        "{} operação(ões) ainda não concluídas serão descartadas: alterações locais recentes podem não chegar à nuvem.",
                        rm.unsent
                    ),
                );
                ui.add_space(6.0);
            }
            ui.checkbox(&mut rm.understood, "Entendi as consequências");
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(rm.understood, widgets::outline("Continuar", theme::DANGER))
                    .clicked()
                {
                    rm.step = 2;
                }
                if ui.button("Cancelar").clicked() {
                    close = true;
                }
            });
        } else {
            ui.label("Para confirmar, digite o nome da pasta:");
            ui.label(RichText::new(&rm.name).monospace().strong().color(theme::DANGER));
            let edit = ui.add(egui::TextEdit::singleline(&mut rm.typed).desired_width(f32::INFINITY));
            edit.request_focus();
            let matches = rm.typed.trim().eq_ignore_ascii_case(&rm.name);
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(matches && !busy, widgets::danger_button("Remover definitivamente"))
                    .clicked()
                {
                    go = true;
                }
                if ui.button("Voltar").clicked() {
                    rm.step = 1;
                }
                if ui.button("Cancelar").clicked() {
                    close = true;
                }
            });
        }
    });
    if go {
        let rm = app.roots.remove.take().expect("open");
        let ok = format!("Pasta removida: {}", rm.local_path);
        app.start_holo(
            "Removendo pasta (pode levar alguns segundos)…",
            vec!["remove-root".into(), rm.id, "--yes".into()],
            Some(ok),
            None,
        );
    } else if close || r.should_close() {
        app.roots.remove = None;
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_remote;

    #[test]
    fn remote_paths() {
        assert_eq!(normalize_remote(""), "/");
        assert_eq!(normalize_remote(" / "), "/");
        assert_eq!(normalize_remote("Docs"), "/Docs");
        assert_eq!(normalize_remote(r"\Docs\a\"), "/Docs/a");
    }
}
