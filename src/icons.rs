//! Native Windows icons rendered at runtime from the system icon font
//! (Segoe Fluent Icons on Windows 11, Segoe MDL2 Assets on Windows 10), so they
//! match Explorer/Settings and stay crisp at any DPI without shipping image files.

use std::sync::OnceLock;

use winapi::shared::windef::{HBITMAP, HFONT, HICON, HWND};
use winapi::um::commctrl::*;
use winapi::um::wingdi::*;
use winapi::um::winuser::*;

// Glyph code points (shared by Segoe Fluent Icons and Segoe MDL2 Assets).
pub const ADD_FILES: char = '\u{E8E5}'; // OpenFile
pub const ADD_FOLDER: char = '\u{E8F4}'; // NewFolder
pub const CLEAR: char = '\u{E74D}'; // Delete
pub const OPTIMIZE: char = '\u{E945}'; // LightningBolt
pub const CANCEL: char = '\u{E711}'; // Cancel
pub const OPEN_FOLDER: char = '\u{ED25}'; // OpenFolderHorizontal
pub const BROWSE: char = '\u{E8DA}'; // OpenLocal
pub const FORMATS: char = '\u{E91B}'; // Photo
pub const DROP: char = '\u{E898}'; // Upload
pub const DONE: char = '\u{E73E}'; // CheckMark
pub const RUNNING: char = '\u{E895}'; // Sync
pub const SKIPPED: char = '\u{E7BA}'; // Warning
pub const ERROR: char = '\u{EA39}'; // ErrorBadge

pub type Rgb = (u8, u8, u8);

// Windows 11 semantic colors (light theme).
pub const ACCENT: Rgb = (0x00, 0x67, 0xC0);
pub const SUCCESS: Rgb = (0x0F, 0x7B, 0x0F);
pub const CAUTION: Rgb = (0x9D, 0x5D, 0x00);
pub const CRITICAL: Rgb = (0xC4, 0x2B, 0x1C);
pub const WHITE: Rgb = (0xFF, 0xFF, 0xFF);


/// Logical (96 DPI) pixels to physical pixels for the system DPI.
pub fn px(logical: i32) -> i32 {
    crate::geom::px(logical)
}


fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// The installed icon font: Segoe Fluent Icons (Windows 11) or Segoe MDL2 Assets (Windows 10).
fn icon_font_face() -> &'static str {
    static FACE: OnceLock<&'static str> = OnceLock::new();
    FACE.get_or_init(|| {
        for face in ["Segoe Fluent Icons", "Segoe MDL2 Assets"] {
            unsafe {
                let font = create_font(face, 16);
                let dc = CreateCompatibleDC(std::ptr::null_mut());
                let old = SelectObject(dc, font as _);
                let mut buf = [0u16; 64];
                let n = GetTextFaceW(dc, buf.len() as i32, buf.as_mut_ptr());
                SelectObject(dc, old);
                DeleteDC(dc);
                DeleteObject(font as _);
                if n > 0 && String::from_utf16_lossy(&buf[..(n as usize - 1)]).eq_ignore_ascii_case(face) {
                    return face;
                }
            }
        }
        "Segoe MDL2 Assets"
    })
}

unsafe fn create_font(face: &str, height: i32) -> HFONT {
    let face = wide(face);
    unsafe {
        CreateFontW(
            -height, 0, 0, 0, FW_NORMAL, 0, 0, 0, DEFAULT_CHARSET, OUT_TT_PRECIS, CLIP_DEFAULT_PRECIS,
            ANTIALIASED_QUALITY, DEFAULT_PITCH, face.as_ptr(),
        )
    }
}

