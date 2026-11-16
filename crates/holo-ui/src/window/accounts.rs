//! "Contas": connected accounts, sign-in wizard (provider → client_id → browser login)
//! and sign-out. Login and logout run `holo login` / `holo logout`.

use eframe::egui::{self, Color32, CornerRadius, Margin, RichText, Stroke};
use holo_core::ProviderKind;

use super::{After, App, Page, Tone, icons::Icon, theme, widgets};
use crate::actions;

const AZURE_APPS: &str =
    "https://portal.azure.com/#view/Microsoft_AAD_RegisteredApps/ApplicationsListBlade";
const REDIRECT_URI: &str = "http://localhost";
const SCOPES: [&str; 3] = ["Files.ReadWrite", "User.Read", "offline_access"];

#[derive(Default)]
pub struct State {
    wizard: Option<Wizard>,
    logout: Option<(String, String)>,
}

impl State {
    pub fn open_wizard(&mut self) {
        if self.wizard.is_none() {
            self.wizard = Some(Wizard::default());
        }
    }
}

#[derive(Default, PartialEq, Clone, Copy)]
enum Step {
    #[default]
    Provider,
    ClientId,
    SignIn,
}

#[derive(Default)]
struct Wizard {
    step: Step,
    /// Editing the client id (pre-filled from the config).
    client_id: String,
    tenant: Tenant,
    tenant_custom: String,
    show_guide: bool,
    loaded: bool,
    error: Option<String>,
}

/// `[onedrive].tenant` choices.
#[derive(Default, PartialEq, Clone, Copy)]
pub enum Tenant {
    #[default]
    Common,
    Consumers,
    Organizations,
    Custom,
}

impl Tenant {
    pub fn from_config(t: Option<&str>) -> (Self, String) {
        match t.map(str::trim) {
            None | Some("") | Some("common") => (Self::Common, String::new()),
            Some("consumers") => (Self::Consumers, String::new()),
            Some("organizations") => (Self::Organizations, String::new()),
            Some(other) => (Self::Custom, other.to_owned()),
        }
    }

    /// Value for the config (`None` = default `common`).
    pub fn to_config(self, custom: &str) -> Option<String> {
        match self {
            Self::Common => None,
            Self::Consumers => Some("consumers".into()),
            Self::Organizations => Some("organizations".into()),
            Self::Custom => Some(custom.trim().to_owned()).filter(|s| !s.is_empty()),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Common => "Pessoais e de trabalho/escola (common)",
            Self::Consumers => "Somente contas pessoais (consumers)",
            Self::Organizations => "Somente trabalho/escola (organizations)",
            Self::Custom => "Um tenant específico (ID)",
        }
    }
}

