//! Window geometry in logical (96 DPI) pixels.
//!
//! native-windows-gui's own size/position helpers call `GetDC` on the screen for every
//! conversion and never release it; called on every resize step and paint, that leaks device
//! contexts until Windows slows down (and eventually fails). The app is system-DPI aware, so
//! the scale is read once here and reused.

use std::sync::OnceLock;

use winapi::shared::windef::{HWND, POINT, RECT};
use winapi::um::wingdi::{GetDeviceCaps, LOGPIXELSX};
use winapi::um::winuser::*;

/// System DPI scale (1.0 = 96 DPI), read once.
pub fn scale() -> f64 {
    static SCALE: OnceLock<f64> = OnceLock::new();
    *SCALE.get_or_init(|| unsafe {
        let dc = GetDC(std::ptr::null_mut());
        let dpi = if dc.is_null() { 96 } else { GetDeviceCaps(dc, LOGPIXELSX) };
        if !dc.is_null() {
            ReleaseDC(std::ptr::null_mut(), dc);
        }
        if dpi > 0 { dpi as f64 / 96.0 } else { 1.0 }
    })
}

/// Logical to physical pixels.
pub fn px(logical: i32) -> i32 {
    (logical as f64 * scale()).round() as i32
}

/// Physical to logical pixels.
pub fn logical(physical: i32) -> i32 {
    (physical as f64 / scale()).round() as i32
}

/// Client-area size of a window.
pub fn client_size(hwnd: HWND) -> (i32, i32) {
    let mut rc: RECT = unsafe { std::mem::zeroed() };
    unsafe { GetClientRect(hwnd, &mut rc) };
    (logical(rc.right - rc.left), logical(rc.bottom - rc.top))
}

/// Outer size of a control.
pub fn size(hwnd: HWND) -> (i32, i32) {
    let mut rc: RECT = unsafe { std::mem::zeroed() };
    unsafe { GetWindowRect(hwnd, &mut rc) };
    (logical(rc.right - rc.left), logical(rc.bottom - rc.top))
}

/// Last (x, y, w, h, flags) requested per window.
type Request = (i32, i32, i32, i32, u32);

thread_local! {
    static LAST_REQUEST: std::cell::RefCell<std::collections::HashMap<usize, Request>> = Default::default();
}

/// Move/resize, skipping the call (and the repaint it causes) when nothing would change.
fn set_pos(hwnd: HWND, x: i32, y: i32, w: i32, h: i32, flags: u32) {
    unsafe {
        let mut rc: RECT = std::mem::zeroed();
        GetWindowRect(hwnd, &mut rc);
        let mut pt = POINT { x: rc.left, y: rc.top };
        ScreenToClient(GetParent(hwnd), &mut pt);
        let same_pos = flags & SWP_NOMOVE != 0 || (pt.x == px(x) && pt.y == px(y));
        let same_size = flags & SWP_NOSIZE != 0 || (rc.right - rc.left == px(w) && rc.bottom - rc.top == px(h));
        if same_pos && same_size {
            return;
        }
        // Some controls never take the requested size (a combo box's height comes from its
        // font), so also skip a request identical to the previous one for this window.
        let request = (x, y, w, h, flags);
        let repeat = LAST_REQUEST.with(|m| m.borrow_mut().insert(hwnd as usize, request) == Some(request));
        if repeat {
            return;
        }
        SetWindowPos(hwnd, std::ptr::null_mut(), px(x), px(y), px(w), px(h), flags | SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

/// Move and resize a control (logical pixels, relative to its parent).
pub fn place(hwnd: HWND, x: i32, y: i32, w: i32, h: i32) {
    set_pos(hwnd, x, y, w, h, 0);
}

pub fn move_to(hwnd: HWND, x: i32, y: i32) {
    set_pos(hwnd, x, y, 0, 0, SWP_NOSIZE);
}

pub fn resize(hwnd: HWND, w: i32, h: i32) {
    set_pos(hwnd, 0, 0, w, h, SWP_NOMOVE);
}
