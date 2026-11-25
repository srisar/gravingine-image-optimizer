//! SVG (and gzipped SVGZ) input: parsed with usvg and drawn with resvg at whatever size an
//! output needs, so vector images stay sharp when they're enlarged.
//!
//! Only what's inside the file is drawn: embedded (data URL) images are, but links to other
//! files or the web aren't, so an SVG from elsewhere can't pull a file from this PC into an
//! output. Text uses the fonts installed on Windows, loaded the first time an SVG is opened.

use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result, bail};
use resvg::tiny_skia;
use resvg::usvg::{self, fontdb};

/// The largest width or height an SVG is drawn at. A bigger size is scaled down to fit.
pub const MAX_SIDE: u32 = 16_384;
/// The most pixels an SVG is drawn with (RGBA, 4 bytes each): the same memory limit as
/// decoding any other image.
const MAX_PIXELS: u64 = 512 * 1024 * 1024 / 4;

/// Whether `bytes` look like an SVG: gzip data (SVGZ), or XML text whose first element is
/// `<svg`.
pub fn is_svg(bytes: &[u8]) -> bool {
    if bytes.starts_with(&[0x1f, 0x8b]) {
        return true;
    }
    let head = &bytes[..bytes.len().min(4096)];
    let text = String::from_utf8_lossy(head);
    let text = text.trim_start_matches('\u{feff}').trim_start();
    text.starts_with('<') && text.contains("<svg")
}

/// A parsed SVG.
pub struct Svg {
    tree: usvg::Tree,
}

