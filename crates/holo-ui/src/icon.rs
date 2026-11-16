//! Status icons drawn in code (no image assets): a coloured disc with a white glyph.

use crate::{
    icon_draw::{self, APP, BLUE, DARK, GREEN, GREY, Glyph, RED, Style},
    snapshot::Status,
};

pub const SIZE: u32 = 32;

pub fn style(status: Status) -> Style {
    let (bg, glyph) = match status {
        Status::UpToDate => (GREEN, Glyph::Check),
        Status::Syncing => (BLUE, Glyph::Arrow),
        Status::Paused => (GREY, Glyph::Bars),
        Status::Attention => (RED, Glyph::Bang),
        Status::Stopped => (DARK, Glyph::Cross),
    };
    Style { bg, glyph }
}

/// Background colour for `status`.
pub fn color(status: Status) -> [u8; 3] {
    style(status).bg
}

pub fn rgba(status: Status) -> Vec<u8> {
    icon_draw::render(&style(status), SIZE)
}

pub fn tray(status: Status) -> tray_icon::Icon {
    tray_icon::Icon::from_rgba(rgba(status), SIZE, SIZE).expect("valid icon size")
}

/// The window/taskbar icon is the app icon, not the status.
pub fn window() -> eframe::egui::IconData {
    eframe::egui::IconData {
        rgba: icon_draw::render(&APP, 64),
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
        let f = icon_draw::ico(&APP, &sizes);
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
