//! Embeds the app icon (drawn by `src/icon_draw.rs`) in `holo-ui.exe`. The `.ico` also
//! stays in OUT_DIR for the installer (`installer/build.ps1`).

#[path = "src/icon_draw.rs"]
#[allow(dead_code)]
mod icon_draw;

fn main() {
    println!("cargo:rerun-if-changed=src/icon_draw.rs");
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let ico = out.join("holo.ico");
    let sizes = [16, 20, 24, 32, 40, 48, 64, 256];
    std::fs::write(&ico, icon_draw::ico(&icon_draw::APP, &sizes)).expect("writing holo.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let rc = out.join("holo.rc");
        let path = ico.display().to_string().replace('\\', "\\\\");
        std::fs::write(&rc, format!("1 ICON \"{path}\"\n")).expect("writing holo.rc");
        embed_resource::compile(&rc, embed_resource::NONE)
            .manifest_optional()
            .expect("embedding the icon");
    }
}
