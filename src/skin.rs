//! Custom look for the Win32 controls: window background with settings cards, per-control
//! text/background colors, and custom-drawn checkboxes, sliders and the accent button.
//! Everything reads the current [`theme::palette`], so switching light/dark is a repaint.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use native_windows_gui as nwg;
use winapi::shared::minwindef::{LPARAM, LRESULT, WPARAM};
use winapi::shared::windef::{HBRUSH, HDC, HWND, RECT};
use winapi::um::commctrl::*;
use winapi::um::wingdi::{CreateSolidBrush, DeleteObject, SetBkColor, SetTextColor};
use winapi::um::winuser::*;

use crate::draw;
use crate::icons::px;
use crate::theme::{self, Fonts, Palette, Rgb, colorref};

/// What a control sits on, which decides its background color.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Surface {
    Window,
    Card,
    List,
    Field,
}

/// Text style of a static control.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Primary,
    Secondary,
    Tertiary,
    Heading,
    Title,
    /// Card titles (semibold body text).
    Strong,
    /// Card descriptions (small, secondary color).
    Caption,
}

#[derive(Clone, Copy)]
struct Style {
    surface: Surface,
    role: Role,
    dimmed: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Checkbox,
    /// A checkbox drawn as a Windows 11 toggle switch (`small` for nested options).
    Toggle { small: bool },
    Slider,
    AccentButton(char),
}

/// Extra shapes painted on the settings pane's background (content coordinates).
#[derive(Clone, Copy, PartialEq)]
pub enum Deco {
    /// Accent-tinted rounded square with an icon-font glyph.
    IconTile([i32; 4], char),
    /// 1px horizontal divider inside a card.
    Divider { left: i32, right: i32, y: i32 },
}

thread_local! {
    static STYLES: RefCell<HashMap<usize, Style>> = RefCell::default();
    static KINDS: RefCell<HashMap<usize, Kind>> = RefCell::default();
    static BRUSHES: RefCell<HashMap<u32, HBRUSH>> = RefCell::default();
    static CARDS: RefCell<Vec<RECT>> = const { RefCell::new(Vec::new()) };
    // The scrolling settings pane: its window, scroll offset, cards and decorations.
    static PANE: Cell<usize> = const { Cell::new(0) };
    static PANE_SCROLL: Cell<i32> = const { Cell::new(0) };
    static PANE_CARDS: RefCell<Vec<RECT>> = const { RefCell::new(Vec::new()) };
    static PANE_DECOS: RefCell<Vec<Deco>> = const { RefCell::new(Vec::new()) };
    static HIGHLIGHT: Cell<Option<RECT>> = const { Cell::new(None) };
    static DRAG_HOVER: Cell<bool> = const { Cell::new(false) };
    static FONTS: Cell<Option<&'static Fonts>> = const { Cell::new(None) };
}

pub fn init_fonts() -> &'static Fonts {
    let fonts: &'static Fonts = Box::leak(Box::new(theme::create_fonts()));
    FONTS.with(|f| f.set(Some(fonts)));
    fonts
}

pub fn fonts() -> &'static Fonts {
    FONTS.with(|f| f.get()).expect("skin::init_fonts not called")
}

fn key(hwnd: HWND) -> usize {
    hwnd as usize
}

/// Give a control a surface and text role.
pub fn style(hwnd: HWND, surface: Surface, role: Role) {
    STYLES.with(|s| s.borrow_mut().insert(key(hwnd), Style { surface, role, dimmed: false }));
}

/// Grey out a static's text without disabling it (disabled statics ignore text colors).
pub fn set_dimmed(hwnd: HWND, dimmed: bool) {
    let changed = STYLES.with(|s| {
        let mut s = s.borrow_mut();
        let entry = s.entry(key(hwnd)).or_insert(Style { surface: Surface::Window, role: Role::Primary, dimmed: false });
        let changed = entry.dimmed != dimmed;
        entry.dimmed = dimmed;
        changed
    });
    if changed {
        unsafe { InvalidateRect(hwnd, std::ptr::null(), 1) };
    }
}

pub fn checkbox(hwnd: HWND, surface: Surface) {
    style(hwnd, surface, Role::Primary);
    KINDS.with(|k| k.borrow_mut().insert(key(hwnd), Kind::Checkbox));
}