/// Application (client) IDs are GUIDs.
pub fn valid_client_id(s: &str) -> bool {
    let s = s.trim();
    let groups: Vec<&str> = s.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(g, n)| g.len() == n && g.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Client id + tenant fields (also used on the settings page).
pub fn onedrive_fields(
    ui: &mut egui::Ui,
    client_id: &mut String,
    tenant: &mut Tenant,
    custom: &mut String,
) {
    egui::Grid::new(ui.scope_id().with("onedrive-fields"))
        .num_columns(2)
        .spacing([14.0, 10.0])
        .show(ui, |ui| {
            ui.label("Application (client) ID");
            ui.vertical(|ui| {
                ui.add(
                    egui::TextEdit::singleline(client_id)
                        .hint_text("00000000-0000-0000-0000-000000000000")
                        .font(egui::TextStyle::Monospace)
                        .desired_width(340.0),
                );
                if !client_id.trim().is_empty() && !valid_client_id(client_id) {
                    ui.label(
                        RichText::new(
                            "Formato esperado: GUID, ex. 1a2b3c4d-1234-5678-9abc-0123456789ab",
                        )
                        .small()
                        .color(theme::DANGER),
                    );
                }
            });
            ui.end_row();
            ui.label("Tipos de conta");
            ui.vertical(|ui| {
                egui::ComboBox::from_id_salt(ui.scope_id().with("tenant"))
                    .width(340.0)
                    .selected_text(tenant.label())
                    .show_ui(ui, |ui| {
                        for t in [
                            Tenant::Common,
                            Tenant::Consumers,
                            Tenant::Organizations,
                            Tenant::Custom,
                        ] {
                            ui.selectable_value(tenant, t, t.label());
                        }
                    });
                if *tenant == Tenant::Custom {
                    ui.add(
                        egui::TextEdit::singleline(custom)
                            .hint_text("ID ou domínio do tenant")
                            .desired_width(340.0),
                    );
                }
                ui.label(
                    RichText::new("Deve bater com \"Supported account types\" do registro do app.")
                        .small()
                        .color(theme::WEAK),
                );
            });
            ui.end_row();
        });
}

pub fn page(app: &mut App, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            widgets::page_title(
                ui,
                "Contas",
                "Contas de nuvem conectadas a este computador.",
            );
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
            if app.accounts_ui.wizard.is_none()
                && ui
                    .add_enabled(
                        !app.busy(),
                        widgets::primary_icon(Icon::UserPlus, "Conectar conta"),
                    )
                    .clicked()
            {
                app.accounts_ui.open_wizard();
            }
        });
    });

    if app.accounts_ui.wizard.is_some() {
        wizard(app, ui);
        ui.add_space(12.0);
    }

    if app.accounts.is_empty() && app.accounts_ui.wizard.is_none() {
        widgets::card(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(10.0);
                widgets::icon_badge(ui, Icon::User, theme::CYAN, 48.0);
                ui.label(RichText::new("Nenhuma conta conectada.").strong());
                ui.add_space(10.0);
            });
        });
    }

    let roots: Vec<String> = app
        .snap
        .as_ref()
        .map(|s| s.roots.iter().map(|r| r.root.account_id.clone()).collect())
        .unwrap_or_default();
    for a in app.accounts.clone() {
        let n = roots.iter().filter(|id| **id == a.id).count();
        widgets::card(ui, |ui| {
            ui.horizontal(|ui| {
                provider_badge(ui, a.provider);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.label(RichText::new(&a.display_name).strong().size(15.0));
                    if let Some(e) = &a.email {
                        ui.label(RichText::new(e).color(theme::WEAK));
                    }
                    ui.label(
                        RichText::new(format!(
                            "id {} · {n} pasta(s) · desde {}",
                            a.id,
                            a.created_at.format("%d/%m/%Y")
                        ))
                        .small()
                        .color(theme::WEAK),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let r = ui.add_enabled(
                        n == 0 && !app.busy(),
                        widgets::outline_icon(Icon::Logout, "Sair", theme::DANGER),
                    );
                    let r = if n > 0 {
                        r.on_disabled_hover_text(
                            "Remova as pastas desta conta primeiro (página Pastas).",
                        )
                    } else {
                        r
                    };
                    if r.clicked() {
                        app.accounts_ui.logout = Some((
                            a.id.clone(),
                            a.email.clone().unwrap_or(a.display_name.clone()),
                        ));
                    }
                    if n > 0 && ui.button("Ver pastas").clicked() {
                        app.page = Page::Roots;
                    }
                });
            });
        });
        ui.add_space(8.0);
    }
}

fn provider_badge(ui: &mut egui::Ui, kind: ProviderKind) {
    let color = match kind {
        ProviderKind::OneDrive => theme::BLUE,
        ProviderKind::GoogleDrive => theme::MINT,
    };
    widgets::icon_badge(ui, Icon::Cloud, color, 38.0);
}

/// Big selectable provider tile.
fn provider_tile(ui: &mut egui::Ui, enabled: bool, name: &str, note: &str, color: Color32) -> bool {
    let frame = egui::Frame::new()
        .fill(if enabled { theme::CARD_HI } else { theme::CARD })
        .stroke(Stroke::new(
            1.0,
            if enabled {
                color.linear_multiply(0.6)
            } else {
                theme::BORDER
            },
        ))
        .corner_radius(CornerRadius::same(theme::RADIUS + 2))
        .inner_margin(Margin::same(16));
    let resp = ui
        .add_enabled_ui(enabled, |ui| {
            frame
                .show(ui, |ui| {
                    ui.set_width(220.0);
                    ui.vertical(|ui| {
                        widgets::icon_badge(
                            ui,
                            Icon::Cloud,
                            if enabled { color } else { theme::WEAK },
                            40.0,
                        );
                        ui.label(RichText::new(name).size(16.0).strong());
                        ui.label(RichText::new(note).small().color(theme::WEAK));
                    });
                })
                .response
        })
        .inner;
    let resp = resp.interact(egui::Sense::click());
    enabled
        && resp
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
}

