//! "Configurações": edit `config.toml` (saved keeping its comments) plus the
//! "start with Windows" switch.

use eframe::egui::{self, RichText};
use holo_cli::config::{ConcurrencySetting, Config, UpdateMode};
use holo_engine::EngineConfig;

use super::{
    App, Page, Tone,
    accounts::{self, Tenant},
    icons::Icon,
    theme, widgets,
};
use crate::actions;

pub struct State {
    draft: Config,
    fixed: bool,
    fixed_n: usize,
    ignore: String,
    blocked_custom: bool,
    blocked: String,
    tenant: Tenant,
    tenant_custom: String,
    /// Saved while the daemon runs: it still uses the old values.
    restart_hint: bool,
}

fn lines(v: &[String]) -> String {
    v.join("\n")
}

fn parse_lines(s: &str) -> Vec<String> {
    s.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

impl State {
    pub fn new(cfg: &Config) -> Self {
        let mut s = Self {
            draft: cfg.clone(),
            fixed: false,
            fixed_n: 4,
            ignore: String::new(),
            blocked_custom: false,
            blocked: String::new(),
            tenant: Tenant::Common,
            tenant_custom: String::new(),
            restart_hint: false,
        };
        s.sync_from(cfg);
        s
    }

    /// Discard edits: show `cfg`.
    pub fn sync_from(&mut self, cfg: &Config) {
        self.draft = cfg.clone();
        match cfg.sync.concurrency {
            ConcurrencySetting::Fixed(n) => {
                self.fixed = true;
                self.fixed_n = n.max(1);
            }
            ConcurrencySetting::Mode(_) => self.fixed = false,
        }
        self.ignore = lines(&cfg.sync.ignore);
        self.blocked_custom = cfg.sync.blocked_hydration_processes.is_some();
        self.blocked = lines(
            cfg.sync
                .blocked_hydration_processes
                .as_deref()
                .unwrap_or(&EngineConfig::default().blocked_hydration_processes),
        );
        self.onedrive_from(cfg);
    }

    /// Only the OneDrive app fields (after the sign-in wizard saved them).
    pub fn onedrive_from(&mut self, cfg: &Config) {
        self.draft.onedrive = cfg.onedrive.clone();
        (self.tenant, self.tenant_custom) = Tenant::from_config(cfg.onedrive.tenant.as_deref());
    }

    fn build(&self) -> Config {
        let mut c = self.draft.clone();
        c.sync.concurrency = if self.fixed {
            ConcurrencySetting::Fixed(self.fixed_n.max(1))
        } else {
            ConcurrencySetting::Mode("auto".into())
        };
        c.sync.concurrency_max = c.sync.concurrency_max.max(c.sync.concurrency_min);
        c.sync.ignore = parse_lines(&self.ignore);
        c.sync.blocked_hydration_processes =
            self.blocked_custom.then(|| parse_lines(&self.blocked));
        c.onedrive.client_id = c.onedrive.client_id.trim().to_owned();
        c.onedrive.tenant = self.tenant.to_config(&self.tenant_custom);
        c
    }
}

fn help(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).small().color(theme::WEAK));
}

fn row(ui: &mut egui::Ui, label: &str, hint: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 1.0;
        ui.label(label);
        if !hint.is_empty() {
            help(ui, hint);
        }
    });
    add(ui);
    ui.end_row();
}

fn grid(ui: &mut egui::Ui, id: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Grid::new(id)
        .num_columns(2)
        .min_col_width(260.0)
        .spacing([20.0, 12.0])
        .show(ui, add);
}

