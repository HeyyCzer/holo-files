//! Pure icon rasterizer, shared by the app (`icon.rs`) and `build.rs` (which embeds
//! `holo.ico` in the executable). No dependencies beyond std.

/// Glyph coordinates are in a 32×32 box and scaled to the target size.
pub type Seg = ((f32, f32), (f32, f32));

#[derive(Clone, Copy)]
pub enum Glyph {
    Check,
    Arrow,
    Bars,
    Bang,
    Cross,
}

pub struct Style {
    pub bg: [u8; 3],
    pub glyph: Glyph,
}

pub const GREEN: [u8; 3] = [0x2e, 0x9e, 0x5b];
pub const BLUE: [u8; 3] = [0x25, 0x74, 0xd9];
pub const GREY: [u8; 3] = [0x7a, 0x80, 0x8a];
pub const RED: [u8; 3] = [0xd9, 0x4a, 0x2b];
pub const DARK: [u8; 3] = [0x4b, 0x50, 0x58];

/// The application icon (Start menu, taskbar, installer).
pub const APP: Style = Style {
    bg: BLUE,
    glyph: Glyph::Arrow,
};

fn segments(g: Glyph) -> &'static [Seg] {
    match g {
        Glyph::Check => &[((9.5, 16.5), (14.0, 21.0)), ((14.0, 21.0), (22.5, 11.5))],
        Glyph::Arrow => &[
            ((16.0, 23.0), (16.0, 9.5)),
            ((10.5, 15.0), (16.0, 9.5)),
            ((21.5, 15.0), (16.0, 9.5)),
        ],
        Glyph::Bars => &[((12.5, 10.5), (12.5, 21.5)), ((19.5, 10.5), (19.5, 21.5))],
        Glyph::Bang => &[((16.0, 9.0), (16.0, 18.0)), ((16.0, 22.5), (16.0, 22.6))],
        Glyph::Cross => &[((11.5, 11.5), (20.5, 20.5)), ((20.5, 11.5), (11.5, 20.5))],
    }
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

/// RGBA pixels (straight alpha), `size`×`size`, rows top to bottom.
pub fn render(style: &Style, size: u32) -> Vec<u8> {
    let k = size as f32 / 32.0;
    let c = size as f32 / 2.0;
    let radius = c - k;
    let half_stroke = 1.6 * k;
    let glyph: Vec<Seg> = segments(style.glyph)
        .iter()
        .map(|((ax, ay), (bx, by))| ((ax * k, ay * k), (bx * k, by * k)))
        .collect();
    let bg = style.bg;
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
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

/// A `.ico` file with one 32-bit BMP image per size (≤ 256). Used by `build.rs`.
#[cfg_attr(not(test), allow(dead_code))]
pub fn ico(style: &Style, sizes: &[u32]) -> Vec<u8> {
    let images: Vec<(u32, Vec<u8>)> = sizes
        .iter()
        .map(|&s| {
            let rgba = render(style, s);
            let mut bmp = Vec::new();
            // BITMAPINFOHEADER; the height counts the colour image plus the AND mask.
            for v in [40u32, s, s * 2] {
                bmp.extend_from_slice(&v.to_le_bytes());
            }
            bmp.extend_from_slice(&1u16.to_le_bytes()); // planes
            bmp.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
            bmp.extend_from_slice(&[0; 24]); // compression, sizes, resolution, palette
            for y in (0..s).rev() {
                for x in 0..s {
                    let i = ((y * s + x) * 4) as usize;
                    bmp.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
                }
            }
            let mask_row = s.div_ceil(32) * 4;
            bmp.resize(bmp.len() + (mask_row * s) as usize, 0);
            (s, bmp)
        })
        .collect();

    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (s, data) in &images {
        let dim = if *s >= 256 { 0 } else { *s as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += data.len() as u32;
    }
    for (_, data) in images {
        out.extend_from_slice(&data);
    }
    out
}