fn wizard(app: &mut App, ui: &mut egui::Ui) {
    let busy = app.busy();
    let cfg_client = app.ctx.cfg.onedrive.client_id.trim().to_owned();
    let cfg_tenant = app.ctx.cfg.onedrive.tenant.clone();
    let Some(w) = app.accounts_ui.wizard.as_mut() else {
        return;
    };
    if !w.loaded {
        w.loaded = true;
        w.client_id = cfg_client.clone();
        (w.tenant, w.tenant_custom) = Tenant::from_config(cfg_tenant.as_deref());
        w.show_guide = cfg_client.is_empty();
    }

    let mut close = false;
    let mut save_client = false;
    let mut login = false;
    widgets::card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Conectar conta").size(16.0).strong());
            ui.add_space(12.0);
            for (i, (s, name)) in [
                (Step::Provider, "Serviço"),
                (Step::ClientId, "App (client_id)"),
                (Step::SignIn, "Entrar"),
            ]
            .into_iter()
            .enumerate()
            {
                let current = w.step == s;
                let done = (w.step as u8) > (s as u8);
                widgets::step_number(ui, i + 1, done);
                ui.label(RichText::new(name).color(if current {
                    theme::TEXT
                } else {
                    theme::WEAK
                }));
                ui.add_space(6.0);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        !busy,
                        egui::Button::new(Icon::X.image(14.0, theme::WEAK)).frame(false),
                    )
                    .on_hover_text("Fechar")
                    .clicked()
                {
                    close = true;
                }
            });
        });
        ui.separator();
        ui.add_space(6.0);

        match w.step {
            Step::Provider => {
                ui.label("Qual serviço de nuvem você quer conectar?");
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if provider_tile(
                        ui,
                        true,
                        "OneDrive",
                        "Pessoal, trabalho ou escola",
                        theme::BLUE,
                    ) {
                        w.step = Step::ClientId;
                    }
                    ui.add_space(8.0);
                    provider_tile(ui, false, "Google Drive", "Em breve", theme::MINT);
                });
            }
            Step::ClientId => {
                ui.label(
                    "O holo usa um registro de aplicativo seu no Microsoft Entra ID (gratuito). \
                     Você só precisa fazer isso uma vez.",
                );
                ui.add_space(6.0);
                if !cfg_client.is_empty() && !w.show_guide {
                    widgets::callout(ui, theme::MINT, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("client_id já configurado:").color(theme::MINT));
                            ui.label(RichText::new(&cfg_client).monospace());
                        });
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.add(widgets::primary("Continuar")).clicked() {
                            w.step = Step::SignIn;
                        }
                        if ui.button("Alterar / ver passo a passo").clicked() {
                            w.show_guide = true;
                        }
                    });
                } else {
                    guide(ui);
                    ui.add_space(10.0);
                    widgets::section(ui, "Cole aqui o ID do aplicativo");
                    onedrive_fields(ui, &mut w.client_id, &mut w.tenant, &mut w.tenant_custom);
                    if let Some(e) = &w.error {
                        ui.add_space(4.0);
                        widgets::danger(ui, e.clone());
                    }
                    ui.add_space(10.0);
                    let valid = valid_client_id(&w.client_id)
                        && (w.tenant != Tenant::Custom || !w.tenant_custom.trim().is_empty());
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(valid, widgets::primary("Salvar e continuar"))
                            .clicked()
                        {
                            save_client = true;
                        }
                        if ui.button("Voltar").clicked() {
                            w.step = Step::Provider;
                        }
                    });
                }
            }
            Step::SignIn => {
                ui.label("Clique abaixo: o navegador abre na página de login da Microsoft.");
                ui.label(
                    RichText::new(
                        "Entre com a conta do OneDrive e aceite as permissões. Ao terminar, volte para esta janela.",
                    )
                    .color(theme::WEAK),
                );
                ui.add_space(8.0);
                if busy {
                    widgets::callout(ui, theme::CYAN, |ui| {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Aguardando o login no navegador…");
                        });
                        ui.label(
                            RichText::new(
                                "Fechou a aba sem querer? Cancele (no topo) e tente de novo.",
                            )
                            .small()
                            .color(theme::WEAK),
                        );
                    });
                } else {
                    if let Some(e) = &w.error {
                        widgets::danger(ui, e.clone());
                        ui.add_space(6.0);
                    }
                    ui.horizontal(|ui| {
                        if ui
                            .add(widgets::primary_icon(Icon::Login, "Entrar com a Microsoft"))
                            .clicked()
                        {
                            login = true;
                        }
                        if ui.button("Voltar").clicked() {
                            w.step = Step::ClientId;
                        }
                    });
                }
            }
        }
    });

    if close {
        app.accounts_ui.wizard = None;
    } else if save_client {
        save_client_id(app);
    } else if login {
        start_login(app);
    }
}