/// Anti-aliased coverage (0..=255) of `glyph` drawn centered in a `size` x `size` box.
fn glyph_coverage(glyph: char, size: i32, glyph_px: i32) -> Vec<u8> {
    unsafe {
        let (dc, bmp, bits) = dib(size);
        let old_bmp = SelectObject(dc, bmp as _);
        let font = create_font(icon_font_face(), glyph_px);
        let old_font = SelectObject(dc, font as _);
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(dc, RGB(255, 255, 255));
        let mut rc = winapi::shared::windef::RECT { left: 0, top: 0, right: size, bottom: size };
        let mut text = [0u16; 2];
        let text = glyph.encode_utf16(&mut text);
        DrawTextW(dc, text.as_ptr(), text.len() as i32, &mut rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
        GdiFlush();
        let pixels = std::slice::from_raw_parts(bits, (size * size * 4) as usize);
        let coverage = pixels.as_chunks::<4>().0.iter().map(|p| p[1]).collect();
        SelectObject(dc, old_font);
        DeleteObject(font as _);
        SelectObject(dc, old_bmp);
        DeleteObject(bmp as _);
        DeleteDC(dc);
        coverage
    }
}

/// A zeroed top-down 32-bit DIB section selected into nothing yet.
unsafe fn dib(size: i32) -> (winapi::shared::windef::HDC, HBITMAP, *mut u8) {
    unsafe {
        let dc = CreateCompatibleDC(std::ptr::null_mut());
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = size;
        info.bmiHeader.biHeight = -size;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB;
        let mut bits: *mut winapi::ctypes::c_void = std::ptr::null_mut();
        let bmp = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
        (dc, bmp, bits as *mut u8)
    }
}

/// Build an HICON from straight-alpha BGRA rows.
pub fn icon_from_bgra(size: i32, bgra: &[u8]) -> HICON {
    unsafe {
        let (dc, color, bits) = dib(size);
        std::ptr::copy_nonoverlapping(bgra.as_ptr(), bits, bgra.len());
        let mask_bits = vec![0u8; (((size + 15) / 16) * 2 * size) as usize];
        let mask = CreateBitmap(size, size, 1, 1, mask_bits.as_ptr() as _);
        let mut info = ICONINFO { fIcon: 1, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&mut info);
        DeleteObject(mask as _);
        DeleteObject(color as _);
        DeleteDC(dc);
        icon
    }
}

/// A glyph icon in `color`. `background` makes it opaque on that color; otherwise transparent.
pub fn glyph_icon(glyph: char, logical_size: i32, color: Rgb, background: Option<Rgb>) -> HICON {
    let size = px(logical_size);
    let coverage = glyph_coverage(glyph, size, size);
    let mut bgra = Vec::with_capacity(coverage.len() * 4);
    for a in coverage {
        match background {
            None => bgra.extend_from_slice(&[color.2, color.1, color.0, a]),
            Some(bg) => {
                let mix = |f: u8, b: u8| ((f as u32 * a as u32 + b as u32 * (255 - a as u32) + 127) / 255) as u8;
                bgra.extend_from_slice(&[mix(color.2, bg.2), mix(color.1, bg.1), mix(color.0, bg.0), 255]);
            }
        }
    }
    icon_from_bgra(size, &bgra)
}

/// The app icon: a white photo glyph on an accent-colored rounded square.
pub fn app_icon(physical_size: i32) -> HICON {
    let size = physical_size.max(16);
    let glyph_px = (size as f32 * 0.62).round() as i32;
    let coverage = glyph_coverage(FORMATS, size, glyph_px);
    let radius = size as f32 * 0.22;
    let inset = (size as f32 * 0.03).max(0.5);
    let mut bgra = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            // Anti-aliased rounded-rectangle plate (signed distance, 1px feather).
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let half = size as f32 / 2.0 - inset;
            let c = size as f32 / 2.0;
            let qx = (fx - c).abs() - (half - radius);
            let qy = (fy - c).abs() - (half - radius);
            let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - radius;
            let plate = (0.5 - outside).clamp(0.0, 1.0);
            let g = coverage[(y * size + x) as usize] as f32 / 255.0;
            let mix = |f: u8, b: u8| (f as f32 * g + b as f32 * (1.0 - g)).round() as u8;
            bgra.extend_from_slice(&[
                mix(WHITE.2, ACCENT.2),
                mix(WHITE.1, ACCENT.1),
                mix(WHITE.0, ACCENT.0),
                (plate * 255.0).round() as u8,
            ]);
        }
    }
    icon_from_bgra(size, &bgra)
}

/// Set the window's title bar / taskbar / Alt+Tab icons.
pub fn set_window_icon(hwnd: HWND) {
    unsafe {
        let small = app_icon(GetSystemMetrics(SM_CXSMICON));
        let big = app_icon(GetSystemMetrics(SM_CXICON));
        SendMessageW(hwnd, WM_SETICON, ICON_SMALL as usize, small as isize);
        SendMessageW(hwnd, WM_SETICON, ICON_BIG as usize, big as isize);
    }
}

