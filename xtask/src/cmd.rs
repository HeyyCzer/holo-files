//! Execução de processos na raiz do workspace.

use std::path::Path;
use std::process::Command;

pub fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask fica na raiz do workspace")
}

/// Roda `program args` na raiz do workspace; erro se não terminar com sucesso.
pub fn run(program: &str, args: &[&str]) -> Result<(), String> {
    let status = Command::new(program)
        .args(args)
        .current_dir(root())
        .status()
        .map_err(|e| format!("falha ao executar {program}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`{program} {}` falhou ({status})", args.join(" ")))
    }
}

/// Saída (stdout) de `program args` na raiz do workspace.
pub fn output(program: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program)
        .args(args)
        .current_dir(root())
        .output()
        .map_err(|e| format!("falha ao executar {program}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "`{program} {}` falhou: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
