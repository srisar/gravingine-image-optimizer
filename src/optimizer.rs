//! Image decoding, resizing and encoding to web formats.

use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use image::{DynamicImage, ImageDecoder, ImageReader, imageops::FilterType};
use serde::{Deserialize, Serialize};

/// Output formats the optimizer can produce.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Format {
    Jpeg,
    WebP,
    Avif,
    Png,
}

impl Format {
    pub const ALL: [Format; 4] = [Format::Jpeg, Format::WebP, Format::Avif, Format::Png];

    pub fn label(self) -> &'static str {
        match self {
            Format::Jpeg => "JPEG",
            Format::WebP => "WebP",
            Format::Avif => "AVIF",
            Format::Png => "PNG",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Jpeg => "jpg",
            Format::WebP => "webp",
            Format::Avif => "avif",
            Format::Png => "png",
        }
    }

}

/// Where optimized files are written.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputLocation {
    /// An `optimized` folder next to each source (or inside a dropped folder).
    Subfolder,
    /// A user-chosen directory.
    Custom(PathBuf),
}

pub const SUBFOLDER_NAME: &str = "optimized";

/// How images are resized when resizing is on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResizeMode {
    /// Shrink to fit within `max_width` x `max_height`.
    #[default]
    FitWithin,
    /// Scale both sides by `resize_percent`.
    Percent,
    /// Shrink to fit within the `box_width` x `box_height` preset, turned to match the
    /// image's orientation (a portrait photo fits 480 x 640 for a 640 x 480 box).
    Box,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub jpeg: bool,
    pub webp: bool,
    pub avif: bool,
    pub png: bool,

    pub jpeg_quality: u8,
    pub webp_quality: u8,
    pub webp_lossless: bool,
    pub avif_quality: u8,
    /// 1 (slowest, smallest) ..= 10 (fastest)
    pub avif_speed: u8,
    /// oxipng preset 0 ..= 6
    pub png_level: u8,

    pub resize: bool,
    pub resize_mode: ResizeMode,
    pub max_width: u32,
    pub max_height: u32,
    /// 1 ..= 100, used by `ResizeMode::Percent`.
    pub resize_percent: u32,
    /// The size preset used by `ResizeMode::Box` (kept apart from the custom `max_*`).
    pub box_width: u32,
    pub box_height: u32,

    /// Keep each output under `target_kb` at the highest quality that fits, for JPEG, lossy
    /// WebP and AVIF (their quality settings are set aside). PNG and lossless WebP aren't
    /// changed.
    pub target_size: bool,
    /// The size limit, in KB (1024 bytes).
    pub target_kb: u32,

    pub output: OutputLocation,
    /// Don't write an output that is not smaller than the original file.
    pub skip_if_larger: bool,
    pub overwrite: bool,
    /// Remove images from the list once they have been optimized (the app's list, not files).
    pub clear_when_done: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            jpeg: false,
            webp: true,
            avif: false,
            png: false,
            jpeg_quality: 80,
            webp_quality: 80,
            webp_lossless: false,
            avif_quality: 65,
            avif_speed: 6,
            png_level: 2,
            resize: false,
            resize_mode: ResizeMode::FitWithin,
            max_width: 1920,
            max_height: 1920,
            resize_percent: 50,
            box_width: 1920,
            box_height: 1080,
            target_size: false,
            target_kb: 200,
            output: OutputLocation::Subfolder,
            skip_if_larger: true,
            overwrite: true,
            clear_when_done: false,
        }
    }
}

impl Settings {
    pub fn enabled(&self, f: Format) -> bool {
        match f {
            Format::Jpeg => self.jpeg,
            Format::WebP => self.webp,
            Format::Avif => self.avif,
            Format::Png => self.png,
        }
    }


    pub fn formats(&self) -> Vec<Format> {
        Format::ALL.into_iter().filter(|f| self.enabled(*f)).collect()
    }

    /// The size limit in bytes, when one is on.
    pub fn target_bytes(&self) -> Option<u64> {
        (self.target_size && self.target_kb > 0).then(|| self.target_kb as u64 * 1024)
    }

    /// The quality setting of a lossy format; `None` for PNG and lossless WebP.
    pub fn quality(&self, format: Format) -> Option<u8> {
        match format {
            Format::Jpeg => Some(self.jpeg_quality),
            Format::WebP if !self.webp_lossless => Some(self.webp_quality),
            Format::Avif => Some(self.avif_quality),
            Format::WebP | Format::Png => None,
        }
    }

    /// The same settings with `format`'s quality set to `quality`.
    fn with_quality(&self, format: Format, quality: u8) -> Settings {
        let mut s = self.clone();
        match format {
            Format::Jpeg => s.jpeg_quality = quality,
            Format::WebP => s.webp_quality = quality,
            Format::Avif => s.avif_quality = quality,
            Format::Png => {}
        }
        s
    }
}

/// The quality range a target file size picks from.
pub const MIN_TARGET_QUALITY: u8 = 10;
pub const MAX_TARGET_QUALITY: u8 = 100;

