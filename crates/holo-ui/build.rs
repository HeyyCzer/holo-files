//! Rasterizes the SVG icons in `assets/icons` into OUT_DIR:
//! - `holo.ico` (from `holo.svg`), embedded in `holo-ui.exe` and used by the installer
//!   (`installer/build.ps1`);
//! - `<name>-<size>.rgba` (straight-alpha RGBA) for the tray and window icons (`src/icon.rs`).

#[path = "src/ico.rs"]
mod ico;

use resvg::{tiny_skia, usvg};
use std::path::{Path, PathBuf};

const ICONS: &str = "assets/icons";
const ICO_SIZES: [u32; 8] = [16, 20, 24, 32, 40, 48, 64, 256];
/// Tray icons (one per status) and the window icon.
const RGBA: [(&str, u32); 6] = [
    ("holo-synced", 32),
    ("holo-syncing", 32),
    ("holo-paused", 32),
    ("holo-attention", 32),
    ("holo-stopped", 32),
    ("holo", 64),
];

fn rasterize(svg: &Path, size: u32) -> Vec<u8> {
    let data = std::fs::read(svg).unwrap_or_else(|e| panic!("reading {}: {e}", svg.display()));
    let tree = usvg::Tree::from_data(&data, &usvg::Options::default())
        .unwrap_or_else(|e| panic!("parsing {}: {e}", svg.display()));
    let mut pixmap = tiny_skia::Pixmap::new(size, size).expect("non-zero icon size");
    let scale = size as f32 / tree.size().width().max(tree.size().height());
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect()
}

fn main() {
    println!("cargo:rerun-if-changed={ICONS}");
    println!("cargo:rerun-if-changed=src/ico.rs");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let svg = |name: &str| Path::new(ICONS).join(format!("{name}.svg"));

    for (name, size) in RGBA {
        std::fs::write(
            out.join(format!("{name}-{size}.rgba")),
            rasterize(&svg(name), size),
        )
        .expect("writing rgba icon");
    }

    let images: Vec<_> = ICO_SIZES
        .iter()
        .map(|&s| (s, rasterize(&svg("holo"), s)))
        .collect();
    let ico = out.join("holo.ico");
    std::fs::write(&ico, ico::ico(&images)).expect("writing holo.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let rc = out.join("holo.rc");
        let path = ico.display().to_string().replace('\\', "\\\\");
        std::fs::write(&rc, format!("1 ICON \"{path}\"\n")).expect("writing holo.rc");
        embed_resource::compile(&rc, embed_resource::NONE)
            .manifest_optional()
            .expect("embedding the icon");
    }
}