impl Svg {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let options = usvg::Options {
            // No `resources_dir` and a resolver that refuses every link: only embedded
            // images are drawn.
            image_href_resolver: usvg::ImageHrefResolver {
                resolve_data: usvg::ImageHrefResolver::default_data_resolver(),
                resolve_string: Box::new(|_, _| None),
            },
            fontdb: system_fonts(),
            ..usvg::Options::default()
        };
        let tree = usvg::Tree::from_data(bytes, &options).context("unsupported or corrupt SVG")?;
        Ok(Svg { tree })
    }

    /// The size the SVG asks for (its `width` and `height`, or its `viewBox`), in pixels,
    /// before any limit.
    fn natural(&self) -> (f64, f64) {
        let size = self.tree.size();
        (size.width() as f64, size.height() as f64)
    }

    /// The SVG's own size in pixels: what it asks for (its `width` and `height`, or its
    /// `viewBox`), scaled down to fit the drawing limits if it's over them. The flag is
    /// whether it had to be. Resizing scales from this size.
    pub fn size(&self) -> ((u32, u32), bool) {
        fit(self.natural(), 1.0)
    }

    /// Draw the SVG stretched to `width` x `height` pixels (straight alpha).
    pub fn render(&self, width: u32, height: u32) -> Result<image::RgbaImage> {
        let Some(mut pixmap) = tiny_skia::Pixmap::new(width, height) else {
            bail!("can't draw an SVG at {width} × {height}");
        };
        let (w, h) = self.natural();
        let transform = tiny_skia::Transform::from_scale((width as f64 / w) as f32, (height as f64 / h) as f32);
        resvg::render(&self.tree, transform, &mut pixmap.as_mut());
        // tiny-skia's pixels are premultiplied; the rest of the app uses straight alpha.
        let mut rgba = pixmap.take();
        for px in rgba.as_chunks_mut::<4>().0 {
            let a = px[3] as u32;
            if a != 0 && a != 255 {
                for c in &mut px[..3] {
                    *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
        image::RgbaImage::from_raw(width, height, rgba).context("drawing SVG")
    }
}

/// `size` scaled by `scale`, rounded to whole pixels (at least 1), then scaled down to fit
/// `MAX_SIDE` and `MAX_PIXELS` if needed (and whether it was).
pub fn fit((w, h): (f64, f64), scale: f64) -> ((u32, u32), bool) {
    let (w, h) = ((w * scale).max(1.0), (h * scale).max(1.0));
    let mut shrink = (MAX_SIDE as f64 / w).min(MAX_SIDE as f64 / h);
    shrink = shrink.min((MAX_PIXELS as f64 / (w * h)).sqrt());
    let limited = shrink < 1.0;
    let shrink = shrink.min(1.0);
    let side = |s: f64| ((s * shrink).round() as u32).clamp(1, MAX_SIDE);
    ((side(w), side(h)), limited)
}

/// The fonts installed on Windows, loaded once (the first SVG opened waits for it).
fn system_fonts() -> Arc<fontdb::Database> {
    static FONTS: OnceLock<Arc<fontdb::Database>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let mut db = fontdb::Database::new();
            db.load_system_fonts();
            // What a generic family ("sans-serif", ...) means: the Windows defaults.
            db.set_serif_family("Times New Roman");
            db.set_sans_serif_family("Arial");
            db.set_monospace_family("Consolas");
            db.set_cursive_family("Comic Sans MS");
            db.set_fantasy_family("Impact");
            Arc::new(db)
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="12" viewBox="0 0 24 12">
        <rect width="12" height="12" fill="#ff0000"/>
    </svg>"##;

    #[test]
    fn recognises_svg_and_svgz() {
        assert!(is_svg(ICON.as_bytes()));
        assert!(is_svg(b"\xef\xbb\xbf<?xml version=\"1.0\"?>\n<!-- logo -->\n<svg/>"));
        assert!(is_svg(&[0x1f, 0x8b, 8, 0]));
        assert!(!is_svg(b"<html><body></body></html>"));
        assert!(!is_svg(b"\x89PNG\r\n\x1a\n"));
    }

    #[test]
    fn draws_at_any_size() {
        let svg = Svg::parse(ICON.as_bytes()).unwrap();
        assert_eq!(svg.size(), ((24, 12), false));
        assert_eq!(fit((24.0, 12.0), 10.0), ((240, 120), false));
        let img = svg.render(240, 120).unwrap();
        // Left half red and opaque, right half transparent.
        assert_eq!(img.get_pixel(60, 60).0, [255, 0, 0, 255]);
        assert_eq!(img.get_pixel(180, 60).0[3], 0);
    }

    #[test]
    fn huge_sizes_are_scaled_down() {
        let huge = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100000" height="50000"/>"#;
        let svg = Svg::parse(huge.as_bytes()).unwrap();
        let ((w, h), limited) = svg.size();
        assert!(limited);
        assert!(w <= MAX_SIDE && h <= MAX_SIDE && (w as u64 * h as u64) <= MAX_PIXELS);
        assert_eq!((w as f64 / h as f64).round(), 2.0);
    }

    #[test]
    fn linked_files_are_not_drawn() {
        let dir = std::env::temp_dir().join(format!("gio-svg-link-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let photo = dir.join("secret.png");
        image::RgbaImage::from_pixel(8, 8, image::Rgba([0, 0, 255, 255])).save(&photo).unwrap();
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="8" height="8">
                <image width="8" height="8" xlink:href="{}"/>
            </svg>"#,
            photo.display()
        );
        // usvg's own defaults would draw it (so the link itself is fine)...
        let tree = usvg::Tree::from_data(svg.as_bytes(), &usvg::Options::default()).unwrap();
        let mut pixmap = tiny_skia::Pixmap::new(8, 8).unwrap();
        resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());
        assert_eq!(pixmap.pixel(4, 4).map(|p| p.alpha()), Some(255), "the test link doesn't resolve");
        // ...but ours don't.
        let img = Svg::parse(svg.as_bytes()).unwrap().render(8, 8).unwrap();
        assert!(img.pixels().all(|p| p.0[3] == 0), "a linked file was drawn");
        std::fs::remove_dir_all(&dir).ok();
    }
}