/// One source image plus where its outputs go.
#[derive(Clone, Debug)]
pub struct Source {
    pub path: PathBuf,
    /// Directory the outputs are written into (before applying a custom output dir).
    pub default_out_dir: PathBuf,
    /// Sub path to use below a custom output directory (keeps folder structure).
    pub custom_rel_dir: PathBuf,
}

#[derive(Clone, Debug)]
pub enum Outcome {
    Written { path: PathBuf, size: u64 },
    /// Encoded but not written because it was not smaller than the original.
    Larger { size: u64 },
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct FormatResult {
    pub format: Format,
    pub outcome: Outcome,
    /// The quality picked to meet the target file size (when one was set for a lossy format).
    pub quality: Option<u8>,
    /// The target file size this output aimed for, in bytes.
    pub target: Option<u64>,
}

impl FormatResult {
    /// Written, but still larger than the target file size (even at the lowest quality).
    pub fn over_target(&self) -> bool {
        match (&self.outcome, self.target) {
            (Outcome::Written { size, .. }, Some(target)) => *size > target,
            _ => false,
        }
    }
}

/// An encoded output, with the quality picked to meet the target size (if one is set).
pub struct Encoded {
    pub data: Vec<u8>,
    pub quality: Option<u8>,
    /// Still over the target at the lowest quality.
    pub over_target: bool,
}

pub fn output_dir(src: &Source, settings: &Settings) -> PathBuf {
    match &settings.output {
        OutputLocation::Subfolder => src.default_out_dir.clone(),
        OutputLocation::Custom(dir) => dir.join(&src.custom_rel_dir),
    }
}

/// New dimensions for a `w` x `h` image, or `None` if it should keep its size.
/// Keeps the aspect ratio and never upscales.
pub fn target_size(w: u32, h: u32, settings: &Settings) -> Option<(u32, u32)> {
    if !settings.resize || w == 0 || h == 0 {
        return None;
    }
    let limit = |max: u32, side: u32| if max == 0 { 1.0 } else { max as f64 / side as f64 };
    let scale = match settings.resize_mode {
        ResizeMode::FitWithin => limit(settings.max_width, w).min(limit(settings.max_height, h)),
        ResizeMode::Percent => settings.resize_percent.clamp(1, 100) as f64 / 100.0,
        ResizeMode::Box => {
            // Turn the box to match the image: a portrait photo fits 480 x 640 for 640 x 480.
            let (bw, bh) = (settings.box_width, settings.box_height);
            let (bw, bh) = if (h > w) != (bh > bw) && h != w { (bh, bw) } else { (bw, bh) };
            limit(bw, w).min(limit(bh, h))
        }
    };
    if scale >= 1.0 {
        return None;
    }
    let new = |side: u32| ((side as f64 * scale).round() as u32).max(1);
    Some((new(w), new(h)))
}

/// A source image read and decoded once, ready to encode (several times, for previews).
pub struct Decoded {
    bytes: Vec<u8>,
    image: DynamicImage,
}

impl Decoded {
    pub fn open(path: &Path) -> Result<Self> {
        let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let image = decode(&bytes)?;
        Ok(Decoded { bytes, image })
    }

    /// Size of the original file in bytes.
    pub fn file_size(&self) -> u64 {
        self.bytes.len() as u64
    }

    /// The upright original pixels.
    pub fn rgba(&self) -> image::RgbaImage {
        self.image.to_rgba8()
    }

    /// The image at the size `settings` ask for, and whether that is a resize.
    fn resized(&self, settings: &Settings) -> (std::borrow::Cow<'_, DynamicImage>, bool) {
        match target_size(self.image.width(), self.image.height(), settings) {
            Some((w, h)) => (std::borrow::Cow::Owned(self.image.resize_exact(w, h, FilterType::Lanczos3)), true),
            None => (std::borrow::Cow::Borrowed(&self.image), false),
        }
    }

    /// Encode `img` (this image, possibly resized) as `format`, in memory.
    fn encode(&self, img: &DynamicImage, resized: bool, format: Format, settings: &Settings) -> Result<Vec<u8>> {
        catch_unwind(AssertUnwindSafe(|| match format {
            Format::Jpeg => encode_jpeg(img, settings.jpeg_quality),
            Format::WebP => encode_webp(img, settings.webp_quality, settings.webp_lossless),
            Format::Avif => encode_avif(img, settings.avif_quality, settings.avif_speed),
            Format::Png => {
                // Re-compress the original PNG directly when possible: keeps palettes/bit depth.
                let source_is_png = image::guess_format(&self.bytes).ok() == Some(image::ImageFormat::Png);
                let input = if source_is_png && !resized { Some(&self.bytes[..]) } else { None };
                encode_png(img, input, settings.png_level)
            }
        }))
        .unwrap_or_else(|_| Err(anyhow!("encoder crashed")))
    }

