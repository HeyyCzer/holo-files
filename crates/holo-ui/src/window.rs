//! Status window (`holo-ui --window`): roots, what's running, the op journal, and the
//! controls the CLI has (pause, start/stop, release/discard/retry). Its own process,
//! so closing it frees everything; the tray keeps running.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use eframe::egui::{self, Color32, RichText};
use holo_cli::cmd::Ctx;
use holo_state::{OpRecord, OpState};

use crate::{
    actions, icon,
    snapshot::{RootView, Snapshot, op_target},
};

const REFRESH: Duration = Duration::from_secs(1);
const MAX_ROWS: usize = 500;
const SHOW_EVENT: &str = "Local\\holo-ui-show";

#[derive(PartialEq, Clone, Copy)]
enum Filter {
    All,
    Pending,
    Held,
    Failed,
}

struct App {
    ctx: Ctx,
    snap: Option<Snapshot>,
    ops: Vec<OpRecord>,
    error: Option<String>,
    last: Instant,
    filter: Filter,
    autostart: bool,
    /// Message from the last action, and when it was set.
    message: Arc<Mutex<Option<(String, Instant)>>>,
    busy: Arc<Mutex<bool>>,
}

/// Open the window, or bring an already open one to the front.
pub fn run(ctx: Ctx) -> anyhow::Result<()> {
    let Some(show) = single_instance() else {
        return Ok(());
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("holo")
            .with_inner_size([860.0, 620.0])
            .with_min_inner_size([560.0, 360.0])
            .with_icon(Arc::new(icon::window())),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "holo",
        options,
        Box::new(move |cc| {
            show.listen(cc.egui_ctx.clone());
            Ok(Box::new(App {
                autostart: actions::autostart_enabled(),
                ctx,
                snap: None,
                ops: Vec::new(),
                error: None,
                last: Instant::now() - REFRESH,
                filter: Filter::All,
                message: Default::default(),
                busy: Default::default(),
            }))
        }),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}

impl App {
    fn refresh(&mut self) {
        self.last = Instant::now();
        match Snapshot::load(&self.ctx).and_then(|s| Ok((s, self.ctx.state.ops(None)?))) {
            Ok((s, ops)) => {
                self.snap = Some(s);
                self.ops = ops;
                self.error = None;
            }
            Err(e) => self.error = Some(format!("{e:#}")),
        }
    }

    fn say(&self, m: impl Into<String>) {
        *self.message.lock().unwrap_or_else(|p| p.into_inner()) = Some((m.into(), Instant::now()));
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        let Some(s) = &self.snap else { return };
        let status = s.status();
        let (paused, daemon) = (s.paused, s.daemon);
        ui.horizontal(|ui| {
            let [r, g, b] = icon::color(status);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 7.0, Color32::from_rgb(r, g, b));
            ui.label(RichText::new(status.label()).size(18.0).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let busy = *self.busy.lock().unwrap_or_else(|p| p.into_inner());
                let text = if daemon { "Parar daemon" } else { "Iniciar daemon" };
                if ui.add_enabled(!busy, egui::Button::new(text)).clicked() {
                    *self.busy.lock().unwrap_or_else(|p| p.into_inner()) = true;
                    let (msg, flag, ctx) = (self.message.clone(), self.busy.clone(), ui.ctx().clone());
                    actions::background(
                        move || if daemon { actions::stop_daemon() } else { actions::start_daemon() },
                        move |m| {
                            *msg.lock().unwrap_or_else(|p| p.into_inner()) = Some((m, Instant::now()));
                            *flag.lock().unwrap_or_else(|p| p.into_inner()) = false;
                            ctx.request_repaint();
                        },
                    );
                }
                let text = if paused { "Retomar" } else { "Pausar" };
                if ui.button(text).on_hover_text("Pausa envios e a leitura da nuvem. Abrir arquivos sob demanda continua funcionando.").clicked() {
                    match actions::set_paused(&self.ctx, !paused) {
                        Ok(()) => self.say(if paused { "Sincronização retomada." } else { "Sincronização pausada." }),
                        Err(e) => self.say(format!("Erro: {e:#}")),
                    }
                    self.refresh();
                }
                if ui.button("Logs").clicked() {
                    actions::open(&self.ctx.paths.logs());
                }
                let mut auto = self.autostart;
                if ui.checkbox(&mut auto, "Iniciar com o Windows").on_hover_text("Ícone na bandeja ao entrar no Windows").changed() {
                    match actions::set_autostart(auto) {
                        Ok(()) => self.autostart = auto,
                        Err(e) => self.say(format!("Erro: {e:#}")),
                    }
                }
            });
        });
    }

    fn root(ui: &mut egui::Ui, r: &RootView, daemon: bool) {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(&r.root.local_path).strong());
                ui.label(RichText::new(format!("·  nuvem: {}", r.root.remote_path)).weak());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Abrir pasta").clicked() {
                        actions::open(std::path::Path::new(&r.root.local_path));
                    }
                });
            });
            ui.horizontal(|ui| {
                stat(ui, "itens", r.items, None);
                stat(ui, "na fila", r.counts.pending, None);
                stat(
                    ui,
                    "retidas",
                    r.counts.held,
                    (r.counts.held > 0).then_some(Color32::from_rgb(0xd9, 0x8c, 0x2b)),
                );
                stat(
                    ui,
                    "falhas",
                    r.counts.failed,
                    (r.counts.failed > 0).then_some(Color32::from_rgb(0xd9, 0x4a, 0x2b)),
                );
                if r.root.cursor.is_none() {
                    ui.label(RichText::new("sincronização inicial pendente").italics());
                }
            });
            if !daemon {
                return;
            }
            if let Some(p) = &r.pull {
                ui.label(p);
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
                .weak(),
            );
            if !pr.running.is_empty() {
                let now = chrono::Utc::now().timestamp_millis();
                egui::Grid::new(("running", &r.root.id))
                    .striped(true)
                    .show(ui, |ui| {
                        for op in &pr.running {
                            ui.label(RichText::new(&op.kind).monospace());
                            ui.label(fmt_age(Duration::from_millis(
                                (now - op.started_ms).max(0) as u64
                            )));
                            ui.label(&op.path);
                            ui.end_row();
                        }
                    });
            }
        });
    }

    fn journal(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Fila de operações");
            for (f, name) in [
                (Filter::All, "todas"),
                (Filter::Pending, "pendentes"),
                (Filter::Held, "retidas"),
                (Filter::Failed, "falhas"),
            ] {
                ui.selectable_value(&mut self.filter, f, name);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let t = self.snap.as_ref().map(|s| s.totals()).unwrap_or_default();
                if ui
                    .add_enabled(t.failed > 0, egui::Button::new("Repetir falhas"))
                    .clicked()
                {
                    let r = actions::retry_failed(&self.ctx);
                    self.report(r, "operação(ões) recolocadas na fila");
                }
                if ui
                    .add_enabled(t.held > 0, egui::Button::new("Descartar retidas"))
                    .on_hover_text(
                        "Não executa as exclusões retidas pela proteção de exclusão em massa",
                    )
                    .clicked()
                {
                    let r = actions::discard_held(&self.ctx);
                    self.report(r, "operação(ões) retidas descartadas");
                }
                if ui
                    .add_enabled(t.held > 0, egui::Button::new("Liberar retidas"))
                    .on_hover_text(
                        "Confirma e executa as operações retidas (ex.: exclusões em massa)",
                    )
                    .clicked()
                {
                    let r = actions::release_held(&self.ctx);
                    self.report(r, "operação(ões) liberadas");
                }
            });
        });

        let filter = self.filter;
        let rows: Vec<&OpRecord> = self
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
        if rows.is_empty() {
            ui.label(RichText::new("Nada aqui.").weak());
            return;
        }
        let now = chrono::Utc::now();
        egui::Grid::new("ops")
            .striped(true)
            .num_columns(5)
            .show(ui, |ui| {
                for h in ["#", "estado", "tipo", "caminho", "detalhe"] {
                    ui.label(RichText::new(h).strong());
                }
                ui.end_row();
                for o in rows {
                    ui.label(o.id.to_string());
                    let (state, color) = match o.state {
                        OpState::Pending => ("pendente", None),
                        OpState::Held => ("retida", Some(Color32::from_rgb(0xd9, 0x8c, 0x2b))),
                        OpState::Failed => ("falha", Some(Color32::from_rgb(0xd9, 0x4a, 0x2b))),
                    };
                    let mut t = RichText::new(state);
                    if let Some(c) = color {
                        t = t.color(c);
                    }
                    ui.label(t);
                    ui.label(RichText::new(&o.kind).monospace());
                    ui.label(op_target(o));
                    let mut detail = String::new();
                    if o.state == OpState::Pending && o.not_before > now {
                        let secs = (o.not_before - now).num_seconds().max(0) as u64;
                        detail.push_str(&format!("em {} · ", fmt_age(Duration::from_secs(secs))));
                    }
                    if o.attempts > 0 {
                        detail.push_str(&format!("{}× · ", o.attempts));
                    }
                    if let Some(e) = &o.last_error {
                        detail.push_str(e);
                    }
                    ui.label(RichText::new(detail.trim_end_matches(" · ")).weak());
                    ui.end_row();
                }
            });
        if self.ops.len() > MAX_ROWS {
            ui.label(RichText::new(format!("… mostrando {MAX_ROWS} de {}", self.ops.len())).weak());
        }
    }

    fn report(&mut self, r: anyhow::Result<usize>, what: &str) {
        match r {
            Ok(n) => self.say(format!("{n} {what}.")),
            Err(e) => self.say(format!("Erro: {e:#}")),
        }
        self.refresh();
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.last.elapsed() >= REFRESH {
            self.refresh();
        }
        ui.ctx().request_repaint_after(REFRESH);

        egui::Panel::top("header").show(ui, |ui| {
            ui.add_space(6.0);
            self.header(ui);
            ui.add_space(6.0);
        });

        let message = self
            .message
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .filter(|(_, at)| at.elapsed() < Duration::from_secs(8));
        if message.is_some() || self.error.is_some() {
            egui::Panel::bottom("message").show(ui, |ui| {
                if let Some(e) = &self.error {
                    ui.colored_label(Color32::from_rgb(0xd9, 0x4a, 0x2b), e);
                }
                if let Some((m, _)) = message {
                    ui.label(m);
                }
            });
        }

        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                let Some(s) = &self.snap else {
                    ui.label("Carregando…");
                    return;
                };
                if s.paused {
                    ui.label(RichText::new("Sincronização pausada: nada é enviado nem baixado, mas arquivos sob demanda continuam abrindo.").italics());
                }
                if s.roots.is_empty() {
                    ui.label("Nenhuma pasta sincronizada. Use `holo add-root` para adicionar.");
                }
                for r in &s.roots {
                    Self::root(ui, r, s.daemon);
                    ui.add_space(4.0);
                }
                ui.add_space(8.0);
                self.journal(ui);
            });
        });
    }
}

fn stat(ui: &mut egui::Ui, name: &str, n: u64, color: Option<Color32>) {
    let mut t = RichText::new(n.to_string()).strong();
    if let Some(c) = color {
        t = t.color(c);
    }
    ui.label(t);
    ui.label(RichText::new(name).weak());
    ui.add_space(8.0);
}

fn fmt_age(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m{:02}s", s / 60, s % 60),
        _ => format!("{}h{:02}m", s / 3600, s / 60 % 60),
    }
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