pub fn slider(hwnd: HWND, surface: Surface) {
    style(hwnd, surface, Role::Primary);
    KINDS.with(|k| k.borrow_mut().insert(key(hwnd), Kind::Slider));
}

pub fn toggle(hwnd: HWND, surface: Surface, small: bool) {
    style(hwnd, surface, Role::Primary);
    KINDS.with(|k| k.borrow_mut().insert(key(hwnd), Kind::Toggle { small }));
}

/// The scrolling settings pane window; its background is painted from [`set_pane_content`].
pub fn set_pane(hwnd: HWND) {
    PANE.with(|p| p.set(key(hwnd)));
}

pub fn set_pane_scroll(y: i32) {
    PANE_SCROLL.with(|s| s.set(y));
}

/// Cards and decorations of the settings pane (logical content coordinates).
/// Returns whether anything changed (so the caller knows to repaint).
pub fn set_pane_content(cards: Vec<RECT>, decos: Vec<Deco>) -> bool {
    let same_cards = PANE_CARDS.with(|c| {
        let c = c.borrow();
        c.len() == cards.len()
            && c.iter().zip(&cards).all(|(a, b)| (a.left, a.top, a.right, a.bottom) == (b.left, b.top, b.right, b.bottom))
    });
    let same_decos = PANE_DECOS.with(|d| {
        let d = d.borrow();
        d.len() == decos.len()
            && d.iter().zip(&decos).all(|(a, b)| a == b)
    });
    PANE_CARDS.with(|c| *c.borrow_mut() = cards);
    PANE_DECOS.with(|d| *d.borrow_mut() = decos);
    !(same_cards && same_decos)
}

fn paint_pane(pane: HWND, hdc: HDC) {
    let p = theme::palette();
    let scroll = PANE_SCROLL.with(|s| s.get());
    let mut client: RECT = unsafe { std::mem::zeroed() };
    unsafe {
        GetClientRect(pane, &mut client);
        FillRect(hdc, &client, brush(p.bg));
    }
    let mut clip: RECT = unsafe { std::mem::zeroed() };
    unsafe { winapi::um::wingdi::GetClipBox(hdc, &mut clip) };
    let touches = |r: &RECT| r.left < clip.right && r.right > clip.left && r.top < clip.bottom && r.bottom > clip.top;
    let shift = |r: &RECT| to_physical(&RECT { left: r.left, top: r.top - scroll, right: r.right, bottom: r.bottom - scroll });
    PANE_CARDS.with(|cards| {
        for card in cards.borrow().iter().map(shift).filter(|r| touches(r)) {
            draw::round_rect(hdc, &card, px(6) as f32, p.card, Some((p.card_border, 1.0)));
        }
    });
    PANE_DECOS.with(|decos| {
        for deco in decos.borrow().iter() {
            match *deco {
                Deco::IconTile([l, t, r_, b], glyph) => {
                    let r = shift(&RECT { left: l, top: t, right: r_, bottom: b });
                    if touches(&r) {
                        let tint = draw::mix(p.accent, p.card, if p.dark { 0.20 } else { 0.14 });
                        draw::round_rect(hdc, &r, px(6) as f32, tint, None);
                        draw::text_centered(hdc, &glyph.to_string(), &r, fonts().icon, p.accent);
                    }
                }
                Deco::Divider { left, right, y } => {
                    let r = shift(&RECT { left, top: y, right, bottom: y });
                    let line = draw::rect(r.left, r.top, r.right, r.top + 1);
                    if touches(&line) {
                        draw::fill(hdc, &line, if p.dark { (0x1D, 0x1D, 0x1D) } else { (0xEA, 0xEA, 0xEA) });
                    }
                }
            }
        }
    });
}

pub fn accent_button(hwnd: HWND, glyph: char) {
    style(hwnd, Surface::Window, Role::Primary);
    KINDS.with(|k| k.borrow_mut().insert(key(hwnd), Kind::AccentButton(glyph)));
}

/// The file list's frame (logical pixels), painted on the window background.
pub fn set_highlight(rect: Option<RECT>) {
    HIGHLIGHT.with(|h| h.set(rect));
}