    /// Encode as `format`, meeting the target file size if one is set: the highest quality
    /// (from `MAX_TARGET_QUALITY` down to `MIN_TARGET_QUALITY`, whatever the format's quality
    /// setting) whose output fits, found by a binary search to within 2 points. If even the
    /// lowest quality is too big, that smallest output is returned, marked as over the target.
    /// With "skip files larger than the original" on, it also keeps under the original's size.
    fn encode_sized(&self, img: &DynamicImage, resized: bool, format: Format, settings: &Settings) -> Result<Encoded> {
        let top = MAX_TARGET_QUALITY;
        let (Some(target), Some(_)) = (settings.target_bytes(), settings.quality(format)) else {
            let data = self.encode(img, resized, format, settings)?;
            return Ok(Encoded { data, quality: None, over_target: false });
        };
        // With "skip files larger than the original" on, a larger output would be thrown
        // away, so it also has to come out smaller than the original.
        let budget = if settings.skip_if_larger { target.min(self.file_size().saturating_sub(1)) } else { target };
        let at = |quality: u8| self.encode(img, resized, format, &settings.with_quality(format, quality));
        let fits = |data: &[u8]| data.len() as u64 <= budget;
        let done = |data: Vec<u8>, quality: u8| {
            let over_target = data.len() as u64 > target;
            Ok(Encoded { data, quality: Some(quality), over_target })
        };

        let first = at(top)?;
        if fits(&first) {
            return done(first, top);
        }
        let lowest = at(MIN_TARGET_QUALITY)?;
        if !fits(&lowest) {
            return done(lowest, MIN_TARGET_QUALITY);
        }
        // `lo` fits and `hi` doesn't; narrow down.
        let (mut lo, mut hi, mut best) = (MIN_TARGET_QUALITY, top, lowest);
        while hi - lo > 2 {
            let mid = lo + (hi - lo) / 2;
            let data = at(mid)?;
            if fits(&data) {
                (lo, best) = (mid, data);
            } else {
                hi = mid;
            }
        }
        done(best, lo)
    }
}

/// What `format` with `settings` would produce, without saving anything (see [`preview`]).
pub struct Preview {
    /// Encoded size in bytes.
    pub size: u64,
    /// The encoded output decoded back, so compression artifacts show.
    pub pixels: image::RgbaImage,
    /// The quality picked to meet the target file size, if one is set.
    pub quality: Option<u8>,
    pub over_target: bool,
}

/// Encode `format` with `settings` in memory (meeting the target file size, if one is set)
/// and decode it back, without saving anything.
pub fn preview(decoded: &Decoded, format: Format, settings: &Settings) -> Result<Preview> {
    let (img, resized) = decoded.resized(settings);
    let encoded = decoded.encode_sized(&img, resized, format, settings)?;
    Ok(Preview {
        size: encoded.data.len() as u64,
        pixels: decode(&encoded.data)?.to_rgba8(),
        quality: encoded.quality,
        over_target: encoded.over_target,
    })
}

/// Decode, optionally resize and encode one source to every enabled format.
pub fn process(src: &Source, settings: &Settings) -> Result<Vec<FormatResult>> {
    let decoded = Decoded::open(&src.path)?;
    let original_size = decoded.file_size();
    let (img, resized) = decoded.resized(settings);
    let out_dir = output_dir(src, settings);
    let stem = src
        .path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "image".into());

    let results = settings
        .formats()
        .into_iter()
        .map(|format| {
            let encoded = decoded.encode_sized(&img, resized, format, settings);
            let quality = encoded.as_ref().ok().and_then(|e| e.quality);
            let target = settings.target_bytes().filter(|_| settings.quality(format).is_some());
            let outcome = match encoded {
                Err(e) => Outcome::Failed(format!("{e:#}")),
                Ok(Encoded { data, .. }) => {
                    let size = data.len() as u64;
                    if settings.skip_if_larger && size >= original_size {
                        Outcome::Larger { size }
                    } else {
                        let path = out_dir.join(format!("{stem}.{}", format.extension()));
                        match write_output(&path, &data, settings.overwrite) {
                            Ok(path) => Outcome::Written { path, size },
                            Err(e) => Outcome::Failed(format!("{e:#}")),
                        }
                    }
                }
            };
            FormatResult { format, outcome, quality, target }
        })
        .collect();

    Ok(results)
}

fn write_output(path: &Path, data: &[u8], overwrite: bool) -> Result<PathBuf> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    // Always create a fresh file (never write through an existing file or symlink at the
    // output path). Overwriting replaces the existing entry; otherwise a number is added.
    if overwrite
        && let Ok(meta) = fs::symlink_metadata(path)
        && !meta.is_dir()
    {
        fs::remove_file(path).with_context(|| format!("replacing {}", path.display()))?;
    }
    // Try the name itself, then (when not overwriting) "name-1", "name-2", … Each attempt is
    // the atomic create itself, so anything already there (even a broken link) is skipped.
    let attempts = if overwrite { 1 } else { MAX_NUMBERED_NAMES };
    for candidate in std::iter::once(path.to_path_buf()).chain(numbered_paths(path)).take(attempts) {
        match fs::OpenOptions::new().write(true).create_new(true).open(&candidate) {
            Ok(mut file) => {
                use std::io::Write;
                file.write_all(data).with_context(|| format!("writing {}", candidate.display()))?;
                return Ok(candidate);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && !overwrite => continue,
            Err(e) => return Err(e).with_context(|| format!("writing {}", candidate.display())),
        }
    }
    bail!("no free file name for {}", path.display())
}

