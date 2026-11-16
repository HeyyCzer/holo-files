//! Status icons drawn in code (no image assets): a coloured disc with a white glyph.

use crate::snapshot::Status;

pub const SIZE: u32 = 32;

type Seg = ((f32, f32), (f32, f32));

fn style(status: Status) -> ([u8; 3], Vec<Seg>) {
    match status {
        // check mark
        Status::UpToDate => (
            [0x2e, 0x9e, 0x5b],
            vec![((9.5, 16.5), (14.0, 21.0)), ((14.0, 21.0), (22.5, 11.5))],
        ),
        // up arrow
        Status::Syncing => (
            [0x25, 0x74, 0xd9],
            vec![
                ((16.0, 23.0), (16.0, 9.5)),
                ((10.5, 15.0), (16.0, 9.5)),
                ((21.5, 15.0), (16.0, 9.5)),
            ],
        ),
        // two bars
        Status::Paused => (
            [0x7a, 0x80, 0x8a],
            vec![((12.5, 10.5), (12.5, 21.5)), ((19.5, 10.5), (19.5, 21.5))],
        ),
        // exclamation mark
        Status::Attention => (
            [0xd9, 0x4a, 0x2b],
            vec![((16.0, 9.0), (16.0, 18.0)), ((16.0, 22.5), (16.0, 22.6))],
        ),
        // cross
        Status::Stopped => (
            [0x4b, 0x50, 0x58],
            vec![((11.5, 11.5), (20.5, 20.5)), ((20.5, 11.5), (11.5, 20.5))],
        ),
    }
}

/// Background colour for `status`.
pub fn color(status: Status) -> [u8; 3] {
    style(status).0
}

fn dist_to_segment(p: (f32, f32), (a, b): Seg) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 {
        0.0
    } else {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0)
    };
    let (cx, cy) = (a.0 + t * dx, a.1 + t * dy);
    ((p.0 - cx).powi(2) + (p.1 - cy).powi(2)).sqrt()
}

/// RGBA pixels (straight alpha), `SIZE`×`SIZE`.
pub fn rgba(status: Status) -> Vec<u8> {
    let (bg, glyph) = style(status);
    let c = SIZE as f32 / 2.0;
    let radius = c - 1.0;
    let half_stroke = 1.6;
    let mut out = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let p = (x as f32 + 0.5, y as f32 + 0.5);
            let disc =
                (radius - ((p.0 - c).powi(2) + (p.1 - c).powi(2)).sqrt() + 0.5).clamp(0.0, 1.0);
            let d = glyph
                .iter()
                .map(|s| dist_to_segment(p, *s))
                .fold(f32::MAX, f32::min);
            let ink = (half_stroke - d + 0.5).clamp(0.0, 1.0);
            let mix = |b: u8| (b as f32 * (1.0 - ink) + 255.0 * ink).round() as u8;
            out.extend_from_slice(&[
                mix(bg[0]),
                mix(bg[1]),
                mix(bg[2]),
                (disc * 255.0).round() as u8,
            ]);
        }
    }
    out
}

pub fn tray(status: Status) -> tray_icon::Icon {
    tray_icon::Icon::from_rgba(rgba(status), SIZE, SIZE).expect("valid icon size")
}

pub fn window(status: Status) -> eframe::egui::IconData {
    eframe::egui::IconData {
        rgba: rgba(status),
        width: SIZE,
        height: SIZE,
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
}
