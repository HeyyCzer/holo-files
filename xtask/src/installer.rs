//! `cargo xtask installer [-NoBuild]` -> `installer/build.ps1`.

use crate::cmd::{root, run};

pub fn installer(extra: Vec<String>) -> Result<(), String> {
    let script = root().join("installer").join("build.ps1");
    let script = script.to_str().ok_or("caminho do build.ps1 não é UTF-8")?;
    let mut args = vec!["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", script];
    args.extend(extra.iter().map(String::as_str));
    run("powershell", &args)
}
