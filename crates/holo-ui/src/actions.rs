//! What the buttons and menu items do. Daemon control goes through `holo.exe` (next to
//! this executable) so there is one implementation of start/stop; journal and pause
//! changes are written to the state store, which the daemon reads.

use std::{
    io::Read as _,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
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

/// A slow action running off the UI thread (a `holo.exe` subcommand or a closure).
/// `holo` jobs can be cancelled, which kills the process (e.g. a login left waiting
/// for the browser).
pub struct Job {
    /// What the UI says while it runs.
    pub what: String,
    rx: mpsc::Receiver<anyhow::Result<String>>,
    child: Option<Arc<Mutex<Child>>>,
    cancelled: Arc<AtomicBool>,
}

impl Job {
    /// Run `holo <args>` hidden; `notify` is called when it finishes.
    pub fn holo(
        what: impl Into<String>,
        args: Vec<String>,
        notify: impl Fn() + Send + 'static,
    ) -> Self {
        let what = what.into();
        let (tx, rx) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let spawned = sibling("holo.exe").and_then(|exe| {
            hidden(Command::new(exe).args(&args))
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .context("running holo.exe")
        });
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(Err(e));
                notify();
                return Self {
                    what,
                    rx,
                    child: None,
                    cancelled,
                };
            }
        };
        let (mut out, mut err) = (child.stdout.take(), child.stderr.take());
        let child = Arc::new(Mutex::new(child));
        let waiter = child.clone();
        let was_cancelled = cancelled.clone();
        std::thread::spawn(move || {
            let errs = std::thread::spawn(move || {
                let mut s = String::new();
                if let Some(e) = err.as_mut() {
                    let _ = e.read_to_string(&mut s);
                }
                s
            });
            let mut text = String::new();
            if let Some(o) = out.as_mut() {
                let _ = o.read_to_string(&mut text);
            }
            text.push_str(&errs.join().unwrap_or_default());
            let text = text.trim().to_string();
            // The pipes closed: the process has exited (or was killed), so this is quick.
            let status = waiter.lock().unwrap_or_else(|p| p.into_inner()).wait();
            let _ = tx.send(match status {
                _ if was_cancelled.load(Ordering::Relaxed) => Err(anyhow::anyhow!("Cancelado.")),
                Ok(s) if s.success() => Ok(text),
                Ok(_) if text.is_empty() => Err(anyhow::anyhow!("holo terminou com erro")),
                Ok(_) => Err(anyhow::anyhow!("{text}")),
                Err(e) => Err(e.into()),
            });
            notify();
        });
        Self {
            what,
            rx,
            child: Some(child),
            cancelled,
        }
    }

    /// Run `f` on a thread; `notify` is called when it finishes.
    pub fn func(
        what: impl Into<String>,
        f: impl FnOnce() -> anyhow::Result<String> + Send + 'static,
        notify: impl Fn() + Send + 'static,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
            notify();
        });
        Self {
            what: what.into(),
            rx,
            child: None,
            cancelled: Default::default(),
        }
    }

    /// The result, once it has finished.
    pub fn poll(&self) -> Option<anyhow::Result<String>> {
        match self.rx.try_recv() {
            Ok(r) => Some(r),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err(anyhow::anyhow!("tarefa interrompida")))
            }
        }
    }

    pub fn cancellable(&self) -> bool {
        self.child.is_some()
    }

    pub fn cancel(&self) {
        if let Some(c) = &self.child {
            self.cancelled.store(true, Ordering::Relaxed);
            let _ = c.lock().unwrap_or_else(|p| p.into_inner()).kill();
        }
    }
}

pub fn open(path: &Path) {
    let _ = Command::new("explorer.exe").arg(path).spawn();
}

/// Show a file selected in Explorer (or its folder if it no longer exists).
pub fn reveal(path: &Path) {
    if path.exists() {
        let mut cmd = Command::new("explorer.exe");
        // Explorer wants `/select,"path"`, which Rust's argument quoting can't produce.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.raw_arg(format!("/select,\"{}\"", path.display()));
        }
        let _ = cmd.spawn();
    } else if let Some(dir) = path.parent() {
        open(dir);
    }
}

/// Open a web page in the default browser.
pub fn open_url(url: &str) {
    let _ = hidden(Command::new("rundll32").args(["url.dll,FileProtocolHandler", url])).spawn();
}

/// Native "choose folder" dialog (blocks this thread while open).
pub fn pick_folder(start: &str) -> Option<PathBuf> {
    let mut d = rfd::FileDialog::new().set_title("Escolha a pasta local");
    let start = Path::new(start);
    if start.is_dir() {
        d = d.set_directory(start);
    }
    d.pick_folder()
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