pub fn page(app: &mut App, ui: &mut egui::Ui) {
    let built = app.settings.build();
    let dirty = built != app.ctx.cfg;
    let daemon = app.snap.as_ref().is_some_and(|s| s.daemon);

    let mut save = false;
    let mut discard = false;
    let action_bar = |ui: &mut egui::Ui, save: &mut bool, discard: &mut bool| {
        ui.horizontal(|ui| {
            if ui
                .add_enabled(dirty, widgets::primary_icon(Icon::Save, "Salvar"))
                .clicked()
            {
                *save = true;
            }
            if ui
                .add_enabled(dirty, egui::Button::new("Descartar alterações"))
                .clicked()
            {
                *discard = true;
            }
            if dirty {
                ui.label(RichText::new("alterações não salvas").color(theme::WARN));
            }
        });
    };

    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            widgets::page_title(
                ui,
                "Configurações",
                "Salvas em config.toml (os comentários do arquivo são mantidos).",
            );
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
            action_bar(ui, &mut save, &mut discard);
        });
    });

    if app.settings.restart_hint && daemon {
        widgets::callout(ui, theme::CYAN, |ui| {
            ui.horizontal(|ui| {
                Icon::Info.show(ui, 16.0, theme::CYAN);
                ui.label(
                    "O daemon lê a configuração ao iniciar: reinicie-o para aplicar as mudanças.",
                );
                if ui
                    .add_enabled(!app.busy(), widgets::primary("Reiniciar daemon"))
                    .clicked()
                {
                    app.settings.restart_hint = false;
                    app.restart_daemon();
                }
            });
        });
        ui.add_space(8.0);
    }

    // ------------------------------------------------------------ general
    widgets::card(ui, |ui| {
        widgets::section(ui, "Geral");
        grid(ui, "general", |ui| {
            row(
                ui,
                "Iniciar com o Windows",
                "Ícone na bandeja ao entrar (aplica na hora).",
                |ui| {
                    let mut auto = app.autostart;
                    if ui.checkbox(&mut auto, "").changed() {
                        match actions::set_autostart(auto) {
                            Ok(()) => app.autostart = auto,
                            Err(e) => app.say(Tone::Error, format!("{e:#}")),
                        }
                    }
                },
            );
            row(ui, "Arquivos", "", |ui| {
                ui.horizontal(|ui| {
                    if ui.button("Abrir config.toml").clicked() {
                        actions::open(&app.ctx.paths.config_file);
                    }
                    if ui.button("Pasta de dados").clicked() {
                        actions::open(&app.ctx.paths.data);
                    }
                    if ui.button("Logs").clicked() {
                        actions::open(&app.ctx.paths.logs());
                    }
                });
            });
        });
    });
    ui.add_space(10.0);

    let s = &mut app.settings;
    // ------------------------------------------------------------ sync
    widgets::card(ui, |ui| {
        widgets::section(ui, "Sincronização");
        grid(ui, "sync", |ui| {
            let d = &mut s.draft.sync;
            row(
                ui,
                "Verificar a nuvem a cada",
                "Busca mudanças feitas em outros dispositivos.",
                |ui| {
                    ui.add(
                        egui::DragValue::new(&mut d.poll_interval_secs)
                            .range(5..=3600)
                            .suffix(" s"),
                    );
                },
            );
            row(
                ui,
                "Varredura local completa a cada",
                "Confere a pasta inteira, além dos avisos do Windows.",
                |ui| {
                    ui.add(
                        egui::DragValue::new(&mut d.full_scan_interval_mins)
                            .range(1..=1440)
                            .suffix(" min"),
                    );
                },
            );
            row(
                ui,
                "Proteção contra exclusão em massa",
                "Mais exclusões que isso de uma vez ficam retidas até você confirmar.",
                |ui| {
                    ui.add(
                        egui::DragValue::new(&mut d.mass_delete_threshold)
                            .range(1..=1_000_000)
                            .suffix(" itens"),
                    );
                },
            );
        });
    });
    ui.add_space(10.0);

    // ------------------------------------------------------------ concurrency
    widgets::card(ui, |ui| {
        widgets::section(ui, "Envios em paralelo");
        grid(ui, "concurrency", |ui| {
            row(
                ui,
                "Modo",
                "Automático se adapta à fila, ao uso do PC e a limites do servidor.",
                |ui| {
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut s.fixed, false, "Automático");
                        ui.radio_value(&mut s.fixed, true, "Fixo");
                    });
                },
            );
            let d = &mut s.draft.sync;
            if s.fixed {
                row(ui, "Envios simultâneos", "", |ui| {
                    ui.add(egui::DragValue::new(&mut s.fixed_n).range(1..=64));
                });
            } else {
                row(ui, "Mínimo / máximo", "", |ui| {
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut d.concurrency_min).range(1..=64));
                        ui.label("até");
                        let min = d.concurrency_min;
                        ui.add(egui::DragValue::new(&mut d.concurrency_max).range(min..=64));
                    });
                });
            }
        });
    });
    ui.add_space(10.0);

    // ------------------------------------------------------------ hydration
    widgets::card(ui, |ui| {
        widgets::section(ui, "Downloads sob demanda");
        grid(ui, "hydrate", |ui| {
            row(
                ui,
                "Leitura antecipada",
                "Mínimo baixado por pedido ao abrir um arquivo.",
                |ui| {
                    ui.add(
                        egui::DragValue::new(&mut s.draft.sync.read_ahead_mib)
                            .range(1..=256)
                            .suffix(" MiB"),
                    );
                },
            );
            row(
                ui,
                "Programas bloqueados",
                "Não baixam arquivos só da nuvem (ex.: o indexador de pesquisa).",
                |ui| {
                    ui.vertical(|ui| {
                        ui.checkbox(&mut s.blocked_custom, "Personalizar a lista");
                        ui.add_enabled(
                            s.blocked_custom,
                            egui::TextEdit::multiline(&mut s.blocked)
                                .hint_text("um executável por linha")
                                .desired_rows(3)
                                .desired_width(320.0)
                                .font(egui::TextStyle::Monospace),
                        );
                    });
                },
            );
        });
    });
    ui.add_space(10.0);

    // ------------------------------------------------------------ ignore
    widgets::card(ui, |ui| {
        widgets::section(ui, "Ignorar");
        grid(ui, "ignore", |ui| {
            row(
                ui,
                "Regras padrão",
                "Arquivos temporários do Office, Thumbs.db, desktop.ini etc.",
                |ui| {
                    ui.checkbox(&mut s.draft.sync.use_default_ignores, "Usar");
                },
            );
            row(
                ui,
                "Padrões extras",
                "Estilo .gitignore, um por linha; valem para todas as pastas.",
                |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut s.ignore)
                            .hint_text("*.tmp\nnode_modules/")
                            .desired_rows(4)
                            .desired_width(320.0)
                            .font(egui::TextStyle::Monospace),
                    );
                },
            );
        });
    });
    ui.add_space(10.0);

    // ------------------------------------------------------------ updates
    widgets::card(ui, |ui| {
        widgets::section(ui, "Atualizações");
        grid(ui, "update", |ui| {
            let u = &mut s.draft.update;
            row(
                ui,
                "Quando houver versão nova",
                "Lido pelo ícone da bandeja ao iniciar.",
                |ui| {
                    ui.vertical(|ui| {
                        ui.radio_value(
                            &mut u.mode,
                            UpdateMode::Notify,
                            "Baixar e avisar no menu da bandeja",
                        );
                        ui.radio_value(
                            &mut u.mode,
                            UpdateMode::Auto,
                            "Instalar sozinho quando nada estiver sincronizando",
                        );
                        ui.radio_value(&mut u.mode, UpdateMode::Off, "Não verificar");
                    });
                },
            );
            if u.mode != UpdateMode::Off {
                row(ui, "Verificar a cada", "", |ui| {
                    ui.add(
                        egui::DragValue::new(&mut u.check_interval_hours)
                            .range(1..=168)
                            .suffix(" h"),
                    );
                });
            }
        });
    });
    ui.add_space(10.0);

    // ------------------------------------------------------------ onedrive
    let mut to_wizard = false;
    widgets::card(ui, |ui| {
        widgets::section(ui, "OneDrive (registro do aplicativo)");
        accounts::onedrive_fields(
            ui,
            &mut s.draft.onedrive.client_id,
            &mut s.tenant,
            &mut s.tenant_custom,
        );
        ui.add_space(4.0);
        if ui
            .link("Como obter o client_id? Ver o passo a passo")
            .clicked()
        {
            to_wizard = true;
        }
        if !app.accounts.is_empty() {
            help(
                ui,
                "Trocar o client_id exige entrar de novo nas contas já conectadas.",
            );
        }
    });
    ui.add_space(10.0);

    ui.horizontal(|ui| {
        action_bar(ui, &mut save, &mut discard);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .button("Restaurar padrões")
                .on_hover_text("Volta sincronização e atualizações aos valores padrão (não mexe no client_id). Ainda precisa salvar.")
                .clicked()
            {
                let d = Config {
                    onedrive: app.settings.draft.onedrive.clone(),
                    gdrive: app.settings.draft.gdrive.clone(),
                    ..Default::default()
                };
                let (t, c) = (app.settings.tenant, app.settings.tenant_custom.clone());
                app.settings.sync_from(&d);
                (app.settings.tenant, app.settings.tenant_custom) = (t, c);
            }
        });
    });

    if to_wizard {
        app.page = Page::Accounts;
        app.accounts_ui.open_wizard();
    }
    if discard {
        let cfg = app.ctx.cfg.clone();
        app.settings.sync_from(&cfg);
    }
    if save {
        let cfg = app.settings.build();
        if !cfg.onedrive.client_id.is_empty() && !accounts::valid_client_id(&cfg.onedrive.client_id)
        {
            app.say(Tone::Error, "client_id inválido: deve ser um GUID.");
            return;
        }
        match cfg.save(&app.ctx.paths) {
            Ok(()) => {
                app.ctx.cfg = cfg.clone();
                app.settings.sync_from(&cfg);
                app.settings.restart_hint = true;
                app.say(Tone::Ok, "Configurações salvas.");
            }
            Err(e) => app.say(Tone::Error, format!("Não consegui salvar: {e:#}")),
        }
    }
}