pub fn surface_color(surface: Surface, p: &Palette) -> Rgb {
    match surface {
        Surface::Window => p.bg,
        Surface::Card => p.card,
        Surface::List => p.list_bg,
        Surface::Field => p.field,
    }
}

fn role_color(role: Role, p: &Palette) -> Rgb {
    match role {
        Role::Primary | Role::Heading | Role::Title | Role::Strong => p.fg,
        Role::Caption => p.fg2,
        Role::Secondary => p.fg2,
        Role::Tertiary => p.fg3,
    }
}

pub fn role_font(role: Role) -> winapi::shared::windef::HFONT {
    let f = fonts();
    match role {
        Role::Heading => f.heading,
        Role::Strong => f.body_strong,
        Role::Caption => f.caption,
        Role::Title => f.title,
        _ => f.body,
    }
}

/// A cached solid brush (freed on [`reset_brushes`]).
pub fn brush(color: Rgb) -> HBRUSH {
    let c = colorref(color);
    BRUSHES.with(|b| *b.borrow_mut().entry(c).or_insert_with(|| unsafe { CreateSolidBrush(c) }))
}

/// Free cached brushes (after the theme changes).
pub fn reset_brushes() {
    BRUSHES.with(|b| {
        for (_, brush) in b.borrow_mut().drain() {
            unsafe { DeleteObject(brush as _) };
        }
    });
}

fn to_physical(rc: &RECT) -> RECT {
    draw::rect(px(rc.left), px(rc.top), px(rc.right), px(rc.bottom))
}

// ------------------------------------------------------------------ message handling

/// Handle painting-related messages sent to the main window (or to the list view, for its
/// child statics). Returns `Some` when the message was handled.
pub fn handle(window: HWND, hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> Option<LRESULT> {
    let pane = PANE.with(|p| p.get()) as HWND;
    match msg {
        // The settings pane paints its own background (cards, icon tiles, dividers).
        WM_ERASEBKGND if hwnd == pane => Some(1),
        WM_PRINTCLIENT if hwnd == pane => {
            paint_pane(hwnd, w as HDC);
            Some(0)
        }
        WM_PAINT if hwnd == pane => unsafe {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);
            paint_pane(hwnd, hdc);
            EndPaint(hwnd, &ps);
            Some(0)
        },
        // WM_PAINT paints the whole background, so there's nothing to erase first.
        WM_ERASEBKGND if hwnd == window => Some(1),
        // Themed buttons ask for the background behind their rounded corners this way.
        WM_PRINTCLIENT if hwnd == window => {
            paint_background(hwnd, w as HDC);
            Some(0)
        }
        // Paint the background on every repaint, not only when Windows asks for an erase: an
        // erase handled before our handlers were attached would otherwise leave the default
        // background until the next resize. The window clips its children, so this never
        // paints over controls.
        WM_PAINT if hwnd == window => unsafe {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);
            paint_background(hwnd, hdc);
            EndPaint(hwnd, &ps);
            Some(0)
        },
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
            Some(ctl_color(msg, w as HDC, l as HWND))
        }
        WM_NOTIFY => {
            let hdr = unsafe { &*(l as *const NMHDR) };
            if hdr.code != NM_CUSTOMDRAW {
                return None;
            }
            let kind = KINDS.with(|k| k.borrow().get(&key(hdr.hwndFrom)).copied())?;
            let cd = unsafe { &*(l as *const NMCUSTOMDRAW) };
            Some(match kind {
                Kind::Checkbox => draw_checkbox(cd),
                Kind::Slider => draw_slider(cd),
                Kind::AccentButton(glyph) => draw_accent_button(cd, glyph),
                Kind::Toggle { small } => draw_toggle(cd, small),
            })
        }
        _ => None,
    }
}

fn ctl_color(msg: u32, hdc: HDC, control: HWND) -> LRESULT {
    let p = theme::palette();
    let style = STYLES.with(|s| s.borrow().get(&key(control)).copied());
    let (surface, role, dimmed) = match (msg, style) {
        (WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX, _) => (Surface::Field, Role::Primary, false),
        (_, Some(s)) => (s.surface, s.role, s.dimmed),
        (_, None) => (Surface::Window, Role::Primary, false),
    };
    let bg = surface_color(surface, &p);
    let fg = if dimmed { p.fg3 } else { role_color(role, &p) };
    unsafe {
        SetBkColor(hdc, colorref(bg));
        SetTextColor(hdc, colorref(fg));
    }
    brush(bg) as LRESULT
}

