//! Settings presets: named "looks" (output formats with their quality settings, and resizing)
//! applied in one click. A few are built in; users can save their own, which are stored in
//! `presets.json` next to the settings file.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::optimizer::{ResizeMode, Settings};

/// What a preset sets. Where files go (Save to) isn't part of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Look {
    pub jpeg: bool,
    pub webp: bool,
    pub avif: bool,
    pub png: bool,
    pub jpeg_quality: u8,
    pub webp_quality: u8,
    pub webp_lossless: bool,
    pub avif_quality: u8,
    pub avif_speed: u8,
    pub png_level: u8,
    pub resize: bool,
    pub resize_mode: ResizeMode,
    pub max_width: u32,
    pub max_height: u32,
    pub resize_percent: u32,
    pub box_width: u32,
    pub box_height: u32,
    pub target_size: bool,
    pub target_kb: u32,
}

impl Default for Look {
    fn default() -> Self {
        Look::of(&Settings::default())
    }
}

impl Look {
    /// The look of some settings.
    pub fn of(s: &Settings) -> Look {
        Look {
            jpeg: s.jpeg,
            webp: s.webp,
            avif: s.avif,
            png: s.png,
            jpeg_quality: s.jpeg_quality,
            webp_quality: s.webp_quality,
            webp_lossless: s.webp_lossless,
            avif_quality: s.avif_quality,
            avif_speed: s.avif_speed,
            png_level: s.png_level,
            resize: s.resize,
            resize_mode: s.resize_mode,
            max_width: s.max_width,
            max_height: s.max_height,
            resize_percent: s.resize_percent,
            box_width: s.box_width,
            box_height: s.box_height,
            target_size: s.target_size,
            target_kb: s.target_kb,
        }
    }

    /// Set these on `s`, leaving the rest of it (where files go) as it is.
    pub fn apply(&self, s: &mut Settings) {
        s.jpeg = self.jpeg;
        s.webp = self.webp;
        s.avif = self.avif;
        s.png = self.png;
        s.jpeg_quality = self.jpeg_quality;
        s.webp_quality = self.webp_quality;
        s.webp_lossless = self.webp_lossless;
        s.avif_quality = self.avif_quality;
        s.avif_speed = self.avif_speed;
        s.png_level = self.png_level;
        s.resize = self.resize;
        s.resize_mode = self.resize_mode;
        s.max_width = self.max_width;
        s.max_height = self.max_height;
        s.resize_percent = self.resize_percent;
        s.box_width = self.box_width;
        s.box_height = self.box_height;
        s.target_size = self.target_size;
        s.target_kb = self.target_kb;
    }

    /// The same look with settings that have no effect cleared (the quality of a format that's
    /// off, resize values of other resize modes...), so equal results compare equal.
    fn effective(mut self) -> Look {
        if !self.jpeg {
            self.jpeg_quality = 0;
        }
        if !self.webp {
            self.webp_lossless = false;
        }
        if !self.webp || self.webp_lossless {
            self.webp_quality = 0;
        }
        if !self.avif {
            self.avif_quality = 0;
            self.avif_speed = 0;
        }
        if !self.png {
            self.png_level = 0;
        }
        if !self.resize {
            self.resize_mode = ResizeMode::FitWithin;
        }
        if !self.resize || self.resize_mode != ResizeMode::FitWithin {
            (self.max_width, self.max_height) = (0, 0);
        }
        if !self.resize || self.resize_mode != ResizeMode::Percent {
            self.resize_percent = 0;
        }
        if !self.resize || self.resize_mode != ResizeMode::Box {
            (self.box_width, self.box_height) = (0, 0);
        }
        // A target only changes JPEG, lossy WebP and AVIF, and then picks their quality.
        let lossy = self.jpeg || (self.webp && !self.webp_lossless) || self.avif;
        if !self.target_size || !lossy {
            (self.target_size, self.target_kb) = (false, 0);
        } else {
            (self.jpeg_quality, self.webp_quality, self.avif_quality) = (0, 0, 0);
        }
        self
    }

    /// Whether the two produce the same images.
    pub fn same_as(&self, other: &Look) -> bool {
        self.effective() == other.effective()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub look: Look,
    /// Built-in presets can't be deleted (and aren't saved to the user's file).
    #[serde(skip)]
    pub built_in: bool,
}

/// The presets that come with the app.
pub fn built_in() -> Vec<Preset> {
    let off = Look { jpeg: false, webp: false, avif: false, png: false, resize: false, ..Look::default() };
    let boxed = |w, h| (ResizeMode::Box, w, h);
    let preset = |name: &str, look: Look| Preset { name: name.into(), look, built_in: true };
    let with_box = |mut look: Look, (mode, w, h): (ResizeMode, u32, u32)| {
        look.resize = true;
        look.resize_mode = mode;
        (look.box_width, look.box_height) = (w, h);
        look
    };
    vec![
        preset("Web", with_box(Look { webp: true, webp_quality: 80, webp_lossless: false, ..off }, boxed(1920, 1080))),
        preset("Smallest", with_box(Look { avif: true, avif_quality: 60, avif_speed: 6, ..off }, boxed(1920, 1080))),
        preset("Photos", Look { jpeg: true, jpeg_quality: 85, ..off }),
        preset("Thumbnails", with_box(Look { webp: true, webp_quality: 75, webp_lossless: false, ..off }, boxed(230, 230))),
        preset("Lossless", Look { png: true, png_level: 4, webp: true, webp_lossless: true, ..off }),
        preset("Email", with_box(Look { jpeg: true, jpeg_quality: 75, ..off }, boxed(800, 600))),
    ]
}

/// The user's own presets from `path` (none if it's missing or unreadable).
pub fn load(path: &Path) -> Vec<Preset> {
    std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
}

/// Save the user's own presets to `path`.
pub fn save(path: &Path, presets: &[Preset]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mine: Vec<&Preset> = presets.iter().filter(|p| !p.built_in).collect();
    std::fs::write(path, serde_json::to_vec_pretty(&mine).map_err(std::io::Error::other)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_compare_by_what_they_produce() {
        let web = built_in().into_iter().find(|p| p.name == "Web").unwrap().look;
        // Settings of a switched-off format don't matter...
        assert!(web.same_as(&Look { jpeg_quality: 12, avif_speed: 2, ..web }));
        // ...nor values of other resize modes...
        assert!(web.same_as(&Look { max_width: 640, resize_percent: 33, ..web }));
        // ...but what's produced does.
        assert!(!web.same_as(&Look { webp_quality: 81, ..web }));
        assert!(!web.same_as(&Look { box_width: 1280, box_height: 720, ..web }));
        assert!(!web.same_as(&Look { resize: false, ..web }));
    }

    #[test]
    fn applying_a_look_keeps_where_files_go() {
        let mut settings = Settings { overwrite: false, skip_if_larger: false, ..Settings::default() };
        let thumbnails = built_in().into_iter().find(|p| p.name == "Thumbnails").unwrap().look;
        thumbnails.apply(&mut settings);
        assert!(Look::of(&settings).same_as(&thumbnails));
        assert!(!settings.overwrite && !settings.skip_if_larger);
    }

    #[test]
    fn user_presets_round_trip_without_the_built_ins() {
        let dir = std::env::temp_dir().join(format!("gio-test-presets-{}", std::process::id()));
        let path = dir.join("presets.json");
        let mut all = built_in();
        all.push(Preset { name: "Blog".into(), look: all[0].look, built_in: false });
        save(&path, &all).unwrap();
        let loaded = load(&path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "Blog");
        assert!(loaded[0].look.same_as(&all[0].look));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
