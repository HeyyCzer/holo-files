//! Tarefas de desenvolvimento, chamadas via alias em `.cargo/config.toml`.
//!
//! `cargo xtask installer [-NoBuild]` -> `installer/build.ps1`
//! `cargo xtask bump <patch|minor|major> [--push]` -> sobe a versão do workspace, commita e
//! cria a tag `v<versão>` (com `--push`, envia branch + tag, disparando o `release.yml`)

use std::path::Path;
use std::process::{Command, ExitCode};

const USAGE: &str =
    "uso: cargo xtask installer [-NoBuild]\n     cargo xtask bump <patch|minor|major> [--push]";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let result = match args.next().as_deref() {
        Some("installer") => installer(args.collect()),
        Some("bump") => bump(args.collect()),
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

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask fica na raiz do workspace")
}

/// Roda `program args` na raiz do workspace; erro se não terminar com sucesso.
fn run(program: &str, args: &[&str]) -> Result<(), String> {
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
fn output(program: &str, args: &[&str]) -> Result<String, String> {
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

fn installer(extra: Vec<String>) -> Result<(), String> {
    let script = root().join("installer").join("build.ps1");
    let script = script.to_str().ok_or("caminho do build.ps1 não é UTF-8")?;
    let mut args = vec!["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", script];
    args.extend(extra.iter().map(String::as_str));
    run("powershell", &args)
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Part {
    Patch,
    Minor,
    Major,
}

type Version = (u64, u64, u64);

fn parse_version(s: &str) -> Option<Version> {
    let mut it = s.split('.').map(|p| p.parse::<u64>().ok());
    let v = (it.next()??, it.next()??, it.next()??);
    it.next().is_none().then_some(v)
}

fn bumped((major, minor, patch): Version, part: Part) -> Version {
    match part {
        Part::Patch => (major, minor, patch + 1),
        Part::Minor => (major, minor + 1, 0),
        Part::Major => (major + 1, 0, 0),
    }
}

/// Troca o `version = "x.y.z"` da seção `[workspace.package]` do `Cargo.toml` raiz.
/// Devolve o manifesto novo e as versões antiga/nova.
fn bump_manifest(manifest: &str, part: Part) -> Result<(String, Version, Version), String> {
    let mut section = "";
    let mut found = None;
    let mut lines: Vec<String> = Vec::new();
    for line in manifest.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            section = t;
        } else if section == "[workspace.package]"
            && found.is_none()
            && let Some(rest) = t.strip_prefix("version")
            && let Some(value) = rest.trim_start().strip_prefix('=')
        {
            let value = value.trim();
            let old = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .and_then(parse_version)
                .ok_or_else(|| format!("versão inválida no Cargo.toml: {value}"))?;
            let new = bumped(old, part);
            found = Some((old, new));
            lines.push(format!("version = \"{}.{}.{}\"", new.0, new.1, new.2));
            continue;
        }
        lines.push(line.to_string());
    }
    let (old, new) = found.ok_or("`version` não encontrado em [workspace.package]")?;
    let mut out = lines.join("\n");
    if manifest.ends_with('\n') {
        out.push('\n');
    }
    Ok((out, old, new))
}

fn bump(args: Vec<String>) -> Result<(), String> {
    let mut part = None;
    let mut push = false;
    for a in &args {
        match a.as_str() {
            "patch" => part = Some(Part::Patch),
            "minor" => part = Some(Part::Minor),
            "major" => part = Some(Part::Major),
            "--push" => push = true,
            _ => return Err(USAGE.to_string()),
        }
    }
    let part = part.ok_or(USAGE)?;

    if !output("git", &["status", "--porcelain"])?.trim().is_empty() {
        return Err("working tree com alterações: commite ou descarte antes do bump".into());
    }

    let path = root().join("Cargo.toml");
    let manifest =
        std::fs::read_to_string(&path).map_err(|e| format!("lendo {}: {e}", path.display()))?;
    let (new_manifest, old, new) = bump_manifest(&manifest, part)?;
    let tag = format!("v{}.{}.{}", new.0, new.1, new.2);
    if !output("git", &["tag", "--list", &tag])?.trim().is_empty() {
        return Err(format!("a tag {tag} já existe"));
    }

    std::fs::write(&path, new_manifest).map_err(|e| format!("gravando {}: {e}", path.display()))?;
    // Atualiza só as versões dos crates do workspace no Cargo.lock.
    run("cargo", &["update", "--workspace", "--quiet"])?;
    run("git", &["add", "Cargo.toml", "Cargo.lock"])?;
    run("git", &["commit", "--quiet", "-m", &format!("🔖 {tag}")])?;
    run("git", &["tag", "-a", &tag, "-m", &tag])?;
    println!(
        "{}.{}.{} -> {tag} (commit + tag criados)",
        old.0, old.1, old.2
    );

    if push {
        run("git", &["push", "--quiet"])?;
        run("git", &["push", "--quiet", "origin", &tag])?;
        println!("branch e {tag} enviados: o release.yml vai publicar a release");
    } else {
        println!("para publicar: git push; git push origin {tag}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = "[workspace]\nmembers = [\"a\"]\n\n[workspace.package]\nversion = \"0.1.9\"\nedition = \"2024\"\n\n[workspace.dependencies]\nfoo = { version = \"1\" }\n";

    #[test]
    fn bumps_each_part() {
        assert_eq!(bumped((1, 2, 3), Part::Patch), (1, 2, 4));
        assert_eq!(bumped((1, 2, 3), Part::Minor), (1, 3, 0));
        assert_eq!(bumped((1, 2, 3), Part::Major), (2, 0, 0));
    }

    #[test]
    fn parses_only_plain_semver() {
        assert_eq!(parse_version("0.10.2"), Some((0, 10, 2)));
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert_eq!(parse_version("1.2.3-beta"), None);
    }

    #[test]
    fn rewrites_only_the_workspace_package_version() {
        let (out, old, new) = bump_manifest(MANIFEST, Part::Minor).unwrap();
        assert_eq!((old, new), ((0, 1, 9), (0, 2, 0)));
        assert_eq!(
            out,
            MANIFEST.replace("version = \"0.1.9\"", "version = \"0.2.0\"")
        );
    }

    #[test]
    fn errors_without_workspace_version() {
        assert!(bump_manifest("[package]\nversion = \"1.0.0\"\n", Part::Patch).is_err());
    }
}
