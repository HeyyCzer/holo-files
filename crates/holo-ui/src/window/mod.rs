//! Status window (`holo-ui --window`): overview of the roots and the op journal, plus
//! management pages (folders, accounts, settings) that drive `holo.exe` for anything
//! that talks to the cloud or to Windows. Its own process, so closing it frees
//! everything; the tray keeps running.

mod accounts;
mod activity;
mod icons;
mod overview;
mod roots;
mod settings;
mod theme;
mod widgets;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use eframe::egui::{self, Color32, CornerRadius, Margin, RichText, Stroke};
use holo_cli::cmd::Ctx;
use holo_state::{AccountRecord, ActivityRecord, OpRecord};

use crate::{actions, actions::Job, icon, snapshot::Snapshot};
use icons::Icon;

const REFRESH: Duration = Duration::from_secs(1);
const SHOW_EVENT: &str = "Local\\holo-ui-show";

#[derive(PartialEq, Clone, Copy)]
enum Page {
    Overview,
    Activity,
    Roots,
    Accounts,
    Settings,
}

#[derive(Clone, Copy, PartialEq)]
enum Tone {
    Info,
    Ok,
    Error,
}

struct Toast {
    text: String,
    tone: Tone,
    at: Instant,
}

/// What to do with a finished job's result (besides showing it).
type After = Box<dyn FnOnce(&mut App, &Result<String, String>)>;

struct Running {
    job: Job,
    /// Message on success; `None` shows the command's own output.
    ok: Option<String>,
    after: Option<After>,
}

pub(crate) struct App {
    ctx: Ctx,
    egui: egui::Context,
    snap: Option<Snapshot>,
    ops: Vec<OpRecord>,
    accounts: Vec<AccountRecord>,
    activity: Vec<ActivityRecord>,
    error: Option<String>,
    last: Instant,
    page: Page,
    autostart: bool,
    toast: Option<Toast>,
    running: Option<Running>,
    logo: egui::TextureHandle,
    overview: overview::State,
    activity_ui: activity::State,
    roots: roots::State,
    accounts_ui: accounts::State,
    settings: settings::State,
}