fn paint_background(window: HWND, hdc: HDC) {
    let p = theme::palette();
    unsafe {
        let mut rc: RECT = std::mem::zeroed();
        GetClientRect(window, &mut rc);
        FillRect(hdc, &rc, brush(p.bg));
    }
    // Only shapes that touch the area being repainted.
    let mut clip: RECT = unsafe { std::mem::zeroed() };
    unsafe { winapi::um::wingdi::GetClipBox(hdc, &mut clip) };
    let touches = |r: &RECT| r.left < clip.right && r.right > clip.left && r.top < clip.bottom && r.bottom > clip.top;
    CARDS.with(|cards| {
        for card in cards.borrow().iter().map(to_physical).filter(|r| touches(r)) {
            draw::round_rect(hdc, &card, px(6) as f32, p.card, Some((p.card_border, 1.0)));
        }
    });
    // Rounded frame around the file list (the list view itself has no border); accent while
    // files are dragged over the window.
    if let Some(rc) = HIGHLIGHT.with(|h| h.get()).filter(|r| touches(&to_physical(r))) {
        let frame = to_physical(&rc);
        let border = if DRAG_HOVER.with(|d| d.get()) { (p.accent, px(2) as f32) } else { (p.card_border, 1.0) };
        draw::round_rect(hdc, &frame, px(6) as f32, p.list_bg, Some(border));
    }
}

/// Show the list frame in the accent color (files are being dragged over the window).
pub fn set_drag_hover(on: bool) {
    DRAG_HOVER.with(|d| d.set(on));
}

/// Whether a control is drawn greyed out (see [`set_dimmed`]).
pub fn is_dimmed(hwnd: HWND) -> bool {
    STYLES.with(|s| s.borrow().get(&key(hwnd)).is_some_and(|s| s.dimmed))
}

fn surface_of(hwnd: HWND) -> Surface {
    STYLES.with(|s| s.borrow().get(&key(hwnd)).map(|s| s.surface)).unwrap_or(Surface::Window)
}

fn window_text(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }
}

const CDIS_SELECTED: u32 = 0x0001;
const CDIS_FOCUS: u32 = 0x0010;
const CDIS_HOT: u32 = 0x0040;
const CDIS_SHOWKEYBOARDCUES: u32 = 0x0200;

/// A Windows 11 style checkbox square (also used on file tiles).
pub fn draw_check_box(hdc: HDC, bx: &RECT, checked: bool, hot: bool, pressed: bool, enabled: bool, surface: Rgb) {
    let p = theme::palette();
    let radius = px(4) as f32;
    if checked {
        let fill = if !enabled {
            p.disabled_fill
        } else if pressed {
            p.accent_pressed
        } else if hot {
            p.accent_hover
        } else {
            p.accent
        };
        draw::round_rect(hdc, bx, radius, fill, None);
        let mark = if enabled { p.on_accent } else { p.disabled_text };
        draw::text_centered(hdc, "\u{E73E}", bx, fonts().icon_small, mark);
    } else {
        let tint = if pressed { 0.10 } else if hot { 0.06 } else { 0.02 };
        let fill = draw::mix(p.fg, surface, tint);
        let border = if enabled { p.control_strong } else { draw::mix(p.fg3, surface, 0.5) };
        draw::round_rect(hdc, bx, radius, fill, Some((border, 1.0)));
    }
}

