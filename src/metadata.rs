//! What outputs keep besides the pixels: the color profile (ICC) and EXIF metadata.
//!
//! - Color profiles: only RGB profiles that render differently from sRGB matter (an untagged
//!   image is read as sRGB anyway). They are embedded as they are, or the pixels are converted
//!   to sRGB with moxcms. Previews and thumbnails are converted for the screen.
//! - EXIF: the TIFF block is rebuilt from the fields worth keeping (camera, lens, exposure,
//!   date taken, author, copyright...), with the location only when asked for. Maker notes,
//!   the embedded thumbnail and the orientation (already applied to the pixels) never come
//!   along.

use anyhow::{Result, anyhow, bail};
use image::DynamicImage;
use moxcms::{ColorProfile, DataColorSpace, Layout, TransformOptions};

// ------------------------------------------------------------------ color profiles

/// `icc` if it's an RGB profile that renders differently from sRGB. `None` for sRGB (or an
/// equivalent: there are many sRGB profiles), for other color spaces (gray, CMYK: decoded
/// pixels are RGB by then) and for profiles that can't be read or applied.
pub fn usable_profile(icc: &[u8]) -> Option<Vec<u8>> {
    let profile = ColorProfile::new_from_slice(icc).ok()?;
    if profile.color_space != DataColorSpace::Rgb {
        return None;
    }
    let transform = profile
        .create_transform_8bit(Layout::Rgb, &ColorProfile::new_srgb(), Layout::Rgb, TransformOptions::default())
        .ok()?;
    // Judge it by what it does to a grid of colors.
    const STEPS: [u8; 5] = [0, 64, 128, 192, 255];
    let grid: Vec<u8> = STEPS
        .iter()
        .flat_map(|&r| STEPS.iter().flat_map(move |&g| STEPS.iter().flat_map(move |&b| [r, g, b])))
        .collect();
    let mut out = vec![0; grid.len()];
    transform.transform(&grid, &mut out).ok()?;
    let like_srgb = grid.iter().zip(&out).all(|(a, b)| a.abs_diff(*b) <= 2);
    (!like_srgb).then(|| icc.to_vec())
}

/// `img`, in the colors of the `icc` profile, converted to sRGB. 8-bit images stay 8-bit;
/// deeper ones come out 16-bit.
pub fn to_srgb(img: &DynamicImage, icc: &[u8]) -> Result<DynamicImage> {
    let profile = ColorProfile::new_from_slice(icc).map_err(|e| anyhow!("reading the color profile: {e}"))?;
    let srgb = ColorProfile::new_srgb();
    let options = TransformOptions::default();
    let failed = |e: moxcms::CmsError| anyhow!("converting to sRGB: {e}");
    let alpha = img.color().has_alpha();
    let deep = img.color().bytes_per_pixel() / img.color().channel_count() > 1;
    let layout = if alpha { Layout::Rgba } else { Layout::Rgb };
    Ok(if deep {
        let t = profile.create_transform_16bit(layout, &srgb, layout, options).map_err(failed)?;
        if alpha {
            let src = img.to_rgba16();
            let mut dst = src.clone();
            t.transform(&src, &mut dst).map_err(failed)?;
            DynamicImage::ImageRgba16(dst)
        } else {
            let src = img.to_rgb16();
            let mut dst = src.clone();
            t.transform(&src, &mut dst).map_err(failed)?;
            DynamicImage::ImageRgb16(dst)
        }
    } else {
        let t = profile.create_transform_8bit(layout, &srgb, layout, options).map_err(failed)?;
        if alpha {
            let src = img.to_rgba8();
            let mut dst = src.clone();
            t.transform(&src, &mut dst).map_err(failed)?;
            DynamicImage::ImageRgba8(dst)
        } else {
            let src = img.to_rgb8();
            let mut dst = src.clone();
            t.transform(&src, &mut dst).map_err(failed)?;
            DynamicImage::ImageRgb8(dst)
        }
    })
}

/// Pixels for the screen: converted to sRGB when the image has another profile.
pub fn display_rgba(img: &DynamicImage, icc: Option<&[u8]>) -> image::RgbaImage {
    match icc.map(|icc| to_srgb(img, icc)) {
        Some(Ok(srgb)) => srgb.to_rgba8(),
        _ => img.to_rgba8(),
    }
}