/// Open the window, or bring an already open one to the front.
pub fn run(ctx: Ctx) -> anyhow::Result<()> {
    let Some(show) = single_instance() else {
        return Ok(());
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("holo")
            .with_inner_size([1000.0, 680.0])
            .with_min_inner_size([760.0, 460.0])
            .with_icon(Arc::new(icon::window())),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "holo",
        options,
        Box::new(move |cc| {
            show.listen(cc.egui_ctx.clone());
            egui_extras::install_image_loaders(&cc.egui_ctx);
            theme::apply(&cc.egui_ctx);
            let i = icon::window();
            let logo = cc.egui_ctx.load_texture(
                "logo",
                egui::ColorImage::from_rgba_unmultiplied(
                    [i.width as usize, i.height as usize],
                    &i.rgba,
                ),
                egui::TextureOptions::LINEAR,
            );
            let settings = settings::State::new(&ctx.cfg);
            Ok(Box::new(App {
                autostart: actions::autostart_enabled(),
                egui: cc.egui_ctx.clone(),
                ctx,
                snap: None,
                ops: Vec::new(),
                accounts: Vec::new(),
                activity: Vec::new(),
                error: None,
                last: Instant::now() - REFRESH,
                page: Page::Overview,
                toast: None,
                running: None,
                logo,
                overview: Default::default(),
                activity_ui: Default::default(),
                roots: Default::default(),
                accounts_ui: Default::default(),
                settings,
            }))
        }),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}

impl App {
    fn refresh(&mut self) {
        self.last = Instant::now();
        let loaded = Snapshot::load(&self.ctx).and_then(|s| {
            let st = &self.ctx.state;
            Ok((
                s,
                st.ops(None)?,
                st.accounts()?,
                st.recent_activity(None, activity::LOAD)?,
            ))
        });
        match loaded {
            Ok((s, ops, accounts, activity)) => {
                self.snap = Some(s);
                self.ops = ops;
                self.accounts = accounts;
                self.activity = activity;
                self.error = None;
            }
            Err(e) => self.error = Some(format!("{e:#}")),
        }
    }

    fn say(&mut self, tone: Tone, text: impl Into<String>) {
        self.toast = Some(Toast {
            text: text.into(),
            tone,
            at: Instant::now(),
        });
    }

    fn report(&mut self, r: anyhow::Result<usize>, what: &str) {
        match r {
            Ok(n) => self.say(Tone::Ok, format!("{n} {what}.")),
            Err(e) => self.say(Tone::Error, format!("{e:#}")),
        }
        self.refresh();
    }

    fn busy(&self) -> bool {
        self.running.is_some()
    }

    fn notifier(&self) -> impl Fn() + Send + 'static {
        let c = self.egui.clone();
        move || c.request_repaint()
    }

    /// Run `holo <args>` in the background (one job at a time).
    fn start_holo(
        &mut self,
        what: &str,
        args: Vec<String>,
        ok: Option<String>,
        after: Option<After>,
    ) {
        let job = Job::holo(what, args, self.notifier());
        self.running = Some(Running { job, ok, after });
    }

    fn start_func(
        &mut self,
        what: &str,
        f: impl FnOnce() -> anyhow::Result<String> + Send + 'static,
    ) {
        let job = Job::func(what, f, self.notifier());
        self.running = Some(Running {
            job,
            ok: None,
            after: None,
        });
    }

    fn poll_job(&mut self) {
        let Some(result) = self.running.as_ref().and_then(|r| r.job.poll()) else {
            return;
        };
        let Running { ok, after, .. } = self.running.take().expect("checked above");
        let result = result.map_err(|e| format!("{e:#}"));
        match &result {
            Ok(out) => {
                let text = ok.unwrap_or_else(|| out.clone());
                self.say(Tone::Ok, text);
            }
            Err(e) => self.say(Tone::Error, e.clone()),
        }
        if let Some(after) = after {
            after(self, &result);
        }
        self.refresh();
    }

    fn toggle_daemon(&mut self, running: bool) {
        if running {
            self.start_func("Parando o daemon…", actions::stop_daemon);
        } else {
            self.start_func("Iniciando o daemon…", actions::start_daemon);
        }
    }

    /// Stop and start again (config changes are read at startup).
    fn restart_daemon(&mut self) {
        self.start_func("Reiniciando o daemon…", || {
            actions::stop_daemon()?;
            actions::start_daemon()?;
            Ok("Daemon reiniciado com a nova configuração.".into())
        });
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            ui.add(egui::Image::new((self.logo.id(), egui::vec2(30.0, 30.0))));
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(RichText::new("holo").size(19.0).strong().color(theme::TEXT));
                ui.label(
                    RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                        .small()
                        .color(theme::WEAK),
                );
            });
        });
        ui.add_space(22.0);

        let attention = self
            .snap
            .as_ref()
            .map(|s| {
                let t = s.totals();
                t.held + t.failed
            })
            .unwrap_or(0);
        let roots = self.snap.as_ref().map_or(0, |s| s.roots.len());
        let pages = [
            (Page::Overview, Icon::Overview, "Visão geral", attention),
            (Page::Activity, Icon::Sync, "Atividade", 0),
            (Page::Roots, Icon::Folder, "Pastas", roots as u64),
            (
                Page::Accounts,
                Icon::User,
                "Contas",
                self.accounts.len() as u64,
            ),
            (Page::Settings, Icon::Settings, "Configurações", 0),
        ];
        for (page, icon, name, badge) in pages {
            if nav_item(
                ui,
                self.page == page,
                icon,
                name,
                badge,
                page == Page::Overview,
            ) {
                if page == Page::Settings && self.page != Page::Settings {
                    self.settings.sync_from(&self.ctx.cfg);
                }
                self.page = page;
            }
        }

        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            ui.add_space(12.0);
            if ui
                .add(
                    egui::Button::new((
                        Icon::Logs.image(15.0, theme::WEAK),
                        RichText::new("Logs").color(theme::WEAK),
                    ))
                    .frame(false),
                )
                .clicked()
            {
                actions::open(&self.ctx.paths.logs());
            }
        });
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        let Some(s) = &self.snap else { return };
        let status = s.status();
        let (paused, daemon) = (s.paused, s.daemon);
        ui.horizontal(|ui| {
            let [r, g, b] = icon::color(status);
            widgets::pill(ui, Color32::from_rgb(r, g, b).gamma_multiply(1.25), status.label());
            if let Some(job) = &self.running {
                ui.add_space(8.0);
                ui.spinner();
                ui.label(RichText::new(&job.job.what).color(theme::WEAK));
                if job.job.cancellable() && ui.small_button("Cancelar").clicked() {
                    job.job.cancel();
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let busy = self.busy();
                let b = if daemon {
                    widgets::button(Icon::Stop, "Parar daemon")
                } else {
                    widgets::primary_icon(Icon::Play, "Iniciar daemon")
                };
                if ui
                    .add_enabled(!busy, b)
                    .on_hover_text("O daemon é quem sincroniza; sem ele nada é enviado nem baixado.")
                    .clicked()
                {
                    self.toggle_daemon(daemon);
                }
                let b = if paused {
                    widgets::button(Icon::Play, "Retomar")
                } else {
                    widgets::button(Icon::Pause, "Pausar")
                };
                if ui
                    .add(b)
                    .on_hover_text(
                        "Pausa envios e a leitura da nuvem. Abrir arquivos sob demanda continua funcionando.",
                    )
                    .clicked()
                {
                    match actions::set_paused(&self.ctx, !paused) {
                        Ok(()) => self.say(
                            Tone::Info,
                            if paused {
                                "Sincronização retomada."
                            } else {
                                "Sincronização pausada."
                            },
                        ),
                        Err(e) => self.say(Tone::Error, format!("{e:#}")),
                    }
                    self.refresh();
                }
            });
        });
    }

    fn toast_bar(&mut self, ui: &mut egui::Ui) {
        let show = self.error.clone().map(|e| (e, Tone::Error)).or_else(|| {
            self.toast
                .as_ref()
                .filter(|t| {
                    let ttl = if t.tone == Tone::Error { 20 } else { 8 };
                    t.at.elapsed() < Duration::from_secs(ttl)
                })
                .map(|t| (t.text.clone(), t.tone))
        });
        let Some((text, tone)) = show else { return };
        let color = match tone {
            Tone::Info => theme::CYAN,
            Tone::Ok => theme::MINT,
            Tone::Error => theme::DANGER,
        };
        egui::Panel::bottom("toast")
            .frame(
                egui::Frame::new()
                    .fill(theme::BG)
                    .inner_margin(Margin::symmetric(16, 8)),
            )
            .show(ui, |ui| {
                widgets::callout(ui, color, |ui| {
                    ui.horizontal(|ui| {
                        let i = match tone {
                            Tone::Info => Icon::Info,
                            Tone::Ok => Icon::CheckCircle,
                            Tone::Error => Icon::Danger,
                        };
                        i.show(ui, 16.0, color);
                        ui.add(egui::Label::new(RichText::new(&text).color(theme::TEXT)).wrap());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if widgets::icon_button(ui, Icon::X, "Fechar") {
                                self.toast = None;
                            }
                        });
                    });
                });
            });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_job();
        if self.last.elapsed() >= REFRESH {
            self.refresh();
        }
        ui.ctx().request_repaint_after(REFRESH);

        egui::Panel::left("nav")
            .exact_size(200.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(theme::SIDEBAR)
                    .inner_margin(Margin::symmetric(10, 0))
                    .stroke(Stroke::new(1.0, theme::BORDER)),
            )
            .show(ui, |ui| self.sidebar(ui));

        egui::Panel::top("header")
            .frame(egui::Frame::new().fill(theme::BG).inner_margin(Margin {
                left: 22,
                right: 22,
                top: 14,
                bottom: 10,
            }))
            .show(ui, |ui| self.header(ui));

        self.toast_bar(ui);

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BG))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        egui::Frame::new()
                            .inner_margin(Margin {
                                left: 22,
                                right: 22,
                                top: 6,
                                bottom: 22,
                            })
                            .show(ui, |ui| {
                                if self.snap.is_none() {
                                    ui.spinner();
                                    return;
                                }
                                match self.page {
                                    Page::Overview => overview::page(self, ui),
                                    Page::Activity => activity::page(self, ui),
                                    Page::Roots => roots::page(self, ui),
                                    Page::Accounts => accounts::page(self, ui),
                                    Page::Settings => settings::page(self, ui),
                                }
                            });
                    });
            });

        overview::modals(self, ui.ctx());
        roots::modals(self, ui.ctx());
        accounts::modals(self, ui.ctx());
    }
}

