//! Tarefas de desenvolvimento, chamadas via alias em `.cargo/config.toml`.
//!
//! `cargo xtask installer [-NoBuild]` -> `installer/build.ps1`

use std::path::Path;
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("installer") => installer(args.collect()),
        _ => {
            eprintln!("uso: cargo xtask installer [-NoBuild]");
            ExitCode::FAILURE
        }
    }
}

fn installer(extra: Vec<String>) -> ExitCode {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask fica na raiz do workspace");
    let status = Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(root.join("installer").join("build.ps1"))
        .args(extra)
        .current_dir(root)
        .status();
    match status {
        Ok(s) if s.success() => ExitCode::SUCCESS,
        Ok(_) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("falha ao executar powershell: {e}");
            ExitCode::FAILURE
        }
    }
}
