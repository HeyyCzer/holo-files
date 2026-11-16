//! What the buttons and menu items do. Daemon control goes through `holo.exe` (next to
//! this executable) so there is one implementation of start/stop; journal and pause
//! changes are written to the state store, which the daemon reads.

use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::Context as _;
use holo_cli::cmd::Ctx;
use holo_state::OpState;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

pub(crate) fn hidden(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    }
    cmd
}

fn sibling(name: &str) -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let p = exe.with_file_name(name);
    anyhow::ensure!(p.exists(), "{} not found next to {}", name, exe.display());
    Ok(p)
}

/// Run `holo <args>` without a console window; its output (or error) as text.
fn holo(args: &[&str]) -> anyhow::Result<String> {
    let out = hidden(Command::new(sibling("holo.exe")?).args(args))
        .stdin(Stdio::null())
        .output()
        .context("running holo.exe")?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
    .trim()
    .to_string();
    anyhow::ensure!(out.status.success(), "{text}");
    Ok(text)
}

/// Start the daemon: through the logon task if installed, else as a hidden process.
pub fn start_daemon() -> anyhow::Result<String> {
    if holo_cli::cmd::service::task_installed() {
        return holo(&["service", "start"]);
    }
    hidden(Command::new(sibling("holo.exe")?).arg("run"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("starting holo run")?;
    Ok("Daemon iniciado.".into())
}

pub fn stop_daemon() -> anyhow::Result<String> {
    holo(&["service", "stop"])
}

/// Run a slow action off the UI thread; `done` gets its message.
pub fn background(
    f: impl FnOnce() -> anyhow::Result<String> + Send + 'static,
    done: impl FnOnce(String) + Send + 'static,
) {
    std::thread::spawn(move || {
        done(match f() {
            Ok(m) => m,
            Err(e) => format!("Erro: {e:#}"),
        })
    });
}

pub fn open(path: &Path) {
    let _ = Command::new("explorer.exe").arg(path).spawn();
}

pub fn set_paused(ctx: &Ctx, paused: bool) -> anyhow::Result<()> {
    Ok(holo_engine::context::set_paused(&ctx.state, paused)?)
}

pub fn release_held(ctx: &Ctx) -> anyhow::Result<usize> {
    Ok(ctx.state.release_held_ops(None)?)
}

pub fn discard_held(ctx: &Ctx) -> anyhow::Result<usize> {
    Ok(ctx.state.discard_held_ops(None)?)
}

pub fn retry_failed(ctx: &Ctx) -> anyhow::Result<usize> {
    let mut n = 0;
    for o in ctx.state.ops(None)? {
        if o.state == OpState::Failed {
            ctx.state.set_op_state(o.id, OpState::Pending, None)?;
            n += 1;
        }
    }
    Ok(n)
}

// ------------------------------------------------------------- autostart

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "holo-ui";

fn reg(args: &[&str]) -> bool {
    hidden(Command::new("reg").args(args))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Whether the tray starts at logon (HKCU Run key).
pub fn autostart_enabled() -> bool {
    reg(&["query", RUN_KEY, "/v", RUN_VALUE])
}

pub fn set_autostart(on: bool) -> anyhow::Result<()> {
    let ok = if on {
        let exe = std::env::current_exe()?;
        let value = format!("\"{}\" --tray", exe.display());
        reg(&[
            "add", RUN_KEY, "/v", RUN_VALUE, "/t", "REG_SZ", "/d", &value, "/f",
        ])
    } else {
        reg(&["delete", RUN_KEY, "/v", RUN_VALUE, "/f"])
    };
    anyhow::ensure!(ok, "não consegui alterar {RUN_KEY}");
    Ok(())
}
