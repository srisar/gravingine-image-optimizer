//! The Gravingine logo, drawn from code: for the window icon at runtime and, through
//! `build.rs` (which includes this file), the `.ico` embedded in the exe for Explorer.
//! No dependencies, so the build script can use it as is.

/// The Gravingine logo (res/gravingine-logo.svg, 200 × 200 view box): rounded squares
/// `(x, y, side, corner radius, color)` curling into a "G".
const LOGO: [(f32, f32, f32, f32, u32); 17] = [
    (115.1, 26.5, 29.2, 4.7, 0xFFB020),
    (83.1, 20.3, 27.5, 4.4, 0xFFA621),
    (53.1, 30.5, 25.8, 4.1, 0xFF9D21),
    (28.7, 59.4, 23.8, 3.8, 0xFF9321),
    (22.8, 88.0, 22.3, 3.6, 0xFF8922),
    (28.8, 115.3, 20.9, 3.3, 0xFF7E22),
    (46.3, 139.5, 19.4, 3.1, 0xFF7322),
    (68.9, 153.2, 18.1, 2.9, 0xFF6722),
    (92.1, 157.6, 16.9, 2.7, 0xFF5B22),
    (114.4, 154.2, 15.8, 2.5, 0xFA5533),
    (133.5, 144.5, 14.6, 2.3, 0xF54F3F),
    (149.6, 127.8, 13.4, 2.2, 0xEF4949),
    (157.4, 111.1, 12.5, 2.0, 0xEA4251),
    (160.2, 94.2, 11.6, 1.9, 0xE53B58),
    (143.9, 94.6, 10.7, 1.7, 0xE0335E),
    (128.3, 95.1, 9.9, 1.6, 0xDB2A64),
    (113.7, 95.4, 9.1, 1.5, 0xD61F69),
];

/// The logo as `size` x `size` straight-alpha BGRA pixels (top row first), cropped to its
/// squares and anti-aliased (4 × 4 samples per pixel).
pub fn logo_bgra(size: u32) -> Vec<u8> {
    // The squares span about 20..175 of the 200 view box; leave a little margin.
    let (origin, span) = (14.0, 168.0);
    let scale = span / size as f32;
    const SAMPLES: u32 = 4;
    let mut bgra = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let (mut r, mut g, mut b, mut hits) = (0u32, 0u32, 0u32, 0u32);
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let px = origin + (x as f32 + (sx as f32 + 0.5) / SAMPLES as f32) * scale;
                    let py = origin + (y as f32 + (sy as f32 + 0.5) / SAMPLES as f32) * scale;
                    if let Some(&(.., color)) = LOGO.iter().find(|&&(rx, ry, side, radius, _)| {
                        // Inside the rounded square (distance to its inner rectangle <= radius).
                        let dx = (px - (rx + side / 2.0)).abs() - (side / 2.0 - radius);
                        let dy = (py - (ry + side / 2.0)).abs() - (side / 2.0 - radius);
                        dx.max(0.0).powi(2) + dy.max(0.0).powi(2) <= radius * radius
                    }) {
                        r += color >> 16 & 0xFF;
                        g += color >> 8 & 0xFF;
                        b += color & 0xFF;
                        hits += 1;
                    }
                }
            }
            // Straight alpha: average color of the covered samples, alpha = coverage.
            let channel = |sum: u32| sum.checked_div(hits).unwrap_or(0) as u8;
            let alpha = (hits * 255 / (SAMPLES * SAMPLES)) as u8;
            bgra.extend_from_slice(&[channel(b), channel(g), channel(r), alpha]);
        }
    }
    bgra
}

/// A Windows `.ico` file with the logo at each of `sizes` (at most 256), as 32-bit images.
#[cfg_attr(not(test), allow(dead_code))] // used by build.rs
pub fn ico(sizes: &[u32]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes.iter().map(|&size| ico_image(size)).collect();
    let mut out = Vec::new();
    // ICONDIR: reserved, type 1 (icon), image count.
    for v in [0u16, 1, sizes.len() as u16] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    // ICONDIRENTRY per image; the image data follows all the entries.
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (&size, image) in sizes.iter().zip(&images) {
        let side = if size >= 256 { 0 } else { size as u8 }; // 0 means 256
        out.extend_from_slice(&[side, side, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes()); // planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(image.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += image.len() as u32;
    }
    for image in images {
        out.extend_from_slice(&image);
    }
    out
}

/// One icon image: a BITMAPINFOHEADER, the BGRA rows bottom-up, then an all-clear AND mask
/// (transparency comes from the alpha channel).
fn ico_image(size: u32) -> Vec<u8> {
    let pixels = logo_bgra(size);
    let mask_row = size.div_ceil(32) * 4;
    let mut out = Vec::new();
    for v in [40u32, size, size * 2] {
        out.extend_from_slice(&v.to_le_bytes()); // header size, width, height (color + mask)
    }
    out.extend_from_slice(&1u16.to_le_bytes()); // planes
    out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
    for v in [0u32, size * size * 4 + mask_row * size, 0, 0, 0, 0] {
        out.extend_from_slice(&v.to_le_bytes()); // compression, image size, resolution, colors
    }
    let row = (size * 4) as usize;
    for y in (0..size as usize).rev() {
        out.extend_from_slice(&pixels[y * row..(y + 1) * row]);
    }
    out.resize(out.len() + (mask_row * size) as usize, 0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logo_is_drawn_with_transparent_corners() {
        let size = 64;
        let bgra = logo_bgra(size);
        assert_eq!(bgra.len(), (size * size * 4) as usize);
        let alpha = |x: u32, y: u32| bgra[((y * size + x) * 4 + 3) as usize];
        // Corners are clear; a square in the top arc of the "G" is solid.
        assert_eq!(alpha(0, 0), 0);
        assert_eq!(alpha(size - 1, size - 1), 0);
        assert_eq!(alpha(size / 2, 6), 255);
    }

    #[test]
    fn ico_has_an_entry_per_size() {
        let sizes = [16, 32, 256];
        let ico = ico(&sizes);
        assert_eq!(&ico[..6], &[0, 0, 1, 0, 3, 0]);
        // The 256 px entry is written as 0; data offsets and lengths stay inside the file.
        assert_eq!(ico[6 + 2 * 16], 0);
        for i in 0..sizes.len() {
            let entry = &ico[6 + i * 16..6 + (i + 1) * 16];
            let len = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as usize;
            let offset = u32::from_le_bytes(entry[12..16].try_into().unwrap()) as usize;
            assert!(offset + len <= ico.len());
        }
    }
}