fn draw_checkbox(cd: &NMCUSTOMDRAW) -> LRESULT {
    if cd.dwDrawStage != CDDS_PREPAINT {
        return CDRF_DODEFAULT as LRESULT;
    }
    let p = theme::palette();
    let hwnd = cd.hdr.hwndFrom;
    let hdc = cd.hdc;
    let surface = surface_color(surface_of(hwnd), &p);
    let rc = cd.rc;
    unsafe { FillRect(hdc, &rc, brush(surface)) };
    let enabled = unsafe { IsWindowEnabled(hwnd) } != 0;
    let checked = unsafe { SendMessageW(hwnd, BM_GETCHECK, 0, 0) } == BST_CHECKED as isize;
    let state = cd.uItemState;
    let size = px(16);
    let top = rc.top + (rc.bottom - rc.top - size) / 2;
    let bx = draw::rect(rc.left + px(1), top, rc.left + px(1) + size, top + size);
    draw_check_box(hdc, &bx, checked, state & CDIS_HOT != 0, state & CDIS_SELECTED != 0, enabled, surface);
    let text_rc = draw::rect(bx.right + px(8), rc.top, rc.right, rc.bottom);
    let text = window_text(hwnd);
    draw::text(hdc, &text, &text_rc, fonts().body, if enabled { p.fg } else { p.fg3 });
    if state & CDIS_FOCUS != 0 && state & CDIS_SHOWKEYBOARDCUES != 0 {
        let w = draw::text_width(hdc, &text, fonts().body);
        let h = draw::line_height(hdc, fonts().body);
        let mid = (rc.top + rc.bottom) / 2;
        let focus = draw::rect(text_rc.left - px(2), mid - h / 2 - 1, text_rc.left + w + px(2), mid + h / 2 + 1);
        unsafe { DrawFocusRect(hdc, &focus) };
    }
    CDRF_SKIPDEFAULT as LRESULT
}

/// A Windows 11 toggle switch, right-aligned in the control.
fn draw_toggle(cd: &NMCUSTOMDRAW, small: bool) -> LRESULT {
    if cd.dwDrawStage != CDDS_PREPAINT {
        return CDRF_DODEFAULT as LRESULT;
    }
    let p = theme::palette();
    let hwnd = cd.hdr.hwndFrom;
    let hdc = cd.hdc;
    let rc = cd.rc;
    let surface = surface_color(surface_of(hwnd), &p);
    unsafe { FillRect(hdc, &rc, brush(surface)) };
    let enabled = unsafe { IsWindowEnabled(hwnd) } != 0;
    let on = unsafe { SendMessageW(hwnd, BM_GETCHECK, 0, 0) } == BST_CHECKED as isize;
    let state = cd.uItemState;
    let (hot, pressed) = (state & CDIS_HOT != 0, state & CDIS_SELECTED != 0);

    let (w, h) = if small { (px(32), px(16)) } else { (px(40), px(20)) };
    let right = rc.right - px(2);
    let top = (rc.top + rc.bottom - h) / 2;
    let pill = draw::rect(right - w, top, right, top + h);
    let r = h as f32 / 2.0;
    let knob_r = (h as f32 / 2.0 - px(4) as f32 + if hot && enabled { px(1) as f32 } else { 0.0 }).max(2.0);
    let cy = (pill.top + pill.bottom) as f32 / 2.0;
    if on {
        let fill = if !enabled {
            p.disabled_fill
        } else if pressed {
            p.accent_pressed
        } else if hot {
            p.accent_hover
        } else {
            p.accent
        };
        draw::round_rect(hdc, &pill, r, fill, None);
        let cx = pill.right as f32 - r;
        draw::circle(hdc, cx, cy, knob_r, if enabled { p.on_accent } else { p.disabled_text }, None);
    } else {
        let tint = if pressed { 0.10 } else if hot { 0.06 } else { 0.02 };
        let border = if enabled { p.control_strong } else { draw::mix(p.fg3, surface, 0.5) };
        draw::round_rect(hdc, &pill, r, draw::mix(p.fg, surface, tint), Some((border, 1.0)));
        let cx = pill.left as f32 + r;
        draw::circle(hdc, cx, cy, knob_r, if enabled { p.fg2 } else { p.fg3 }, None);
    }
    if state & CDIS_FOCUS != 0 && state & CDIS_SHOWKEYBOARDCUES != 0 {
        let mut focus = pill;
        unsafe {
            InflateRect(&mut focus, px(2), px(2));
            DrawFocusRect(hdc, &focus);
        }
    }
    CDRF_SKIPDEFAULT as LRESULT
}