/// Show `glyph` to the left of a push button's text, greyed out when the button is disabled.
pub fn set_button_icon(button: HWND, glyph: char, color: Rgb, disabled_color: Rgb) {
    unsafe {
        let size = px(16);
        let list = ImageList_Create(size, size, ILC_COLOR32, 6, 0);
        let normal = glyph_icon(glyph, 16, color, None);
        let disabled = glyph_icon(glyph, 16, disabled_color, None);
        // One image per state: normal, hot, pressed, disabled, defaulted, stylus-hot.
        for icon in [normal, normal, normal, disabled, normal, normal] {
            ImageList_ReplaceIcon(list, -1, icon);
        }
        DestroyIcon(normal);
        DestroyIcon(disabled);
        let mut info = BUTTON_IMAGELIST {
            himl: list,
            margin: winapi::shared::windef::RECT { left: px(2), top: 0, right: px(2), bottom: 0 },
            uAlign: BUTTON_IMAGELIST_ALIGN_LEFT,
        };
        // Replace (and free) any previous image list, e.g. after a theme change.
        let mut old: BUTTON_IMAGELIST = std::mem::zeroed();
        SendMessageW(button, BCM_GETIMAGELIST, 0, &mut old as *mut _ as isize);
        SendMessageW(button, BCM_SETIMAGELIST, 0, &mut info as *mut _ as isize);
        if !old.himl.is_null() {
            ImageList_Destroy(old.himl);
        }
    }
}

/// Put `icon` into a static control (SS_ICON).
pub fn set_static_icon(control: HWND, icon: HICON) {
    unsafe {
        SendMessageW(control, STM_SETICON, icon as usize, 0);
    }
}

pub const SELECT_ALL: char = '\u{E8B3}'; // SelectAll
pub const SELECT_NONE: char = '\u{E8E6}'; // ClearSelection
pub const FORMAT_WEBP: char = '\u{E774}'; // Globe: the web format
pub const FORMAT_AVIF: char = '\u{E734}'; // FavoriteStar: best compression
pub const FORMAT_JPEG: char = '\u{EB9F}'; // Photo2: photos
pub const FORMAT_PNG: char = '\u{E790}'; // Color (palette): graphics
pub const RESIZE: char = '\u{E740}'; // FullScreen (resize arrows)
const PHOTO: char = '\u{EB9F}'; // Photo2

/// Placeholder thumbnail (BGRA) shown until the real one is decoded: a photo glyph on a light tile.
pub fn placeholder_thumb(size: i32, tile: Rgb, glyph: Rgb) -> Vec<u8> {
    let coverage = glyph_coverage(PHOTO, size, size / 2);
    let mut bgra = Vec::with_capacity(coverage.len() * 4);
    for a in coverage {
        let mix = |f: u8, b: u8| ((f as u32 * a as u32 + b as u32 * (255 - a as u32) + 127) / 255) as u8;
        bgra.extend_from_slice(&[mix(glyph.2, tile.2), mix(glyph.1, tile.1), mix(glyph.0, tile.0), 255]);
    }
    bgra
}

/// A thumbnail-sized, mostly transparent BGRA image with a round status badge in the bottom-right
/// corner, to composite over thumbnails.
pub fn badge_overlay(glyph: char, color: Rgb, size: i32) -> Vec<u8> {
    let r = (size as f32 * 0.17).max(7.0);
    let (cx, cy) = (size as f32 - r - 1.0, size as f32 - r - 1.0);
    let glyph_box = (r * 2.0).ceil() as i32;
    let coverage = glyph_coverage(glyph, glyph_box, (r * 1.25).round() as i32);
    let (gx0, gy0) = ((cx - r).round() as i32, (cy - r).round() as i32);
    let mut bgra = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
            let disc = (r + 0.5 - d).clamp(0.0, 1.0);
            if disc <= 0.0 {
                continue;
            }
            let (lx, ly) = (x - gx0, y - gy0);
            let g = if (0..glyph_box).contains(&lx) && (0..glyph_box).contains(&ly) {
                coverage[(ly * glyph_box + lx) as usize] as f32 / 255.0
            } else {
                0.0
            };
            let mix = |f: u8| (f as f32 * g + 255.0 * (1.0 - g)).round() as u8;
            let i = ((y * size + x) * 4) as usize;
            bgra[i..i + 4].copy_from_slice(&[mix(color.2), mix(color.1), mix(color.0), (disc * 255.0).round() as u8]);
        }
    }
    bgra
}
