//! `holo service`: run the daemon in the background at logon.
//!
//! Not a real Windows service on purpose: a sync provider has to live in the user's
//! session (Cloud Filter callbacks, Explorer integration) and the tokens are
//! DPAPI-encrypted per user, which LocalSystem/S4U can't decrypt. So, like OneDrive,
//! it's a per-user scheduled task started at logon, without a console window
//! (`conhost --headless`), restarted on failure. `stop` asks the daemon to shut down
//! cleanly through a named event instead of killing it.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};

use super::{Ctx, daemon_running};
use crate::ServiceAction;

const TASK_NAME: &str = "HoloFiles";
/// Session-local, so each logged-on user has their own.
pub const STOP_EVENT: &str = "Local\\holo-files-stop";

pub async fn service(ctx: &Ctx, action: ServiceAction) -> anyhow::Result<()> {
    match action {
        ServiceAction::Install { no_start } => install(ctx, !no_start).await,
        ServiceAction::Uninstall => uninstall(ctx).await,
        ServiceAction::Start => start(),
        ServiceAction::Stop => stop(ctx).await,
        ServiceAction::Status => status(ctx),
    }
}

async fn install(ctx: &Ctx, start_now: bool) -> anyhow::Result<()> {
    if ctx.state.roots()?.is_empty() {
        bail!("no sync roots; add one with `holo add-root` first");
    }
    let exe = std::env::current_exe()
        .context("locating holo.exe")?
        .canonicalize()?;
    let exe = strip_verbatim(&exe);
    let lower = exe.to_lowercase();
    if lower.contains("\\target\\debug\\") || lower.contains("\\target\\release\\") {
        println!(
            "! the task will run {exe}\n  `cargo build` can't overwrite it while the daemon runs; consider copying holo.exe somewhere stable and installing from there."
        );
    }
    let user = current_user()?;
    let xml = task_xml(&user, &exe);

    // schtasks wants the XML in UTF-16.
    let file = std::env::temp_dir().join(format!("holo-task-{}.xml", std::process::id()));
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(xml.encode_utf16().flat_map(u16::to_le_bytes));
    std::fs::write(&file, bytes)?;
    let r = schtasks(&[
        "/Create",
        "/TN",
        TASK_NAME,
        "/XML",
        &file.to_string_lossy(),
        "/F",
    ]);
    let _ = std::fs::remove_file(&file);
    r.context("creating the scheduled task")?;

    println!("Installed task '{TASK_NAME}': runs `holo run` at logon for {user}.");
    println!("   exe  : {exe}");
    println!("   logs : {}", ctx.paths.logs().display());
    if start_now {
        if daemon_running(ctx) {
            println!("A daemon is already running; it'll be the task from the next logon on.");
        } else {
            start()?;
        }
    }
    Ok(())
}

async fn uninstall(ctx: &Ctx) -> anyhow::Result<()> {
    if !task_installed() {
        println!("Task '{TASK_NAME}' is not installed.");
        return Ok(());
    }
    if daemon_running(ctx) {
        stop(ctx).await?;
    }
    schtasks(&["/Delete", "/TN", TASK_NAME, "/F"]).context("deleting the scheduled task")?;
    println!("Removed task '{TASK_NAME}'.");
    Ok(())
}

fn start() -> anyhow::Result<()> {
    if !task_installed() {
        bail!("task '{TASK_NAME}' is not installed; run `holo service install`");
    }
    schtasks(&["/Run", "/TN", TASK_NAME]).context("starting the scheduled task")?;
    println!("Started. Follow it with `holo status --stream`.");
    Ok(())
}