fn draw_slider(cd: &NMCUSTOMDRAW) -> LRESULT {
    const TBCD_TICS: usize = 1;
    const TBCD_THUMB: usize = 2;
    const TBCD_CHANNEL: usize = 3;
    match cd.dwDrawStage {
        CDDS_PREPAINT => return CDRF_NOTIFYITEMDRAW as LRESULT,
        CDDS_ITEMPREPAINT => {}
        _ => return CDRF_DODEFAULT as LRESULT,
    }
    let p = theme::palette();
    let hwnd = cd.hdr.hwndFrom;
    let hdc = cd.hdc;
    // Sliders are never really disabled (Windows skips custom drawing for disabled
    // trackbars); a format that's off just dims its slider.
    let enabled = unsafe { IsWindowEnabled(hwnd) } != 0 && !is_dimmed(hwnd);
    let mut thumb: RECT = unsafe { std::mem::zeroed() };
    unsafe { SendMessageW(hwnd, TBM_GETTHUMBRECT, 0, &mut thumb as *mut _ as isize) };
    let (tx, ty) = ((thumb.left + thumb.right) as f32 / 2.0, (thumb.top + thumb.bottom) as f32 / 2.0);
    match cd.dwItemSpec {
        TBCD_TICS => {}
        TBCD_CHANNEL => {
            let mut client: RECT = unsafe { std::mem::zeroed() };
            unsafe { GetClientRect(hwnd, &mut client) };
            let pad = px(10);
            let h = px(4);
            let top = ty.round() as i32 - h / 2;
            let rail = draw::rect(client.left + pad, top, client.right - pad, top + h);
            let surface = surface_color(surface_of(hwnd), &p);
            let rail_color = if enabled { p.control_strong } else { draw::mix(p.fg3, surface, 0.5) };
            draw::round_rect(hdc, &rail, h as f32 / 2.0, rail_color, None);
            let filled = draw::rect(rail.left, rail.top, tx.round() as i32, rail.bottom);
            draw::round_rect(hdc, &filled, h as f32 / 2.0, if enabled { p.accent } else { p.fg3 }, None);
        }
        TBCD_THUMB => {
            let outer = px(10) as f32;
            let ring = if p.dark { (0x45, 0x45, 0x45) } else { (0xFF, 0xFF, 0xFF) };
            let edge = if p.dark { (0x30, 0x30, 0x30) } else { (0xD8, 0xD8, 0xD8) };
            draw::circle(hdc, tx, ty, outer, ring, Some((edge, 1.0)));
            let state = cd.uItemState;
            let inner = if state & CDIS_SELECTED != 0 {
                px(5)
            } else if state & CDIS_HOT != 0 {
                px(7)
            } else {
                px(6)
            } as f32;
            draw::circle(hdc, tx, ty, inner, if enabled { p.accent } else { p.fg3 }, None);
        }
        _ => return CDRF_DODEFAULT as LRESULT,
    }
    CDRF_SKIPDEFAULT as LRESULT
}

fn draw_accent_button(cd: &NMCUSTOMDRAW, glyph: char) -> LRESULT {
    if cd.dwDrawStage != CDDS_PREPAINT {
        return CDRF_DODEFAULT as LRESULT;
    }
    let p = theme::palette();
    let hwnd = cd.hdr.hwndFrom;
    let hdc = cd.hdc;
    let rc = cd.rc;
    unsafe { FillRect(hdc, &rc, brush(surface_color(surface_of(hwnd), &p))) };
    let enabled = unsafe { IsWindowEnabled(hwnd) } != 0;
    let state = cd.uItemState;
    let fill = if !enabled {
        p.disabled_fill
    } else if state & CDIS_SELECTED != 0 {
        p.accent_pressed
    } else if state & CDIS_HOT != 0 {
        p.accent_hover
    } else {
        p.accent
    };
    draw::round_rect(hdc, &rc, px(4) as f32, fill, None);
    let fg = if enabled { p.on_accent } else { p.disabled_text };
    let f = fonts();
    let text = window_text(hwnd);
    let glyph_s = glyph.to_string();
    let (gw, tw, gap) = (draw::text_width(hdc, &glyph_s, f.icon), draw::text_width(hdc, &text, f.body_strong), px(8));
    let x = rc.left + ((rc.right - rc.left) - (gw + gap + tw)) / 2;
    draw::text(hdc, &glyph_s, &draw::rect(x, rc.top, x + gw, rc.bottom), f.icon, fg);
    draw::text(hdc, &text, &draw::rect(x + gw + gap, rc.top, rc.right, rc.bottom), f.body_strong, fg);
    if state & CDIS_FOCUS != 0 && state & CDIS_SHOWKEYBOARDCUES != 0 {
        let mut focus = rc;
        unsafe {
            InflateRect(&mut focus, -px(3), -px(3));
            DrawFocusRect(hdc, &focus);
        }
    }
    CDRF_SKIPDEFAULT as LRESULT
}

