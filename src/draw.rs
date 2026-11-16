//! Small GDI drawing helpers: anti-aliased rounded shapes (GDI's own are jagged),
//! text and icon-font glyphs. Coordinates are physical pixels.

use winapi::shared::windef::{HDC, HFONT, RECT};
use winapi::um::wingdi::*;
use winapi::um::winuser::{DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DrawTextW, FillRect};

use crate::theme::{Rgb, colorref};

pub fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
    RECT { left, top, right, bottom }
}

/// Fill with a solid color (aliased; for backgrounds).
pub fn fill(hdc: HDC, rc: &RECT, color: Rgb) {
    unsafe {
        let brush = CreateSolidBrush(colorref(color));
        FillRect(hdc, rc, brush);
        DeleteObject(brush as _);
    }
}

/// Anti-aliased rounded rectangle with an optional inner border.
///
/// Only the corners are rendered per pixel; straight edges and the interior are solid fills,
/// so large shapes (cards, the list frame) stay cheap to repaint.
pub fn round_rect(hdc: HDC, rc: &RECT, radius: f32, fill: Rgb, border: Option<(Rgb, f32)>) {
    let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
    if w <= 0 || h <= 0 {
        return;
    }
    let r = radius.min(w as f32 / 2.0).min(h as f32 / 2.0);
    let shade = move |x: f32, y: f32| {
        // Signed distance to the rounded box, then coverage of the shape and of its border.
        let (hw, hh) = (w as f32 / 2.0, h as f32 / 2.0);
        let qx = (x - hw).abs() - (hw - r);
        let qy = (y - hh).abs() - (hh - r);
        let d = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r;
        let outside = (0.5 - d).clamp(0.0, 1.0);
        match border {
            Some((bc, bw)) => (mix(fill, bc, (0.5 - (d + bw)).clamp(0.0, 1.0)), outside),
            None => (fill, outside),
        }
    };
    let corner = (r.ceil() as i32 + 1).max(1);
    if corner * 2 >= w.min(h) || w * h <= 4096 {
        blend(hdc, rc.left, rc.top, w, h, 0, 0, shade);
        return;
    }
    // Four anti-aliased corners.
    for (ox, oy) in [(0, 0), (w - corner, 0), (0, h - corner), (w - corner, h - corner)] {
        blend(hdc, rc.left + ox, rc.top + oy, corner, corner, ox, oy, shade);
    }
    // Solid interior (a cross between the corners) and straight border strips.
    let (l, t, r_, b) = (rc.left, rc.top, rc.right, rc.bottom);
    let bw = border.map_or(0, |(_, bw)| bw.round().max(1.0) as i32);
    self::fill(hdc, &rect(l + corner, t, r_ - corner, b), fill);
    self::fill(hdc, &rect(l, t + corner, l + corner, b - corner), fill);
    self::fill(hdc, &rect(r_ - corner, t + corner, r_, b - corner), fill);
    if let Some((bc, _)) = border {
        self::fill(hdc, &rect(l + corner, t, r_ - corner, t + bw), bc);
        self::fill(hdc, &rect(l + corner, b - bw, r_ - corner, b), bc);
        self::fill(hdc, &rect(l, t + corner, l + bw, b - corner), bc);
        self::fill(hdc, &rect(r_ - bw, t + corner, r_, b - corner), bc);
    }
}

/// Anti-aliased filled circle centered at (cx, cy), with an optional border.
pub fn circle(hdc: HDC, cx: f32, cy: f32, radius: f32, fill: Rgb, border: Option<(Rgb, f32)>) {
    let x0 = (cx - radius - 1.0).floor() as i32;
    let y0 = (cy - radius - 1.0).floor() as i32;
    let size = (radius * 2.0 + 3.0).ceil() as i32;
    blend(hdc, x0, y0, size, size, 0, 0, |x, y| {
        let d = ((x + x0 as f32 - cx).powi(2) + (y + y0 as f32 - cy).powi(2)).sqrt() - radius;
        let outside = (0.5 - d).clamp(0.0, 1.0);
        match border {
            Some((bc, bw)) => (mix(fill, bc, (0.5 - (d + bw)).clamp(0.0, 1.0)), outside),
            None => (fill, outside),
        }
    });
}