/// How many numbered alternatives ("name-1" … ) to try before giving up.
const MAX_NUMBERED_NAMES: usize = 10_000;

/// "dir/name-1.ext", "dir/name-2.ext", …
fn numbered_paths(path: &Path) -> impl Iterator<Item = PathBuf> + '_ {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let ext = path.extension().unwrap_or_default().to_string_lossy();
    let dir = path.parent().unwrap_or(Path::new(""));
    (1..).map(move |i| dir.join(format!("{stem}-{i}.{ext}")))
}

/// A small preview of a source image for the file list.
pub struct Thumbnail {
    /// `size` x `size` RGBA pixels (straight alpha): the image scaled to cover the square,
    /// center-cropped.
    pub rgba: Vec<u8>,
    /// Side of the square, in pixels.
    pub size: u32,
    /// Dimensions of the full image (after applying its orientation).
    pub width: u32,
    pub height: u32,
    pub format: &'static str,
}

pub fn thumbnail(path: &Path, size: u32) -> Result<Thumbnail> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let format = match image::guess_format(&bytes) {
        Ok(image::ImageFormat::Jpeg) => "JPEG",
        Ok(image::ImageFormat::Png) => "PNG",
        Ok(image::ImageFormat::WebP) => "WebP",
        Ok(image::ImageFormat::Gif) => "GIF",
        Ok(image::ImageFormat::Bmp) => "BMP",
        Ok(image::ImageFormat::Tiff) => "TIFF",
        Ok(image::ImageFormat::Ico) => "ICO",
        Ok(image::ImageFormat::Avif) => "AVIF",
        _ => "Image",
    };
    let img = decode(&bytes)?;
    let (width, height) = (img.width(), img.height());
    let small = img.resize_to_fill(size, size, FilterType::Triangle).to_rgba8();
    Ok(Thumbnail { size: small.width(), rgba: small.into_raw(), width, height, format })
}

/// Full-size RGBA pixels of an image file, upright (EXIF orientation applied), for previews.
pub fn load_rgba(path: &Path) -> Result<image::RgbaImage> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(decode(&bytes)?.to_rgba8())
}

fn decode(bytes: &[u8]) -> Result<DynamicImage> {
    // The image crate has no AVIF decoder here; avif-decode (pure Rust, rav1d) reads AVIF.
    if is_avif(bytes) {
        return decode_avif(bytes).map(DynamicImage::ImageRgba8);
    }
    let mut decoder = ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()?
        .into_decoder()
        .context("unsupported or corrupt image")?;
    // Outputs drop EXIF, so bake the camera orientation into the pixels.
    let orientation = decoder.orientation().ok();
    let mut img = DynamicImage::from_decoder(decoder).context("decoding image")?;
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    Ok(img)
}

/// An AVIF file: an ISO-BMFF `ftyp` box with the `avif` (image) or `avis` (sequence) brand.
fn is_avif(bytes: &[u8]) -> bool {
    bytes.get(4..12).is_some_and(|b| b == b"ftypavif" || b == b"ftypavis")
}

/// The most memory one decoded image may take (RGBA, 16-bit worst case): the image crate's
/// default allocation limit, applied to AVIF too.
const MAX_DECODED_BYTES: u64 = 512 * 1024 * 1024;

/// The largest width x height declared in the file's `ispe` (image spatial extents) boxes.
/// Box layout: size, "ispe", version/flags, width, height (all 32-bit big-endian).
fn avif_declared_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let be = |at: usize| bytes.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    bytes
        .windows(4)
        .enumerate()
        .filter(|(_, w)| *w == b"ispe")
        .filter_map(|(at, _)| Some((be(at + 8)?, be(at + 12)?)))
        .max_by_key(|&(w, h)| w as u64 * h as u64)
}