// ------------------------------------------------------------------ static text

/// A plain Win32 static text control styled through the skin.
#[derive(Default)]
pub struct Text {
    pub handle: nwg::ControlHandle,
}

#[derive(Clone, Copy)]
pub enum Align {
    Left,
    Right,
    Center,
}

impl Text {
    pub fn new(parent: nwg::ControlHandle, text: &str, surface: Surface, role: Role, align: Align) -> Result<Text, nwg::NwgError> {
        let align = match align {
            Align::Left => SS_LEFT,
            Align::Right => SS_RIGHT,
            Align::Center => SS_CENTER,
        };
        let handle = nwg::ControlBase::build_hwnd()
            .class_name("STATIC")
            .text(text)
            .size((10, 10))
            .flags(WS_CHILD | WS_VISIBLE | SS_NOPREFIX | SS_ENDELLIPSIS | align)
            .parent(Some(parent))
            .build()?;
        if let Some(hwnd) = handle.hwnd() {
            style(hwnd, surface, role);
            unsafe { SendMessageW(hwnd, WM_SETFONT, role_font(role) as usize, 1) };
        }
        Ok(Text { handle })
    }

    pub fn hwnd(&self) -> HWND {
        self.handle.hwnd().unwrap_or(std::ptr::null_mut())
    }

    /// Position in logical pixels (relative to the parent).
    pub fn place(&self, x: i32, y: i32, w: i32, h: i32) {
        crate::geom::place(self.hwnd(), x, y, w, h);
    }

    pub fn text(&self) -> String {
        window_text(self.hwnd())
    }

    pub fn set_text(&self, text: &str) {
        if self.text() != text {
            let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
            unsafe { SetWindowTextW(self.hwnd(), wide.as_ptr()) };
        }
    }

    pub fn set_visible(&self, visible: bool) {
        if self.visible() != visible {
            unsafe { ShowWindow(self.hwnd(), if visible { SW_SHOW } else { SW_HIDE }) };
        }
    }

    /// The control's own visible flag (true even while its parent window is hidden).
    pub fn visible(&self) -> bool {
        shown(self.hwnd())
    }

    pub fn set_dimmed(&self, dimmed: bool) {
        set_dimmed(self.hwnd(), dimmed);
    }
}

/// Whether a control has its own WS_VISIBLE flag set. Unlike `IsWindowVisible`, this is
/// independent of whether its parent window is currently shown.
pub fn shown(hwnd: HWND) -> bool {
    unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) & WS_VISIBLE as isize != 0 }
}

/// native-windows-gui's text boxes center their text by padding the frame, and paint that
/// padding with the light system window color. Repaint it with the current field color
/// (call after the library has painted the frame).
pub fn repaint_field_padding(hwnd: HWND) {
    unsafe {
        let mut window: RECT = std::mem::zeroed();
        let mut client: RECT = std::mem::zeroed();
        GetWindowRect(hwnd, &mut window);
        GetClientRect(hwnd, &mut client);
        let mut top_left = winapi::shared::windef::POINT { x: window.left, y: window.top };
        let mut bottom_right = winapi::shared::windef::POINT { x: window.right, y: window.bottom };
        ScreenToClient(hwnd, &mut top_left);
        ScreenToClient(hwnd, &mut bottom_right);
        let top = RECT { left: 0, top: top_left.y, right: client.right, bottom: client.top };
        let bottom = RECT { left: 0, top: client.bottom, right: client.right, bottom: bottom_right.y };
        let dc = GetDC(hwnd);
        let fill = brush(theme::palette().field);
        FillRect(dc, &top, fill);
        FillRect(dc, &bottom, fill);
        ReleaseDC(hwnd, dc);
    }
}
