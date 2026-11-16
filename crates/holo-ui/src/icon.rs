//! Tray and window icons. The artwork lives in `assets/icons/*.svg` (one complete file per
//! status); `build.rs` rasterizes it into OUT_DIR and the pixels are embedded here.

use crate::snapshot::Status;

pub const SIZE: u32 = 32;

macro_rules! rgba {
    ($name:literal, $size:literal) => {
        include_bytes!(concat!(env!("OUT_DIR"), "/", $name, "-", $size, ".rgba"))
    };
}

pub fn rgba(status: Status) -> &'static [u8] {
    match status {
        Status::UpToDate => rgba!("holo-synced", 32),
        Status::Syncing => rgba!("holo-syncing", 32),
        Status::Paused => rgba!("holo-paused", 32),
        Status::Attention => rgba!("holo-attention", 32),
        Status::Stopped => rgba!("holo-stopped", 32),
    }
}

/// Badge colour of `status` (same as in its SVG).
pub fn color(status: Status) -> [u8; 3] {
    match status {
        Status::UpToDate => [0x2e, 0x9e, 0x5b],
        Status::Syncing => [0x25, 0x74, 0xd9],
        Status::Paused => [0x7a, 0x80, 0x8a],
        Status::Attention => [0xd9, 0x4a, 0x2b],
        Status::Stopped => [0x4b, 0x50, 0x58],
    }
}

pub fn tray(status: Status) -> tray_icon::Icon {
    tray_icon::Icon::from_rgba(rgba(status).to_vec(), SIZE, SIZE).expect("valid icon size")
}

/// The window/taskbar icon is the app icon, not the status.
pub fn window() -> eframe::egui::IconData {
    eframe::egui::IconData {
        rgba: rgba!("holo", 64).to_vec(),
        width: 64,
        height: 64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_have_transparent_corners_and_opaque_centre() {
        for s in [
            Status::UpToDate,
            Status::Syncing,
            Status::Paused,
            Status::Attention,
            Status::Stopped,
        ] {
            let px = rgba(s);
            assert_eq!(px.len(), (SIZE * SIZE * 4) as usize);
            assert_eq!(px[3], 0, "corner is transparent");
            let mid = ((SIZE / 2 * SIZE + SIZE / 2) * 4) as usize;
            assert_eq!(px[mid + 3], 255, "centre is opaque");
        }
    }

    #[test]
    fn ico_directory_is_consistent() {
        let sizes = [16, 32, 256];
        let images: Vec<_> = sizes
            .iter()
            .map(|&s| (s, vec![0u8; (s * s * 4) as usize]))
            .collect();
        let f = crate::ico::ico(&images);
        assert_eq!(&f[..4], &[0, 0, 1, 0]);
        assert_eq!(u16::from_le_bytes([f[4], f[5]]), 3);
        for (i, s) in sizes.iter().enumerate() {
            let e = &f[6 + 16 * i..6 + 16 * (i + 1)];
            assert_eq!(e[0], if *s == 256 { 0 } else { *s as u8 });
            let len = u32::from_le_bytes(e[8..12].try_into().unwrap()) as usize;
            let off = u32::from_le_bytes(e[12..16].try_into().unwrap()) as usize;
            assert!(off + len <= f.len());
            let width = u32::from_le_bytes(f[off + 4..off + 8].try_into().unwrap());
            assert_eq!(width, *s);
        }
    }
}