/// Decode an AVIF file to 8-bit RGBA.
fn decode_avif(bytes: &[u8]) -> Result<image::RgbaImage> {
    use avif_decode::Image;
    // Refuse images too large to decode before the decoder allocates for them.
    match avif_declared_size(bytes) {
        Some((w, h)) if w as u64 * h as u64 * 8 > MAX_DECODED_BYTES => {
            bail!("AVIF image is too large to decode ({w} × {h})")
        }
        Some(_) => {}
        None => bail!("decoding AVIF: no image size in the file"),
    }
    let image = catch_unwind(AssertUnwindSafe(|| avif_decode::Decoder::from_avif(bytes).and_then(|d| d.to_image())))
        .map_err(|_| anyhow!("decoding AVIF: the decoder crashed"))?
        .map_err(|e| anyhow!("decoding AVIF: {e}"))?;
    // 16-bit samples use the full 0..=65535 range.
    let wide = |v: u16| (v >> 8) as u8;
    let (width, height, rgba): (usize, usize, Vec<u8>) = match image {
        Image::Rgb8(img) => (img.width(), img.height(), img.pixels().flat_map(|p| [p.r, p.g, p.b, 255]).collect()),
        Image::Rgba8(img) => (img.width(), img.height(), img.pixels().flat_map(|p| [p.r, p.g, p.b, p.a]).collect()),
        Image::Rgb16(img) => {
            (img.width(), img.height(), img.pixels().flat_map(|p| [wide(p.r), wide(p.g), wide(p.b), 255]).collect())
        }
        Image::Rgba16(img) => {
            (img.width(), img.height(), img.pixels().flat_map(|p| [wide(p.r), wide(p.g), wide(p.b), wide(p.a)]).collect())
        }
        Image::Gray8(img) => (img.width(), img.height(), img.pixels().flat_map(|p| { let v = p.value(); [v, v, v, 255] }).collect()),
        Image::Gray16(img) => {
            (img.width(), img.height(), img.pixels().flat_map(|p| { let v = wide(p.value()); [v, v, v, 255] }).collect())
        }
    };
    image::RgbaImage::from_raw(width as u32, height as u32, rgba).context("decoding AVIF: unexpected pixel buffer size")
}

fn encode_jpeg(img: &DynamicImage, quality: u8) -> Result<Vec<u8>> {
    // JPEG has no alpha channel: composite transparent pixels onto white.
    let rgb = if img.color().has_alpha() {
        let rgba = img.to_rgba8();
        let mut out = image::RgbImage::new(rgba.width(), rgba.height());
        for (dst, src) in out.pixels_mut().zip(rgba.pixels()) {
            let a = src[3] as u32;
            for c in 0..3 {
                dst[c] = ((src[c] as u32 * a + 255 * (255 - a) + 127) / 255) as u8;
            }
        }
        out
    } else {
        img.to_rgb8()
    };

    let mut comp = mozjpeg::Compress::new(mozjpeg::ColorSpace::JCS_RGB);
    comp.set_size(rgb.width() as usize, rgb.height() as usize);
    comp.set_quality(quality as f32);
    comp.set_progressive_mode();
    comp.set_optimize_scans(true);
    let mut started = comp.start_compress(Vec::new())?;
    started.write_scanlines(rgb.as_raw())?;
    Ok(started.finish()?)
}

fn encode_webp(img: &DynamicImage, quality: u8, lossless: bool) -> Result<Vec<u8>> {
    let mut config = webp::WebPConfig::new().map_err(|_| anyhow!("invalid WebP config"))?;
    config.lossless = lossless as i32;
    config.quality = if lossless { 75.0 } else { quality as f32 };
    config.method = if lossless { 4 } else { 6 };
    config.alpha_compression = 1;

    let mem = if img.color().has_alpha() {
        let rgba = img.to_rgba8();
        webp::Encoder::from_rgba(rgba.as_raw(), rgba.width(), rgba.height()).encode_advanced(&config)
    } else {
        let rgb = img.to_rgb8();
        webp::Encoder::from_rgb(rgb.as_raw(), rgb.width(), rgb.height()).encode_advanced(&config)
    }
    .map_err(|e| anyhow!("WebP encoding failed: {e:?}"))?;
    Ok(mem.to_vec())
}

fn encode_avif(img: &DynamicImage, quality: u8, speed: u8) -> Result<Vec<u8>> {
    let encoder = ravif::Encoder::new()
        .with_quality(quality as f32)
        .with_alpha_quality((quality as f32 + 10.0).min(100.0))
        .with_speed(speed.clamp(1, 10))
        .with_alpha_color_mode(ravif::AlphaColorMode::UnassociatedClean);
    let (w, h) = (img.width() as usize, img.height() as usize);

    let encoded = if img.color().has_alpha() {
        let rgba = img.to_rgba8();
        let px: Vec<rgb::RGBA8> = rgba
            .as_raw()
            .as_chunks::<4>().0.iter()
            .map(|p| rgb::RGBA8::new(p[0], p[1], p[2], p[3]))
            .collect();
        encoder.encode_rgba(imgref::Img::new(&px[..], w, h))
    } else {
        let rgb_img = img.to_rgb8();
        let px: Vec<rgb::RGB8> = rgb_img
            .as_raw()
            .as_chunks::<3>().0.iter()
            .map(|p| rgb::RGB8::new(p[0], p[1], p[2]))
            .collect();
        encoder.encode_rgb(imgref::Img::new(&px[..], w, h))
    }
    .map_err(|e| anyhow!("AVIF encoding failed: {e}"))?;
    Ok(encoded.avif_file)
}