/// `a` where `t` = 1, `b` where `t` = 0.
pub fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let m = |x: u8, y: u8| (x as f32 * t + y as f32 * (1.0 - t)).round() as u8;
    (m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

/// Render `shade(x, y) -> (color, alpha)` into a premultiplied DIB and alpha-blend it onto
/// `hdc` at (x0, y0). `shade` gets pixel centers in shape coordinates, offset by (ox, oy)
/// when only part of a shape is rendered.
#[allow(clippy::too_many_arguments)]
fn blend(hdc: HDC, x0: i32, y0: i32, w: i32, h: i32, ox: i32, oy: i32, shade: impl Fn(f32, f32) -> (Rgb, f32)) {
    unsafe {
        let mem = CreateCompatibleDC(hdc);
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = w;
        info.bmiHeader.biHeight = -h;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB;
        let mut bits: *mut winapi::ctypes::c_void = std::ptr::null_mut();
        let bmp = CreateDIBSection(mem, &info, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
        if bmp.is_null() {
            DeleteDC(mem);
            return;
        }
        let px = std::slice::from_raw_parts_mut(bits as *mut u8, (w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let (c, a) = shade((x + ox) as f32 + 0.5, (y + oy) as f32 + 0.5);
                let i = ((y * w + x) * 4) as usize;
                let pm = |v: u8| (v as f32 * a).round() as u8;
                px[i] = pm(c.2);
                px[i + 1] = pm(c.1);
                px[i + 2] = pm(c.0);
                px[i + 3] = (a * 255.0).round() as u8;
            }
        }
        let old = SelectObject(mem, bmp as _);
        let bf = BLENDFUNCTION { BlendOp: AC_SRC_OVER, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA };
        AlphaBlend(hdc, x0, y0, w, h, mem, 0, 0, w, h, bf);
        SelectObject(mem, old);
        DeleteObject(bmp as _);
        DeleteDC(mem);
    }
}

/// Single-line text, left-aligned and vertically centered, ellipsized to fit.
pub fn text(hdc: HDC, s: &str, rc: &RECT, font: HFONT, color: Rgb) {
    draw_text(hdc, s, rc, font, color, DT_LEFT);
}

/// Single-line text centered in `rc`.
pub fn text_centered(hdc: HDC, s: &str, rc: &RECT, font: HFONT, color: Rgb) {
    draw_text(hdc, s, rc, font, color, DT_CENTER);
}

fn draw_text(hdc: HDC, s: &str, rc: &RECT, font: HFONT, color: Rgb, align: u32) {
    let wide: Vec<u16> = s.encode_utf16().collect();
    unsafe {
        let old = SelectObject(hdc, font as _);
        SetBkMode(hdc, TRANSPARENT as i32);
        SetTextColor(hdc, colorref(color));
        let mut r = *rc;
        DrawTextW(hdc, wide.as_ptr(), wide.len() as i32, &mut r, align | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);
        SelectObject(hdc, old);
    }
}

/// Width of `s` in `font`, in physical pixels.
pub fn text_width(hdc: HDC, s: &str, font: HFONT) -> i32 {
    let wide: Vec<u16> = s.encode_utf16().collect();
    unsafe {
        let old = SelectObject(hdc, font as _);
        let mut size = std::mem::zeroed();
        GetTextExtentPoint32W(hdc, wide.as_ptr(), wide.len() as i32, &mut size);
        SelectObject(hdc, old);
        size.cx
    }
}

/// Line height of `font`, in physical pixels.
pub fn line_height(hdc: HDC, font: HFONT) -> i32 {
    unsafe {
        let old = SelectObject(hdc, font as _);
        let mut tm: TEXTMETRICW = std::mem::zeroed();
        GetTextMetricsW(hdc, &mut tm);
        SelectObject(hdc, old);
        tm.tmHeight
    }
}