async fn stop(ctx: &Ctx) -> anyhow::Result<()> {
    if !daemon_running(ctx) {
        println!("holo is not running.");
        return Ok(());
    }
    if signal_stop()? {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if !daemon_running(ctx) {
                println!("Stopped.");
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        println!("! daemon didn't stop within 30 s");
    }
    if task_installed() {
        println!("Ending the task forcibly.");
        schtasks(&["/End", "/TN", TASK_NAME]).context("ending the scheduled task")?;
        return Ok(());
    }
    bail!("couldn't stop the running daemon (started by hand? use Ctrl+C in its console)")
}

fn status(ctx: &Ctx) -> anyhow::Result<()> {
    println!(
        "task   : {}",
        if task_installed() {
            "installed (starts at logon)"
        } else {
            "not installed"
        }
    );
    println!(
        "daemon : {}",
        if daemon_running(ctx) {
            "running"
        } else {
            "stopped"
        }
    );
    println!("logs   : {}", ctx.paths.logs().display());
    Ok(())
}

fn task_installed() -> bool {
    Command::new("schtasks")
        .args(["/Query", "/TN", TASK_NAME])
        .output()
        .is_ok_and(|o| o.status.success())
}

fn schtasks(args: &[&str]) -> anyhow::Result<()> {
    let out = Command::new("schtasks")
        .args(args)
        .output()
        .context("running schtasks.exe")?;
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr);
        let msg = if msg.trim().is_empty() {
            String::from_utf8_lossy(&out.stdout)
        } else {
            msg
        };
        bail!("schtasks failed: {}", msg.trim());
    }
    Ok(())
}

fn current_user() -> anyhow::Result<String> {
    let user = std::env::var("USERNAME").context("USERNAME not set")?;
    Ok(match std::env::var("USERDOMAIN") {
        Ok(d) if !d.is_empty() => format!("{d}\\{user}"),
        _ => user,
    })
}

fn strip_verbatim(p: &Path) -> String {
    let s = p.to_string_lossy();
    s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn task_xml(user: &str, exe: &str) -> String {
    let user = xml_escape(user);
    let conhost = std::env::var("SystemRoot")
        .map(|r| format!("{r}\\System32\\conhost.exe"))
        .unwrap_or_else(|_| r"C:\Windows\System32\conhost.exe".into());
    let conhost = xml_escape(&conhost);
    let args = xml_escape(&format!("--headless \"{exe}\" run"));
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>holo-files: OneDrive Files On-Demand sync engine (holo run).</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
      <Delay>PT15S</Delay>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>999</Count>
    </RestartOnFailure>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{conhost}</Command>
      <Arguments>{args}</Arguments>
    </Exec>
  </Actions>
</Task>
"#
    )
}

/// Set the stop event of a running daemon. `false` if none is listening.
#[cfg(windows)]
fn signal_stop() -> anyhow::Result<bool> {
    use windows::Win32::System::Threading::{EVENT_MODIFY_STATE, OpenEventW, SetEvent};
    use windows::core::HSTRING;
    // SAFETY: plain Win32 calls; the handle is closed before returning.
    unsafe {
        let Ok(h) = OpenEventW(EVENT_MODIFY_STATE, false, &HSTRING::from(STOP_EVENT)) else {
            return Ok(false);
        };
        let r = SetEvent(h);
        let _ = windows::Win32::Foundation::CloseHandle(h);
        r.context("signalling the daemon")?;
    }
    Ok(true)
}

#[cfg(not(windows))]
fn signal_stop() -> anyhow::Result<bool> {
    Ok(false)
}

/// Resolves when `holo service stop` signals the daemon. Never resolves if the
/// event can't be created (Ctrl+C still works).
pub async fn stop_requested() {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::WAIT_OBJECT_0;
        use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};
        use windows::core::HSTRING;
        // SAFETY: manual-reset, initially unset event; the handle lives for the process.
        let handle = unsafe { CreateEventW(None, true, false, &HSTRING::from(STOP_EVENT)) };
        if let Ok(h) = handle {
            let raw = h.0 as usize;
            let signalled = tokio::task::spawn_blocking(move || {
                let h = windows::Win32::Foundation::HANDLE(raw as *mut _);
                // SAFETY: `h` stays open (never closed) while we wait on it.
                unsafe { WaitForSingleObject(h, INFINITE) == WAIT_OBJECT_0 }
            })
            .await
            .unwrap_or(false);
            if signalled {
                return;
            }
        }
    }
    std::future::pending::<()>().await
}