// ------------------------------------------------------------------ EXIF

const EXIF_IFD: u16 = 0x8769;
const GPS_IFD: u16 = 0x8825;
const PIXEL_X_DIMENSION: u16 = 0xA002;
const PIXEL_Y_DIMENSION: u16 = 0xA003;
const MAKER_NOTE: u16 = 0x927C;
const INTEROP_IFD: u16 = 0xA005;

/// The main-image fields kept: description, camera, resolution, software, date, author,
/// copyright, rating and the Windows title/comment/author/keywords/subject. (Not the
/// orientation, which is applied to the pixels, nor the layout of the original's pixel data.)
const KEEP_MAIN: [u16; 18] = [
    0x010E, 0x010F, 0x0110, 0x011A, 0x011B, 0x0128, 0x0131, 0x0132, 0x013B, 0x013C, 0x4746, 0x4749, 0x8298,
    0x9C9B, 0x9C9C, 0x9C9D, 0x9C9E, 0x9C9F,
];

/// Exif IFD fields kept: the standard ones (exposure, dates, lens...), except the maker
/// notes (proprietary and often tens of KB) and the interoperability pointer.
fn keep_exif_field(tag: u16) -> bool {
    (0x8000..=0xA500).contains(&tag) && !matches!(tag, MAKER_NOTE | INTEROP_IFD | EXIF_IFD | GPS_IFD)
}

/// Largest value read from a field: bigger ones are never camera or author info.
const MAX_FIELD: usize = 16 * 1024;
/// Largest EXIF block written (a JPEG APP1 segment holds at most 64 KB).
const MAX_EXIF: usize = 60 * 1024;

/// A TIFF field, its value kept as raw bytes in the block's byte order.
#[derive(Clone, Debug, PartialEq)]
struct Field {
    tag: u16,
    kind: u16,
    count: u32,
    value: Vec<u8>,
}

/// Size of one value of a TIFF field type.
fn type_size(kind: u16) -> Option<usize> {
    match kind {
        1 | 2 | 6 | 7 => Some(1),     // BYTE, ASCII, SBYTE, UNDEFINED
        3 | 8 => Some(2),             // SHORT, SSHORT
        4 | 9 | 11 | 13 => Some(4),   // LONG, SLONG, FLOAT, IFD
        5 | 10 | 12 => Some(8),       // RATIONAL, SRATIONAL, DOUBLE
        _ => None,
    }
}

struct Tiff<'a> {
    data: &'a [u8],
    big_endian: bool,
}

impl Tiff<'_> {
    fn u16(&self, at: usize) -> Option<u16> {
        let b: [u8; 2] = self.data.get(at..at.checked_add(2)?)?.try_into().ok()?;
        Some(if self.big_endian { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) })
    }

    fn u32(&self, at: usize) -> Option<u32> {
        let b: [u8; 4] = self.data.get(at..at.checked_add(4)?)?.try_into().ok()?;
        Some(if self.big_endian { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) })
    }

    /// The fields of the IFD at `at` (fields with unknown types or broken offsets are skipped).
    fn ifd(&self, at: usize) -> Option<Vec<Field>> {
        let count = self.u16(at)? as usize;
        if count > 1000 {
            return None;
        }
        let mut fields = Vec::with_capacity(count);
        for i in 0..count {
            let entry = at + 2 + i * 12;
            let (tag, kind, n) = (self.u16(entry)?, self.u16(entry + 2)?, self.u32(entry + 4)?);
            let Some(len) = type_size(kind).and_then(|size| (n as usize).checked_mul(size)) else { continue };
            if len > MAX_FIELD {
                continue;
            }
            let start = if len <= 4 { entry + 8 } else { self.u32(entry + 8)? as usize };
            let Some(value) = start.checked_add(len).and_then(|end| self.data.get(start..end)) else { continue };
            fields.push(Field { tag, kind, count: n, value: value.to_vec() });
        }
        Some(fields)
    }

    /// Where the sub-IFD that `tag` points to starts.
    fn pointer(&self, fields: &[Field], tag: u16) -> Option<usize> {
        let f = fields.iter().find(|f| f.tag == tag && matches!(f.kind, 4 | 13) && f.count == 1)?;
        Tiff { data: &f.value, big_endian: self.big_endian }.u32(0).map(|v| v as usize)
    }
}

