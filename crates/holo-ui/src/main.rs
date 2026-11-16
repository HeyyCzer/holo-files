//! `holo-ui`: tray icon (default) and status window (`--window`) for holo.
#![cfg_attr(not(test), windows_subsystem = "windows")]

mod actions;
#[cfg(test)]
mod ico;
mod icon;
mod snapshot;
mod tray;
mod window;

use holo_cli::{
    cmd::Ctx,
    config::{Config, Paths},
};

fn ctx() -> anyhow::Result<Ctx> {
    let paths = Paths::resolve()?;
    let cfg = Config::load(&paths)?;
    Ctx::new(cfg, paths)
}

/// Only one tray per user: holds `ui.lock` exclusively while running.
fn tray_lock(ctx: &Ctx) -> Option<std::fs::File> {
    std::fs::create_dir_all(&ctx.paths.data).ok()?;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(false);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.share_mode(0);
    }
    o.open(ctx.paths.data.join("ui.lock")).ok()
}

/// `--tray`: only the tray icon (logon autostart). `--window`: only the window (what
/// the tray opens). No argument (Start menu): the window, starting the tray if needed.
fn main() {
    let arg = |name: &str| std::env::args().any(|a| a == name);
    let result = ctx().and_then(|ctx| {
        if arg("--window") {
            return window::run(ctx);
        }
        if arg("--tray") {
            return match tray_lock(&ctx) {
                Some(_lock) => tray::run(ctx),
                None => Ok(()), // already running
            };
        }
        if tray_lock(&ctx).is_some()
            && let Ok(exe) = std::env::current_exe()
        {
            // The lock is released when this temporary drops, before the tray takes it.
            let _ = std::process::Command::new(exe).arg("--tray").spawn();
        }
        window::run(ctx)
    });
    if let Err(e) = result {
        fatal(&format!("{e:#}"));
    }
}

/// No console in a GUI-subsystem app: report startup errors in a message box.
fn fatal(msg: &str) {
    #[cfg(windows)]
    {
        use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
        use windows::core::HSTRING;
        // SAFETY: modal message box with owned strings.
        unsafe {
            MessageBoxW(
                None,
                &HSTRING::from(msg),
                &HSTRING::from("holo"),
                MB_OK | MB_ICONERROR,
            );
        }
    }
    #[cfg(not(windows))]
    eprintln!("{msg}");
}