fn guide(ui: &mut egui::Ui) {
    widgets::section(ui, "Passo a passo: registrar o aplicativo");
    let step = |ui: &mut egui::Ui, n: usize, add: &mut dyn FnMut(&mut egui::Ui)| {
        ui.horizontal_top(|ui| {
            widgets::step_number(ui, n, false);
            ui.vertical(|ui| add(ui));
        });
        ui.add_space(4.0);
    };
    step(ui, 1, &mut |ui| {
        ui.label("Abra \"App registrations\" no portal do Azure (entre com qualquer conta Microsoft) e clique em New registration.");
        if ui
            .add(widgets::button(Icon::External, "Abrir o portal do Azure"))
            .clicked()
        {
            actions::open_url(AZURE_APPS);
        }
    });
    step(ui, 2, &mut |ui| {
        ui.label("Name: qualquer nome (ex.: holo).");
        ui.label("Supported account types: Accounts in any organizational directory and personal Microsoft accounts.");
        ui.label(
            RichText::new("Só vai usar conta pessoal? \"Personal Microsoft accounts only\" também serve: escolha \"Somente contas pessoais\" abaixo.")
                .small()
                .color(theme::WEAK),
        );
    });
    step(ui, 3, &mut |ui| {
        ui.label(
            "Redirect URI: plataforma \"Public client/native (mobile & desktop)\" com o endereço:",
        );
        widgets::copy_chip(ui, REDIRECT_URI);
        ui.label("Clique em Register.");
    });
    step(ui, 4, &mut |ui| {
        ui.label("Em Authentication > Advanced settings, ative \"Allow public client flows\" (Yes) e salve.");
    });
    step(ui, 5, &mut |ui| {
        ui.label("Em API permissions > Add a permission > Microsoft Graph > Delegated permissions, adicione:");
        ui.horizontal_wrapped(|ui| {
            for s in SCOPES {
                widgets::copy_chip(ui, s);
            }
        });
    });
    step(ui, 6, &mut |ui| {
        ui.label("Em Overview, copie o \"Application (client) ID\" e cole abaixo.");
    });
}

fn save_client_id(app: &mut App) {
    let Some(w) = app.accounts_ui.wizard.as_mut() else {
        return;
    };
    let mut cfg = app.ctx.cfg.clone();
    cfg.onedrive.client_id = w.client_id.trim().to_owned();
    cfg.onedrive.tenant = w.tenant.to_config(&w.tenant_custom);
    match cfg.save(&app.ctx.paths) {
        Ok(()) => {
            w.error = None;
            w.show_guide = false;
            w.step = Step::SignIn;
            app.ctx.cfg = cfg;
            app.settings.onedrive_from(&app.ctx.cfg);
            app.say(Tone::Ok, "client_id salvo na configuração.");
        }
        Err(e) => w.error = Some(format!("Não consegui salvar a configuração: {e:#}")),
    }
}

fn start_login(app: &mut App) {
    if let Some(w) = app.accounts_ui.wizard.as_mut() {
        w.error = None;
    }
    let after: After = Box::new(|app, r| match r {
        Ok(_) => {
            app.accounts_ui.wizard = None;
            app.refresh();
            if app.snap.as_ref().is_some_and(|s| s.roots.is_empty()) {
                app.page = Page::Roots;
                app.roots.open_form();
            }
        }
        Err(e) => {
            if let Some(w) = app.accounts_ui.wizard.as_mut() {
                w.error = Some(e.clone());
            }
        }
    });
    app.start_holo(
        "Aguardando o login no navegador…",
        vec!["login".into(), "--provider".into(), "onedrive".into()],
        None,
        Some(after),
    );
}

pub fn modals(app: &mut App, ctx: &egui::Context) {
    let Some((id, who)) = app.accounts_ui.logout.clone() else {
        return;
    };
    let mut go = false;
    let mut close = false;
    let r = egui::Modal::new(egui::Id::unique("confirm-logout")).show(ctx, |ui| {
        ui.set_width(440.0);
        ui.label(RichText::new("Sair da conta?").size(17.0).strong());
        ui.label(RichText::new(&who).color(theme::WEAK));
        ui.add_space(8.0);
        ui.label("Os tokens de acesso salvos neste computador serão apagados. Nada muda na nuvem; para usar a conta de novo, conecte-a outra vez.");
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.add(widgets::danger_button("Sair da conta")).clicked() {
                go = true;
            }
            if ui.button("Cancelar").clicked() {
                close = true;
            }
        });
    });
    if go {
        app.accounts_ui.logout = None;
        app.start_holo(
            "Saindo da conta…",
            vec!["logout".into(), id],
            Some(format!("Conta desconectada: {who}")),
            None,
        );
    } else if close || r.should_close() {
        app.accounts_ui.logout = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_id_format() {
        assert!(valid_client_id("1a2b3c4d-1234-5678-9abc-0123456789ab"));
        assert!(valid_client_id(" 1A2B3C4D-1234-5678-9ABC-0123456789AB "));
        assert!(!valid_client_id("1a2b3c4d-1234-5678-9abc"));
        assert!(!valid_client_id("zzzzzzzz-1234-5678-9abc-0123456789ab"));
    }

    #[test]
    fn tenant_roundtrip() {
        for t in [
            None,
            Some("consumers"),
            Some("organizations"),
            Some("contoso.onmicrosoft.com"),
        ] {
            let (k, c) = Tenant::from_config(t);
            assert_eq!(k.to_config(&c).as_deref(), t);
        }
        assert_eq!(Tenant::from_config(Some("common")).0.to_config(""), None);
    }
}