/// The EXIF block to write: `raw` (a TIFF block, with or without the `Exif\0\0` header)
/// rebuilt with only the fields worth keeping, the location only if `keep_location`, and the
/// pixel dimensions set to `width` x `height`. `None` if nothing is left or it can't be read.
pub fn clean_exif(raw: &[u8], keep_location: bool, width: u32, height: u32) -> Option<Vec<u8>> {
    let data = raw.strip_prefix(b"Exif\0\0").unwrap_or(raw);
    let big_endian = match data.get(0..4)? {
        b"II*\0" => false,
        b"MM\0*" => true,
        _ => return None,
    };
    let tiff = Tiff { data, big_endian };
    let ifd0 = tiff.ifd(tiff.u32(4)? as usize)?;
    let sub_ifd = |tag| tiff.pointer(&ifd0, tag).and_then(|at| tiff.ifd(at)).unwrap_or_default();

    let main: Vec<Field> = ifd0.iter().filter(|f| KEEP_MAIN.contains(&f.tag)).cloned().collect();
    let exif: Vec<Field> = sub_ifd(EXIF_IFD)
        .into_iter()
        .filter(|f| keep_exif_field(f.tag) && f.kind != 13)
        .map(|f| match f.tag {
            PIXEL_X_DIMENSION => long(f.tag, width, big_endian),
            PIXEL_Y_DIMENSION => long(f.tag, height, big_endian),
            _ => f,
        })
        .collect();
    let gps: Vec<Field> =
        if keep_location { sub_ifd(GPS_IFD).into_iter().filter(|f| f.kind != 13).collect() } else { Vec::new() };
    if main.is_empty() && exif.is_empty() && gps.is_empty() {
        return None;
    }
    let out = write_tiff(main, exif, gps, big_endian);
    (out.len() <= MAX_EXIF).then_some(out)
}

/// A LONG field.
fn long(tag: u16, value: u32, big_endian: bool) -> Field {
    let value = if big_endian { value.to_be_bytes() } else { value.to_le_bytes() }.to_vec();
    Field { tag, kind: 4, count: 1, value }
}

/// Fields sorted by tag (as TIFF requires), one per tag.
fn sorted(mut fields: Vec<Field>) -> Vec<Field> {
    fields.sort_by_key(|f| f.tag);
    fields.dedup_by_key(|f| f.tag);
    fields
}

/// Bytes an IFD takes: the count, the entries, the next-IFD offset, then the values that
/// don't fit in an entry (each padded to an even length).
fn ifd_len(fields: &[Field]) -> usize {
    2 + fields.len() * 12 + 4 + fields.iter().filter(|f| f.value.len() > 4).map(|f| f.value.len().next_multiple_of(2)).sum::<usize>()
}

/// A TIFF block: the main IFD, then the Exif and GPS IFDs (when not empty), no thumbnail.
fn write_tiff(main: Vec<Field>, exif: Vec<Field>, gps: Vec<Field>, big_endian: bool) -> Vec<u8> {
    let (exif, gps) = (sorted(exif), sorted(gps));
    let mut main = main;
    // Pointers first, with their offsets filled in below (they're inline, so the sizes hold).
    if !exif.is_empty() {
        main.push(long(EXIF_IFD, 0, big_endian));
    }
    if !gps.is_empty() {
        main.push(long(GPS_IFD, 0, big_endian));
    }
    let mut main = sorted(main);
    let exif_at = 8 + ifd_len(&main);
    let gps_at = exif_at + if exif.is_empty() { 0 } else { ifd_len(&exif) };
    for f in &mut main {
        match f.tag {
            EXIF_IFD => *f = long(EXIF_IFD, exif_at as u32, big_endian),
            GPS_IFD => *f = long(GPS_IFD, gps_at as u32, big_endian),
            _ => {}
        }
    }

    let mut out = Vec::new();
    out.extend_from_slice(if big_endian { b"MM\0*" } else { b"II*\0" });
    put_u32(&mut out, 8, big_endian);
    for ifd in [&main, &exif, &gps] {
        if !ifd.is_empty() {
            write_ifd(&mut out, ifd, big_endian);
        }
    }
    out
}

