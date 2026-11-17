//! Windows-only touches for the Slint window: the app icon (drawn at runtime from the system
//! icon font, so no image files are shipped), which icon font is installed, and a title bar
//! tinted to match the window background.

use std::sync::OnceLock;

use winapi::shared::minwindef::DWORD;
use winapi::shared::windef::{HBITMAP, HFONT, HICON, HWND};
use winapi::um::wingdi::*;
use winapi::um::winuser::*;

type Rgb = (u8, u8, u8);

/// App icon colors: a white photo glyph on an accent-colored plate.
const ICON_PLATE: Rgb = (0x00, 0x67, 0xC0);
const WHITE: Rgb = (0xFF, 0xFF, 0xFF);
const PHOTO: char = '\u{E91B}';

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// The installed icon font: Segoe Fluent Icons (Windows 11) or Segoe MDL2 Assets (Windows 10).
pub fn icon_font_face() -> &'static str {
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

/// A zeroed top-down 32-bit DIB section.
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
fn icon_from_bgra(size: i32, bgra: &[u8]) -> HICON {
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

/// The app icon: a white photo glyph on an accent-colored rounded square.
fn app_icon(physical_size: i32) -> HICON {
    let size = physical_size.max(16);
    let glyph_px = (size as f32 * 0.62).round() as i32;
    let coverage = glyph_coverage(PHOTO, size, glyph_px);
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
                mix(WHITE.2, ICON_PLATE.2),
                mix(WHITE.1, ICON_PLATE.1),
                mix(WHITE.0, ICON_PLATE.0),
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

/// Dark or light title bar, tinted to the window background `bg` (Windows 11).
pub fn tint_title_bar(hwnd: HWND, dark: bool, bg: Rgb, fg: Rgb) {
    use winapi::um::dwmapi::DwmSetWindowAttribute;
    const DWMWA_USE_IMMERSIVE_DARK_MODE: DWORD = 20;
    const DWMWA_CAPTION_COLOR: DWORD = 35;
    const DWMWA_TEXT_COLOR: DWORD = 36;
    let colorref = |c: Rgb| c.0 as DWORD | (c.1 as DWORD) << 8 | (c.2 as DWORD) << 16;
    unsafe {
        let set = |attr: DWORD, value: DWORD| {
            DwmSetWindowAttribute(hwnd, attr, &value as *const _ as _, std::mem::size_of::<DWORD>() as DWORD);
        };
        set(DWMWA_USE_IMMERSIVE_DARK_MODE, dark as DWORD);
        set(DWMWA_CAPTION_COLOR, colorref(bg));
        set(DWMWA_TEXT_COLOR, colorref(fg));
    }
}
