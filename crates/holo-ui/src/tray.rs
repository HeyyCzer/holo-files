//! Tray icon process: always running, tiny. Polls the state store, keeps the icon,
//! tooltip and menu current, and opens the status window as a separate process.

use std::{
    path::PathBuf,
    process::Child,
    sync::mpsc,
    time::{Duration, Instant},
};

use holo_cli::cmd::Ctx;
use tray_icon::{
    MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu},
};

use crate::{
    actions, icon,
    snapshot::{Snapshot, Status},
};

const REFRESH: Duration = Duration::from_millis(1500);

struct Items {
    open: MenuItem,
    pause: MenuItem,
    daemon: MenuItem,
    folders: Submenu,
    logs: MenuItem,
    quit: MenuItem,
}

pub fn run(ctx: Ctx) -> anyhow::Result<()> {
    let items = Items {
        open: MenuItem::new("Abrir holo", true, None),
        pause: MenuItem::new("Pausar sincronização", true, None),
        daemon: MenuItem::new("Parar daemon", true, None),
        folders: Submenu::new("Abrir pasta", true),
        logs: MenuItem::new("Abrir logs", true, None),
        quit: MenuItem::new("Sair do ícone", true, None),
    };
    let menu = Menu::new();
    menu.append_items(&[
        &items.open,
        &PredefinedMenuItem::separator(),
        &items.folders,
        &items.pause,
        &items.daemon,
        &items.logs,
        &PredefinedMenuItem::separator(),
        &items.quit,
    ])?;

    let mut status = Status::Stopped;
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip("holo")
        .with_icon(icon::tray(status))
        .build()?;

    let mut window: Option<Child> = None;
    let mut folders: Vec<(MenuItem, PathBuf)> = Vec::new();
    let mut tooltip = String::new();
    let mut last = Instant::now() - REFRESH;
    let mut snap: Option<Snapshot> = None;
    // Results of start/stop, which run in the background.
    let (done_tx, done_rx) = mpsc::channel::<String>();
    let mut busy = false;

    loop {
        pump_messages();

        while let Ok(ev) = TrayIconEvent::receiver().try_recv() {
            match ev {
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
                | TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                } => open_window(&mut window),
                _ => {}
            }
        }

        while let Ok(ev) = MenuEvent::receiver().try_recv() {
            let id = ev.id();
            if id == items.open.id() {
                open_window(&mut window);
            } else if id == items.pause.id() {
                if let Some(s) = &snap {
                    let _ = actions::set_paused(&ctx, !s.paused);
                    last = Instant::now() - REFRESH;
                }
            } else if id == items.daemon.id() && !busy {
                let running = snap.as_ref().is_some_and(|s| s.daemon);
                busy = true;
                items.daemon.set_enabled(false);
                let tx = done_tx.clone();
                actions::background(
                    move || {
                        if running {
                            actions::stop_daemon()
                        } else {
                            actions::start_daemon()
                        }
                    },
                    move |m| {
                        let _ = tx.send(m);
                    },
                );
            } else if id == items.logs.id() {
                actions::open(&ctx.paths.logs());
            } else if id == items.quit.id() {
                if let Some(mut c) = window.take() {
                    let _ = c.kill();
                }
                return Ok(());
            } else if let Some((_, path)) = folders.iter().find(|(i, _)| i.id() == id) {
                actions::open(path);
            }
        }

        if done_rx.try_recv().is_ok() {
            busy = false;
            items.daemon.set_enabled(true);
            last = Instant::now() - REFRESH;
        }

        if last.elapsed() >= REFRESH {
            last = Instant::now();
            if let Ok(s) = Snapshot::load(&ctx) {
                let now = s.status();
                if now != status {
                    status = now;
                    let _ = tray.set_icon(Some(icon::tray(status)));
                }
                let tip = s.tooltip();
                if tip != tooltip {
                    let _ = tray.set_tooltip(Some(&tip));
                    tooltip = tip;
                }
                items.pause.set_text(if s.paused {
                    "Retomar sincronização"
                } else {
                    "Pausar sincronização"
                });
                if !busy {
                    items.daemon.set_text(if s.daemon {
                        "Parar daemon"
                    } else {
                        "Iniciar daemon"
                    });
                }
                let paths: Vec<PathBuf> = s
                    .roots
                    .iter()
                    .map(|r| PathBuf::from(&r.root.local_path))
                    .collect();
                if paths.iter().ne(folders.iter().map(|(_, p)| p)) {
                    for (i, _) in folders.drain(..) {
                        let _ = items.folders.remove(&i);
                    }
                    for p in paths {
                        let i = MenuItem::new(p.display().to_string(), true, None);
                        let _ = items.folders.append(&i);
                        folders.push((i, p));
                    }
                }
                items.folders.set_enabled(!folders.is_empty());
                snap = Some(s);
            }
        }

        wait_for_input(Duration::from_millis(250));
    }
}

/// Start the window process. If one is already open, the new one just brings it to
/// the front and exits (see `window::single_instance`).
fn open_window(window: &mut Option<Child>) {
    let spawned = std::env::current_exe()
        .ok()
        .and_then(|exe| std::process::Command::new(exe).arg("--window").spawn().ok());
    let alive = window
        .as_mut()
        .is_some_and(|c| matches!(c.try_wait(), Ok(None)));
    if !alive {
        *window = spawned;
    }
}

/// Dispatch pending window messages (the tray icon's hidden window lives on this thread).
fn pump_messages() {
    #[cfg(windows)]
    {
        use windows::Win32::UI::WindowsAndMessaging::{
            DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
        };
        let mut msg = MSG::default();
        // SAFETY: standard message pump on the thread that owns the tray window.
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

/// Sleep until a window message arrives or `timeout` passes.
fn wait_for_input(timeout: Duration) {
    #[cfg(windows)]
    {
        use windows::Win32::UI::WindowsAndMessaging::{MsgWaitForMultipleObjects, QS_ALLINPUT};
        // SAFETY: no handles; only waits on this thread's message queue.
        unsafe {
            MsgWaitForMultipleObjects(None, false, timeout.as_millis() as u32, QS_ALLINPUT);
        }
    }
    #[cfg(not(windows))]
    std::thread::sleep(timeout);
}