fn put_u16(out: &mut Vec<u8>, v: u16, big_endian: bool) {
    out.extend_from_slice(&if big_endian { v.to_be_bytes() } else { v.to_le_bytes() });
}

fn put_u32(out: &mut Vec<u8>, v: u32, big_endian: bool) {
    out.extend_from_slice(&if big_endian { v.to_be_bytes() } else { v.to_le_bytes() });
}

/// Append an IFD, with its out-of-line values right after it.
fn write_ifd(out: &mut Vec<u8>, fields: &[Field], big_endian: bool) {
    let mut values = Vec::new();
    let values_at = out.len() + 2 + fields.len() * 12 + 4;
    put_u16(out, fields.len() as u16, big_endian);
    for f in fields {
        put_u16(out, f.tag, big_endian);
        put_u16(out, f.kind, big_endian);
        put_u32(out, f.count, big_endian);
        if f.value.len() <= 4 {
            let mut inline = [0u8; 4];
            inline[..f.value.len()].copy_from_slice(&f.value);
            out.extend_from_slice(&inline);
        } else {
            put_u32(out, (values_at + values.len()) as u32, big_endian);
            values.extend_from_slice(&f.value);
            if values.len() % 2 == 1 {
                values.push(0);
            }
        }
    }
    put_u32(out, 0, big_endian); // no next IFD (no thumbnail)
    out.extend_from_slice(&values);
}

// ------------------------------------------------------------------ containers

/// A JPEG APP1 payload for an EXIF block.
pub fn jpeg_exif_segment(tiff: &[u8]) -> Vec<u8> {
    [b"Exif\0\0".as_slice(), tiff].concat()
}

/// JPEG APP2 payloads for a color profile: `ICC_PROFILE\0`, the chunk's number (from 1) and
/// the number of chunks, then up to 64 KB of the profile each. (The mozjpeg crate's
/// `write_icc_profile` numbers chunks from 0, which readers following the spec reject.)
pub fn jpeg_icc_segments(icc: &[u8]) -> Vec<Vec<u8>> {
    const HEADER: &[u8] = b"ICC_PROFILE\0";
    const MAX_CHUNK: usize = 65533 - HEADER.len() - 2;
    let chunks: Vec<&[u8]> = icc.chunks(MAX_CHUNK).collect();
    let count = chunks.len().min(255);
    chunks.iter().take(count).enumerate().map(|(i, chunk)| [HEADER, &[i as u8 + 1, count as u8], chunk].concat()).collect()
}