/// Sidebar entry; returns whether it was clicked.
fn nav_item(
    ui: &mut egui::Ui,
    selected: bool,
    icon: Icon,
    name: &str,
    badge: u64,
    badge_alert: bool,
) -> bool {
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 36.0), egui::Sense::click());
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, CornerRadius::same(theme::RADIUS), theme::CARD_HI);
        let bar = egui::Rect::from_min_size(
            rect.left_top() + egui::vec2(0.0, 8.0),
            egui::vec2(3.0, 20.0),
        );
        p.rect_filled(bar, CornerRadius::same(2), theme::CYAN);
    } else if resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(theme::RADIUS), theme::CARD);
    }
    let color = if selected { theme::TEXT } else { theme::WEAK };
    let ir = egui::Rect::from_center_size(
        rect.left_center() + egui::vec2(22.0, 0.0),
        egui::vec2(17.0, 17.0),
    );
    icon.paint(ui, ir, if selected { theme::CYAN } else { color });
    p.text(
        rect.left_center() + egui::vec2(40.0, 0.0),
        egui::Align2::LEFT_CENTER,
        name,
        egui::FontId::proportional(14.5),
        color,
    );
    if badge > 0 {
        let c = if badge_alert {
            theme::WARN
        } else {
            theme::WEAK
        };
        let center = rect.right_center() - egui::vec2(20.0, 0.0);
        let r = egui::Rect::from_center_size(center, egui::vec2(26.0, 18.0));
        p.rect_filled(r, CornerRadius::same(9), c.linear_multiply(0.18));
        p.text(
            center,
            egui::Align2::CENTER_CENTER,
            badge.to_string(),
            egui::FontId::proportional(11.5),
            c,
        );
    }
    ui.add_space(2.0);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