fn encode_png(img: &DynamicImage, original_png: Option<&[u8]>, level: u8) -> Result<Vec<u8>> {
    let png = match original_png {
        Some(bytes) => bytes.to_vec(),
        None => {
            let mut buf = Vec::new();
            img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)?;
            buf
        }
    };
    let mut opts = oxipng::Options::from_preset(level.min(6));
    opts.strip = oxipng::StripChunks::Safe;
    oxipng::optimize_from_memory(&png, &opts).map_err(|e| anyhow!("PNG optimization failed: {e}"))
}

/// Extensions we accept as input.
pub const INPUT_EXTENSIONS: &[&str] = &["jpg", "jpeg", "jfif", "png", "webp", "avif", "gif", "bmp", "tif", "tiff", "ico"];

pub fn is_supported(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| INPUT_EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

/// Expand dropped files/folders into image sources.
pub fn collect_sources(paths: &[PathBuf]) -> Result<Vec<Source>> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            let folder_name = p.file_name().map(PathBuf::from).unwrap_or_default();
            for entry in walkdir::WalkDir::new(p)
                .follow_links(true)
                .into_iter()
                // Never re-process our own output folders.
                .filter_entry(|e| !(e.file_type().is_dir() && e.file_name() == SUBFOLDER_NAME))
                .filter_map(|e| e.ok())
            {
                let path = entry.path();
                if entry.file_type().is_file() && is_supported(path) {
                    let rel_dir = path
                        .parent()
                        .and_then(|d| d.strip_prefix(p).ok())
                        .map(Path::to_path_buf)
                        .unwrap_or_default();
                    out.push(Source {
                        path: path.to_path_buf(),
                        default_out_dir: p.join(SUBFOLDER_NAME).join(&rel_dir),
                        custom_rel_dir: folder_name.join(&rel_dir),
                    });
                }
            }
        } else if p.is_file() {
            if !is_supported(p) {
                continue;
            }
            let parent = p.parent().unwrap_or(Path::new("."));
            out.push(Source {
                path: p.clone(),
                default_out_dir: parent.join(SUBFOLDER_NAME),
                custom_rel_dir: PathBuf::new(),
            });
        } else {
            bail!("{} does not exist", p.display());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gio-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn avif_round_trips_through_the_decoder() {
        for alpha in [false, true] {
            let original = sample_image(alpha);
            let avif = encode_avif(&original, 90, 10).unwrap();
            assert!(is_avif(&avif));
            let decoded = decode(&avif).unwrap().to_rgba8();
            assert_eq!(decoded.dimensions(), (original.width(), original.height()));
            // Close to the original on average (catches wrong bit-depth scaling, swapped
            // channels or lost alpha, which are off by far more than compression).
            let original = original.to_rgba8();
            let diff: u64 = original
                .as_raw()
                .iter()
                .zip(decoded.as_raw())
                .map(|(a, b)| a.abs_diff(*b) as u64)
                .sum();
            let mean = diff as f64 / original.as_raw().len() as f64;
            assert!(mean < 6.0, "alpha={alpha}: mean difference {mean:.2}");
        }
    }

    /// A detailed (noisy) image that doesn't compress well, so file sizes depend on quality.
    fn noisy_image() -> DynamicImage {
        DynamicImage::ImageRgb8(image::RgbImage::from_fn(400, 300, |x, y| {
            let n = |k: u32| ((x.wrapping_mul(7919) ^ y.wrapping_mul(104_729).wrapping_add(k)) % 251) as u8;
            image::Rgb([n(1), n(2), n(3)])
        }))
    }

    #[test]
    fn target_size_picks_the_highest_quality_that_fits() {
        let dir = temp_dir("target");
        let src = dir.join("noise.png");
        noisy_image().save(&src).unwrap();
        let decoded = Decoded::open(&src).unwrap();
        let base = Settings { jpeg: true, webp: false, jpeg_quality: 90, skip_if_larger: false, ..Settings::default() };
        let full = preview(&decoded, Format::Jpeg, &base).unwrap();
        assert_eq!(full.quality, None, "no target: the quality setting as is");

        // A target well below the 90% size: a lower quality that fits.
        let target_kb = (full.size * 6 / 10 / 1024) as u32;
        let sized = Settings { target_size: true, target_kb, ..base.clone() };
        let fit = preview(&decoded, Format::Jpeg, &sized).unwrap();
        let q = fit.quality.unwrap();
        assert!(fit.size <= target_kb as u64 * 1024, "{} > {target_kb} KB", fit.size);
        assert!((MIN_TARGET_QUALITY..90).contains(&q), "quality {q}");
        assert!(!fit.over_target);
        // Within 2 points of the best: 3 points higher doesn't fit.
        let higher = preview(&decoded, Format::Jpeg, &Settings { target_size: false, jpeg_quality: q + 3, ..base.clone() }).unwrap();
        assert!(higher.size > target_kb as u64 * 1024, "quality {} would also fit", q + 3);

        // The quality setting doesn't hold it back: with the slider at 80% and room for more,
        // it goes above 80% (up to 100% when even that fits).
        let at = |quality| preview(&decoded, Format::Jpeg, &Settings { jpeg_quality: quality, ..base.clone() }).unwrap().size;
        let (q80, q100) = (at(80), at(100));
        let between = Settings { target_size: true, target_kb: ((q80 + q100) / 2 / 1024) as u32, jpeg_quality: 80, ..base.clone() };
        let up = preview(&decoded, Format::Jpeg, &between).unwrap();
        assert!(up.quality.unwrap() > 80, "quality {:?} with room above 80%", up.quality);
        let roomy = Settings { target_kb: (q100 / 1024 + 1) as u32, ..between };
        assert_eq!(preview(&decoded, Format::Jpeg, &roomy).unwrap().quality, Some(MAX_TARGET_QUALITY));

        // With "skip files larger than the original" on, it also stays under the original's
        // size, so the output isn't thrown away (the budget here is bigger than the original).
        let original = decoded.file_size();
        let skip = Settings { skip_if_larger: true, target_kb: (original.max(q100) / 1024 + 64) as u32, ..roomy };
        let kept = preview(&decoded, Format::Jpeg, &skip).unwrap();
        assert!(kept.size < original, "{} >= original {original}", kept.size);
        assert!(!kept.over_target);

        // Out of reach: the lowest quality, flagged.
        let tiny = preview(&decoded, Format::Jpeg, &Settings { target_kb: 1, ..sized.clone() }).unwrap();
        assert_eq!(tiny.quality, Some(MIN_TARGET_QUALITY));
        assert!(tiny.over_target);

        // Lossless formats aren't changed by a target.
        let png = preview(&decoded, Format::Png, &Settings { png: true, ..sized }).unwrap();
        assert_eq!(png.quality, None);
        assert!(!png.over_target);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn avif_files_are_accepted_as_input() {
        assert!(is_supported(Path::new("photo.avif")));
        assert!(is_supported(Path::new("PHOTO.AVIF")));
        let dir = temp_dir("avif-input");
        let src = dir.join("photo.avif");
        fs::write(&src, encode_avif(&sample_image(false), 80, 10).unwrap()).unwrap();
        let thumb = thumbnail(&src, 64).unwrap();
        assert_eq!((thumb.width, thumb.height, thumb.format), (320, 200, "AVIF"));
        let settings = Settings { webp: true, skip_if_larger: false, ..Settings::default() };
        let results = process(&collect_sources(std::slice::from_ref(&src)).unwrap()[0], &settings).unwrap();
        assert!(matches!(results[0].outcome, Outcome::Written { .. }), "{:?}", results[0].outcome);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn avif_claiming_a_huge_size_is_refused_before_decoding() {
        let mut avif = encode_avif(&sample_image(false), 60, 10).unwrap();
        assert_eq!(avif_declared_size(&avif), Some((320, 200)));
        // Rewrite every ispe box to claim 100 000 x 100 000 pixels.
        let at: Vec<usize> = avif.windows(4).enumerate().filter(|(_, w)| *w == b"ispe").map(|(i, _)| i).collect();
        for i in at {
            avif[i + 8..i + 12].copy_from_slice(&100_000u32.to_be_bytes());
            avif[i + 12..i + 16].copy_from_slice(&100_000u32.to_be_bytes());
        }
        let err = decode(&avif).unwrap_err().to_string();
        assert!(err.contains("too large"), "{err}");
    }

    #[test]
    fn preview_encodes_in_memory_at_the_output_size() {
        let dir = temp_dir("preview");
        let src_path = dir.join("photo.png");
        sample_image(false).save(&src_path).unwrap();
        let decoded = Decoded::open(&src_path).unwrap();
        let settings = Settings { resize: true, max_width: 160, max_height: 0, ..Settings::default() };
        for format in [Format::Jpeg, Format::WebP, Format::Png] {
            let preview = preview(&decoded, format, &settings).unwrap();
            assert!(preview.size > 0, "{format:?}");
            assert_eq!(preview.pixels.dimensions(), (160, 100), "{format:?}");
        }
        // Nothing is written next to the image.
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_output_numbers_or_replaces() {
        let dir = temp_dir("write");
        let target = dir.join("out").join("a.webp");
        assert_eq!(write_output(&target, b"one", false).unwrap(), target);
        // Not overwriting: a numbered name, the original untouched.
        let second = write_output(&target, b"two", false).unwrap();
        assert_eq!(second, dir.join("out").join("a-1.webp"));
        assert_eq!(fs::read(&target).unwrap(), b"one");
        // Taken numbered names are skipped too.
        assert_eq!(write_output(&target, b"two", false).unwrap(), dir.join("out").join("a-2.webp"));
        // Overwriting: the same name, new contents.
        assert_eq!(write_output(&target, b"three", true).unwrap(), target);
        assert_eq!(fs::read(&target).unwrap(), b"three");
        let _ = fs::remove_dir_all(&dir);
    }

    fn sample_image(alpha: bool) -> DynamicImage {
        let (w, h) = (320, 200);
        if alpha {
            DynamicImage::ImageRgba8(image::RgbaImage::from_fn(w, h, |x, y| {
                image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, if x < w / 2 { 255 } else { 64 }])
            }))
        } else {
            DynamicImage::ImageRgb8(image::RgbImage::from_fn(w, h, |x, y| {
                image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8])
            }))
        }
    }

    #[test]
    fn encodes_every_format_with_and_without_alpha() {
        for alpha in [false, true] {
            let img = sample_image(alpha);
            for (name, data) in [
                ("jpeg", encode_jpeg(&img, 80).unwrap()),
                ("webp", encode_webp(&img, 80, false).unwrap()),
                ("webp-lossless", encode_webp(&img, 80, true).unwrap()),
                ("avif", encode_avif(&img, 60, 10).unwrap()),
                ("png", encode_png(&img, None, 1).unwrap()),
            ] {
                assert!(data.len() > 20, "{name} alpha={alpha} produced {} bytes", data.len());
                // Round-trip through the decoder to make sure the output is valid.
                let back = decode(&data).unwrap();
                assert_eq!((back.width(), back.height()), (320, 200), "{name}");
            }
        }
    }

    #[test]
    fn target_size_by_percent_and_fit() {
        let pct = |p| Settings { resize: true, resize_mode: ResizeMode::Percent, resize_percent: p, ..Settings::default() };
        assert_eq!(target_size(4000, 3000, &pct(75)), Some((3000, 2250)));
        assert_eq!(target_size(4000, 3000, &pct(50)), Some((2000, 1500)));
        assert_eq!(target_size(4000, 3000, &pct(25)), Some((1000, 750)));
        assert_eq!(target_size(333, 101, &pct(33)), Some((110, 33)));
        assert_eq!(target_size(3, 1, &pct(1)), Some((1, 1)), "never collapses to 0 px");
        assert_eq!(target_size(4000, 3000, &pct(100)), None);

        let fit = |w, h| Settings { resize: true, max_width: w, max_height: h, ..Settings::default() };
        assert_eq!(target_size(4000, 3000, &fit(1920, 1920)), Some((1920, 1440)));
        assert_eq!(target_size(3000, 4000, &fit(1920, 0)), Some((1920, 2560)));
        assert_eq!(target_size(800, 600, &fit(1920, 1920)), None, "never upscales");
        assert_eq!(target_size(4000, 3000, &fit(0, 0)), None);

        let preset = |w, h| Settings { resize: true, resize_mode: ResizeMode::Box, box_width: w, box_height: h, ..Settings::default() };
        assert_eq!(target_size(4000, 3000, &preset(640, 480)), Some((640, 480)));
        assert_eq!(target_size(3000, 4000, &preset(640, 480)), Some((480, 640)), "turned for portrait");
        assert_eq!(target_size(1920, 1080, &preset(800, 600)), Some((800, 450)));
        assert_eq!(target_size(1080, 1920, &preset(800, 600)), Some((450, 800)));
        assert_eq!(target_size(1000, 1000, &preset(640, 480)), Some((480, 480)), "square fits the short side");
        assert_eq!(target_size(3000, 2000, &preset(230, 230)), Some((230, 153)));
        assert_eq!(target_size(600, 400, &preset(1920, 1080)), None, "never upscales");

        let off = Settings { resize: false, resize_mode: ResizeMode::Percent, ..Settings::default() };
        assert_eq!(target_size(4000, 3000, &off), None);
    }

    #[test]
    fn process_writes_outputs_and_resizes() {
        let dir = temp_dir("process");
        let src_path = dir.join("photo.png");
        sample_image(false).save(&src_path).unwrap();

        let settings = Settings {
            jpeg: true,
            webp: true,
            avif: true,
            png: true,
            avif_speed: 10,
            resize: true,
            max_width: 160,
            max_height: 0,
            skip_if_larger: false,
            ..Settings::default()
        };
        let sources = collect_sources(std::slice::from_ref(&dir)).unwrap();
        assert_eq!(sources.len(), 1);
        let results = process(&sources[0], &settings).unwrap();
        assert_eq!(results.len(), 4);
        for r in &results {
            match &r.outcome {
                Outcome::Written { path, size } => {
                    assert!(path.starts_with(dir.join(SUBFOLDER_NAME)));
                    assert_eq!(fs::metadata(path).unwrap().len(), *size);
                }
                other => panic!("{:?}: {other:?}", r.format),
            }
        }
        let jpg = image::open(dir.join(SUBFOLDER_NAME).join("photo.jpg")).unwrap();
        assert_eq!((jpg.width(), jpg.height()), (160, 100));

        // Output folder is skipped when the folder is scanned again.
        assert_eq!(collect_sources(std::slice::from_ref(&dir)).unwrap().len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }
}