/// A WebP file from libwebp (`VP8 ` or `VP8L`, or `VP8X` with `ALPH` and `VP8 `) with a color
/// profile and/or EXIF added: rewritten in the extended format with `ICCP` and `EXIF` chunks.
pub fn webp_with_metadata(webp: &[u8], width: u32, height: u32, icc: Option<&[u8]>, exif: Option<&[u8]>) -> Result<Vec<u8>> {
    if icc.is_none() && exif.is_none() {
        return Ok(webp.to_vec());
    }
    if webp.get(0..4) != Some(b"RIFF") || webp.get(8..12) != Some(b"WEBP") {
        bail!("not a WebP file");
    }
    // The image's own chunks, as they are.
    let mut image_chunks = Vec::new();
    let mut alpha = false;
    let mut at = 12;
    while let Some(header) = webp.get(at..at + 8) {
        let id: [u8; 4] = header[0..4].try_into()?;
        let len = u32::from_le_bytes(header[4..8].try_into()?) as usize;
        let data = webp.get(at + 8..at + 8 + len).ok_or_else(|| anyhow!("truncated WebP chunk"))?;
        match &id {
            b"ALPH" => alpha = true,
            // VP8L: signature byte, then 14 + 14 bits of size and the alpha-is-used bit.
            b"VP8L" => alpha |= data.get(1..5).is_some_and(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) >> 28 & 1 == 1),
            _ => {}
        }
        if matches!(&id, b"ALPH" | b"VP8 " | b"VP8L") {
            push_riff_chunk(&mut image_chunks, &id, data);
        }
        at += 8 + len + (len & 1);
    }
    if image_chunks.is_empty() {
        bail!("no image data in the WebP file");
    }

    let flags = if icc.is_some() { 0x20 } else { 0 } | if alpha { 0x10 } else { 0 } | if exif.is_some() { 0x08 } else { 0 };
    let mut vp8x = vec![flags, 0, 0, 0];
    vp8x.extend_from_slice(&(width - 1).to_le_bytes()[..3]);
    vp8x.extend_from_slice(&(height - 1).to_le_bytes()[..3]);

    let mut chunks = Vec::new();
    push_riff_chunk(&mut chunks, b"VP8X", &vp8x);
    if let Some(icc) = icc {
        push_riff_chunk(&mut chunks, b"ICCP", icc);
    }
    chunks.extend_from_slice(&image_chunks);
    if let Some(exif) = exif {
        push_riff_chunk(&mut chunks, b"EXIF", exif);
    }
    let mut out = Vec::with_capacity(12 + chunks.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(4 + chunks.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WEBP");
    out.extend_from_slice(&chunks);
    Ok(out)
}

fn push_riff_chunk(out: &mut Vec<u8>, id: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(id);
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
    if data.len() % 2 == 1 {
        out.push(0);
    }
}

/// A PNG file with an `eXIf` chunk added right after its header.
pub fn png_with_exif(png: &[u8], exif: &[u8]) -> Result<Vec<u8>> {
    // The 8-byte signature, then IHDR: length, type, 13 bytes of data, CRC.
    const AFTER_IHDR: usize = 8 + 8 + 13 + 4;
    if png.get(12..16) != Some(b"IHDR") || png.len() < AFTER_IHDR {
        bail!("not a PNG file");
    }
    let mut out = Vec::with_capacity(png.len() + exif.len() + 12);
    out.extend_from_slice(&png[..AFTER_IHDR]);
    out.extend_from_slice(&(exif.len() as u32).to_be_bytes());
    let typed = [b"eXIf".as_slice(), exif].concat();
    out.extend_from_slice(&typed);
    out.extend_from_slice(&crc32fast::hash(&typed).to_be_bytes());
    out.extend_from_slice(&png[AFTER_IHDR..]);
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const MAKE: u16 = 0x010F;
    pub const ORIENTATION: u16 = 0x0112;
    pub const COPYRIGHT: u16 = 0x8298;
    pub const DATE_TAKEN: u16 = 0x9003;
    pub const GPS_LATITUDE: u16 = 0x0002;

    fn ascii(tag: u16, s: &str) -> Field {
        let value = [s.as_bytes(), b"\0"].concat();
        Field { tag, kind: 2, count: value.len() as u32, value }
    }

    fn short(tag: u16, v: u16, big_endian: bool) -> Field {
        let value = if big_endian { v.to_be_bytes() } else { v.to_le_bytes() }.to_vec();
        Field { tag, kind: 3, count: 1, value }
    }

    /// A camera-like EXIF block: make, orientation, copyright, a date taken, pixel dimensions,
    /// 8 KB of maker notes and a GPS latitude.
    pub fn camera_exif(big_endian: bool, orientation: u16) -> Vec<u8> {
        let main = vec![ascii(MAKE, "Gravicam"), short(ORIENTATION, orientation, big_endian), ascii(COPYRIGHT, "(c) Gravitide")];
        let exif = vec![
            ascii(DATE_TAKEN, "2026:11:18 10:00:00"),
            long(PIXEL_X_DIMENSION, 4000, big_endian),
            long(PIXEL_Y_DIMENSION, 3000, big_endian),
            Field { tag: MAKER_NOTE, kind: 7, count: 8000, value: vec![0xAB; 8000] },
        ];
        let lat = [7u32, 1, 0, 1, 0, 1].iter().flat_map(|v| if big_endian { v.to_be_bytes() } else { v.to_le_bytes() }).collect();
        let gps = vec![Field { tag: GPS_LATITUDE, kind: 5, count: 3, value: lat }];
        write_tiff(main, exif, gps, big_endian)
    }

    /// Tags and values of one IFD.
    pub type Fields = Vec<(u16, Vec<u8>)>;

    /// Every field of an EXIF block, by IFD: (main, exif, gps).
    pub fn read_exif(raw: &[u8]) -> (Fields, Fields, Fields) {
        let data = raw.strip_prefix(b"Exif\0\0").unwrap_or(raw);
        let tiff = Tiff { data, big_endian: &data[0..2] == b"MM" };
        let ifd0 = tiff.ifd(tiff.u32(4).unwrap() as usize).unwrap();
        let sub = |tag| tiff.pointer(&ifd0, tag).map(|at| tiff.ifd(at).unwrap()).unwrap_or_default();
        let pairs = |fields: Vec<Field>| -> Fields { fields.into_iter().map(|f| (f.tag, f.value)).collect() };
        let (exif, gps) = (pairs(sub(EXIF_IFD)), pairs(sub(GPS_IFD)));
        (pairs(ifd0), exif, gps)
    }

    pub fn has(fields: &[(u16, Vec<u8>)], tag: u16) -> bool {
        fields.iter().any(|(t, _)| *t == tag)
    }

    #[test]
    fn exif_keeps_camera_and_author_but_not_location_maker_notes_or_orientation() {
        for big_endian in [false, true] {
            let raw = camera_exif(big_endian, 6);
            let cleaned = clean_exif(&raw, false, 800, 600).unwrap();
            let (main, exif, gps) = read_exif(&cleaned);
            assert!(has(&main, MAKE) && has(&main, COPYRIGHT));
            assert!(!has(&main, ORIENTATION), "the orientation is applied to the pixels");
            assert!(has(&exif, DATE_TAKEN));
            assert!(!has(&exif, MAKER_NOTE));
            assert!(gps.is_empty() && !has(&main, GPS_IFD));
            let dims: Vec<u32> = exif
                .iter()
                .filter(|(t, _)| matches!(*t, PIXEL_X_DIMENSION | PIXEL_Y_DIMENSION))
                .map(|(_, v)| Tiff { data: v, big_endian }.u32(0).unwrap())
                .collect();
            assert_eq!(dims, [800, 600]);
            assert!(cleaned.len() < 400, "{} bytes", cleaned.len());

            let with_location = clean_exif(&raw, true, 800, 600).unwrap();
            let (_, _, gps) = read_exif(&with_location);
            assert!(has(&gps, GPS_LATITUDE));
        }
        // "Exif\0\0"-prefixed blocks (as JPEG has them) are read too.
        assert!(clean_exif(&jpeg_exif_segment(&camera_exif(false, 1)), false, 1, 1).is_some());
    }

    #[test]
    fn broken_exif_is_dropped() {
        assert_eq!(clean_exif(b"", false, 1, 1), None);
        assert_eq!(clean_exif(b"II*\0\xff\xff\xff\xff", false, 1, 1), None);
        let mut raw = camera_exif(false, 1);
        raw.truncate(20);
        assert_eq!(clean_exif(&raw, false, 1, 1), None);
        // Only an orientation: nothing worth keeping.
        assert_eq!(clean_exif(&write_tiff(vec![short(ORIENTATION, 6, false)], vec![], vec![], false), false, 1, 1), None);
    }

    #[test]
    fn srgb_profiles_are_left_out_and_others_kept() {
        let srgb = ColorProfile::new_srgb().encode().unwrap();
        let p3 = ColorProfile::new_display_p3().encode().unwrap();
        assert_eq!(usable_profile(&srgb), None);
        assert_eq!(usable_profile(&p3), Some(p3.clone()));
        assert_eq!(usable_profile(b"not a profile"), None);

        // P3's red is redder than sRGB's: converted, it needs more red and less of the rest.
        let img = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(2, 2, image::Rgb([200, 60, 40])));
        let converted = to_srgb(&img, &p3).unwrap().to_rgb8();
        let px = converted.get_pixel(0, 0);
        assert!(px[0] > 200 && px[1] < 60, "{px:?}");
    }
}