// --------------------------------------------------------- single instance

/// Named event the window waits on; set by a second launch to bring it forward.
struct ShowSignal {
    #[cfg(windows)]
    raw: usize,
}

/// `None` if a window is already open (it was asked to come to the front).
fn single_instance() -> Option<ShowSignal> {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
        use windows::Win32::System::Threading::{CreateEventW, SetEvent};
        use windows::core::HSTRING;
        // SAFETY: auto-reset event; kept open for the life of the process.
        unsafe {
            let h = CreateEventW(None, false, false, &HSTRING::from(SHOW_EVENT)).ok()?;
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = SetEvent(h);
                return None;
            }
            Some(ShowSignal { raw: h.0 as usize })
        }
    }
    #[cfg(not(windows))]
    Some(ShowSignal {})
}

impl ShowSignal {
    fn listen(self, ctx: egui::Context) {
        #[cfg(windows)]
        std::thread::spawn(move || {
            use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
            use windows::Win32::System::Threading::{INFINITE, WaitForSingleObject};
            let h = HANDLE(self.raw as *mut _);
            // SAFETY: the handle is never closed while this thread runs.
            while unsafe { WaitForSingleObject(h, INFINITE) } == WAIT_OBJECT_0 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                ctx.request_repaint();
            }
        });
        #[cfg(not(windows))]
        let _ = ctx;
    }
}
