//! Tarefas de desenvolvimento, chamadas via alias em `.cargo/config.toml`
//! (ou `cargo run -q -p xtask -- <tarefa>`):
//!
//! - `installer [-NoBuild]`: gera o instalador ([`installer`]);
//! - `bump <patch|minor|major> [--push]`: nova versão + tag ([`bump`]).

mod bump;
mod cmd;
mod installer;

use std::process::ExitCode;

const USAGE: &str =
    "uso: cargo xtask installer [-NoBuild]\n     cargo xtask bump <patch|minor|major> [--push]";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let result = match args.next().as_deref() {
        Some("installer") => installer::installer(args.collect()),
        Some("bump") => bump::bump(args.collect()),
        _ => Err(USAGE.to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
