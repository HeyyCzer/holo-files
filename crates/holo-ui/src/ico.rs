//! `.ico` encoder, shared by `build.rs` (which embeds `holo.ico` in the executable) and
//! the tests. No dependencies beyond std.

/// A `.ico` file with one 32-bit BMP image per `(size, straight-alpha RGBA)` (size ≤ 256).
pub fn ico(images: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let bmps: Vec<(u32, Vec<u8>)> = images
        .iter()
        .map(|(s, rgba)| {
            let s = *s;
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
    out.extend_from_slice(&(bmps.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * bmps.len() as u32;
    for (s, data) in &bmps {
        let dim = if *s >= 256 { 0 } else { *s as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += data.len() as u32;
    }
    for (_, data) in bmps {
        out.extend_from_slice(&data);
    }
    out
}
