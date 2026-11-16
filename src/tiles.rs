//! The file list: a native list view in Tiles view (keyboard, selection, checkboxes and
//! accessibility come from Windows) whose tiles are custom drawn: checkbox, thumbnail with a
//! status badge, bold file name, colored result line and a progress bar while optimizing.

use std::cell::RefCell;
use std::collections::HashMap;

use winapi::shared::minwindef::LRESULT;
use winapi::shared::windef::{HDC, HWND, RECT, SIZE};
use winapi::um::commctrl::*;
use winapi::um::sysinfoapi::GetTickCount64;
use winapi::um::uxtheme::{CloseThemeData, DrawThemeBackground, OpenThemeData};
use winapi::um::winuser::*;

use crate::draw;
use crate::icons::{self, px};
use crate::skin;
use crate::theme::{self, Palette, colorref};

/// Thumbnail edge in logical pixels.
pub const THUMB: i32 = 64;
/// Detail lines under the file name (list view columns 1..=3). The text lives in the list
/// view too, so screen readers and tooltips see it.
const LINES: u32 = 3;
const DETAIL_COLUMNS: [u32; LINES as usize] = [1, 2, 3];

/// Status badge drawn on a thumbnail's bottom-right corner.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Badge {
    None,
    Running,
    Done,
    Skipped,
    Error,
}

impl Badge {
    fn glyph(self) -> Option<(char, icons::Rgb)> {
        match self {
            Badge::None => None,
            Badge::Running => Some((icons::RUNNING, icons::ACCENT)),
            Badge::Done => Some((icons::DONE, icons::SUCCESS)),
            Badge::Skipped => Some((icons::SKIPPED, icons::CAUTION)),
            Badge::Error => Some((icons::ERROR, icons::CRITICAL)),
        }
    }
}

/// Color of a tile's result line.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Normal,
    Success,
    Caution,
    Critical,
}

/// What one tile shows.
#[derive(Clone)]
pub struct TileView {
    /// Name, details, output, location.
    pub texts: [String; 4],
    pub tone: Tone,
    pub running: bool,
    pub thumb: i32,
    pub badge: Badge,
    pub selected: bool,
}

/// Image list index of the placeholder thumbnail.
pub const PLACEHOLDER: i32 = 0;

pub struct Tiles {
    hwnd: HWND,
    images: HIMAGELIST,
    /// Physical thumbnail edge.
    size: i32,
    /// Pixels of every plain thumbnail by image index, to composite badges onto.
    pixels: RefCell<HashMap<i32, Vec<u8>>>,
    /// Thumbnail + badge composites already in the image list.
    composites: RefCell<HashMap<(i32, Badge), i32>>,
    badges: RefCell<HashMap<Badge, Vec<u8>>>,
    views: RefCell<Vec<TileView>>,
}

impl Tiles {
    /// Switch a report-style list view to Tiles view with checkboxes and thumbnails.
    pub fn new(hwnd: HWND) -> Self {
        let size = px(THUMB);
        unsafe {
            SendMessageW(hwnd, LVM_SETVIEW, LV_VIEW_TILE as usize, 0);
            let ex = LVS_EX_CHECKBOXES | LVS_EX_DOUBLEBUFFER;
            SendMessageW(hwnd, LVM_SETEXTENDEDLISTVIEWSTYLE, ex as usize, ex as isize);
            let images = ImageList_Create(size, size, ILC_COLOR32, 16, 64);
            SendMessageW(hwnd, LVM_SETIMAGELIST, LVSIL_NORMAL as usize, images as isize);
            let tiles = Tiles {
                hwnd,
                images,
                size,
                pixels: RefCell::default(),
                composites: RefCell::default(),
                badges: RefCell::default(),
                views: RefCell::default(),
            };
            tiles.add_thumbnail(&placeholder(size, &theme::palette()));
            tiles
        }
    }

    /// Re-color the list for the current palette.
    pub fn apply_palette(&self, p: &Palette) {
        unsafe {
            SendMessageW(self.hwnd, LVM_SETBKCOLOR, 0, colorref(p.list_bg) as isize);
            SendMessageW(self.hwnd, LVM_SETTEXTBKCOLOR, 0, colorref(p.list_bg) as isize);
            SendMessageW(self.hwnd, LVM_SETTEXTCOLOR, 0, colorref(p.fg) as isize);
            let icon = icons::icon_from_bgra(self.size, &placeholder(self.size, p));
            ImageList_ReplaceIcon(self.images, PLACEHOLDER, icon);
            DestroyIcon(icon);
        }
        self.pixels.borrow_mut().insert(PLACEHOLDER, placeholder(self.size, p));
        self.composites.borrow_mut().retain(|(thumb, _), _| *thumb != PLACEHOLDER);
        unsafe { InvalidateRect(self.hwnd, std::ptr::null(), 1) };
    }

    /// Drop all thumbnails (after the list is cleared).
    pub fn reset_images(&self) {
        unsafe { ImageList_Remove(self.images, -1) };
        self.pixels.borrow_mut().clear();
        self.composites.borrow_mut().clear();
        self.add_thumbnail(&placeholder(self.size, &theme::palette()));
    }

    /// Add a thumbnail (`size` x `size` straight-alpha BGRA); returns its image index.
    pub fn add_thumbnail(&self, bgra: &[u8]) -> i32 {
        let index = self.add_image(bgra);
        self.pixels.borrow_mut().insert(index, bgra.to_vec());
        index
    }

    fn add_image(&self, bgra: &[u8]) -> i32 {
        let icon = icons::icon_from_bgra(self.size, bgra);
        unsafe {
            let index = ImageList_ReplaceIcon(self.images, -1, icon);
            DestroyIcon(icon);
            index
        }
    }

    /// Image index of `thumb` with `badge` drawn in its corner (made once, then cached).
    fn image_with_badge(&self, thumb: i32, badge: Badge) -> i32 {
        let Some((glyph, color)) = badge.glyph() else { return thumb };
        if let Some(&index) = self.composites.borrow().get(&(thumb, badge)) {
            return index;
        }
        let Some(base) = self.pixels.borrow().get(&thumb).cloned() else { return thumb };
        let mut badges = self.badges.borrow_mut();
        let overlay = badges.entry(badge).or_insert_with(|| icons::badge_overlay(glyph, color, self.size));
        let mut out = base;
        for (dst, src) in out.as_chunks_mut::<4>().0.iter_mut().zip(overlay.as_chunks::<4>().0) {
            let a = src[3] as f32 / 255.0;
            if a == 0.0 {
                continue;
            }
            let da = dst[3] as f32 / 255.0;
            let oa = a + da * (1.0 - a);
            for c in 0..3 {
                dst[c] = ((src[c] as f32 * a + dst[c] as f32 * da * (1.0 - a)) / oa).round() as u8;
            }
            dst[3] = (oa * 255.0).round() as u8;
        }
        drop(badges);
        let index = self.add_image(&out);
        self.composites.borrow_mut().insert((thumb, badge), index);
        index
    }

    /// One tile per row: make tiles as wide as the list (call after resizing it).
    pub fn fit_width(&self) {
        unsafe {
            let mut rc: RECT = std::mem::zeroed();
            GetClientRect(self.hwnd, &mut rc);
            let width = (rc.right - rc.left - GetSystemMetrics(SM_CXVSCROLL) - px(4)).max(px(200));
            let mut info: LVTILEVIEWINFO = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<LVTILEVIEWINFO>() as u32;
            info.dwMask = LVTVIM_TILESIZE | LVTVIM_COLUMNS | LVTVIM_LABELMARGIN;
            info.dwFlags = LVTVIF_FIXEDSIZE;
            info.sizeTile = SIZE { cx: width, cy: px(THUMB + 16) };
            info.cLines = LINES as i32;
            info.rcLabelMargin = RECT { left: px(6), top: 0, right: px(4), bottom: 0 };
            SendMessageW(self.hwnd, LVM_SETTILEVIEWINFO, 0, &mut info as *mut _ as isize);
        }
    }

    /// Insert (when `row` == the current count) or replace a tile.
    pub fn set_tile(&self, row: usize, view: TileView, insert: bool) {
        {
            let mut views = self.views.borrow_mut();
            if insert {
                let at = row.min(views.len());
                views.insert(at, view.clone());
            } else if let Some(v) = views.get_mut(row) {
                *v = view.clone();
            }
        }
        let image = self.image_with_badge(view.thumb, view.badge);
        let mut columns = DETAIL_COLUMNS;
        let mut name: Vec<u16> = view.texts[0].encode_utf16().chain(Some(0)).collect();
        unsafe {
            let mut item: LVITEMW = std::mem::zeroed();
            item.iItem = row as i32;
            item.mask = LVIF_TEXT;
            item.pszText = name.as_mut_ptr();
            if insert {
                SendMessageW(self.hwnd, LVM_INSERTITEMW, 0, &mut item as *mut _ as isize);
            } else {
                SendMessageW(self.hwnd, LVM_SETITEMW, 0, &mut item as *mut _ as isize);
            }
            for (col, text) in view.texts.iter().enumerate().skip(1) {
                let mut wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
                let mut sub: LVITEMW = std::mem::zeroed();
                sub.mask = LVIF_TEXT;
                sub.iItem = row as i32;
                sub.iSubItem = col as i32;
                sub.pszText = wide.as_mut_ptr();
                SendMessageW(self.hwnd, LVM_SETITEMW, 0, &mut sub as *mut _ as isize);
            }
            let mut item: LVITEMW = std::mem::zeroed();
            item.mask = LVIF_IMAGE | LVIF_STATE | LVIF_COLUMNS;
            item.iItem = row as i32;
            item.iImage = image;
            item.state = index_to_state_image(if view.selected { 2 } else { 1 });
            item.stateMask = LVIS_STATEIMAGEMASK;
            item.cColumns = LINES;
            item.puColumns = columns.as_mut_ptr();
            SendMessageW(self.hwnd, LVM_SETITEMW, 0, &mut item as *mut _ as isize);
        }
    }

    pub fn clear(&self) {
        self.views.borrow_mut().clear();
        unsafe { SendMessageW(self.hwnd, LVM_DELETEALLITEMS, 0, 0) };
    }

    pub fn is_checked(&self, row: usize) -> bool {
        let state = unsafe { SendMessageW(self.hwnd, LVM_GETITEMSTATE, row, LVIS_STATEIMAGEMASK as isize) } as u32;
        (state >> 12) == 2
    }

    pub fn set_checked(&self, row: usize, checked: bool) {
        unsafe {
            let mut item: LVITEMW = std::mem::zeroed();
            item.state = index_to_state_image(if checked { 2 } else { 1 });
            item.stateMask = LVIS_STATEIMAGEMASK;
            SendMessageW(self.hwnd, LVM_SETITEMSTATE, row, &mut item as *mut _ as isize);
        }
    }


    /// Repaint tiles that show a moving progress bar.
    pub fn animate(&self) {
        let rows: Vec<usize> = self.views.borrow().iter().enumerate().filter(|(_, v)| v.running).map(|(i, _)| i).collect();
        for row in rows {
            let rc = self.item_rect(row, LVIR_BOUNDS);
            unsafe { InvalidateRect(self.hwnd, &rc, 0) };
        }
    }

    /// Our checkbox square (centered on the list view's state-image slot) and the
    /// thumbnail's left edge, for a tile.
    fn checkbox_and_thumb_x(&self, row: usize) -> (RECT, i32) {
        let bounds = self.item_rect(row, LVIR_BOUNDS);
        let icon = self.item_rect(row, LVIR_ICON);
        let (mut sw, mut sh) = (0, 0);
        unsafe {
            let states = SendMessageW(self.hwnd, LVM_GETIMAGELIST, LVSIL_STATE as usize, 0) as HIMAGELIST;
            if !states.is_null() {
                ImageList_GetIconSize(states, &mut sw, &mut sh);
            }
        }
        let size = px(16);
        let center_x = icon.left + sw / 2;
        let center_y = (bounds.top + bounds.bottom) / 2;
        let bx = draw::rect(center_x - size / 2, center_y - size / 2, center_x + size - size / 2, center_y + size - size / 2);
        (bx, icon.left + sw + px(4))
    }

    /// Debug check: does the list view hit-test our checkbox's center as the state icon?
    #[cfg(any(debug_assertions, feature = "snapshot"))]
    pub fn checkbox_hit_ok(&self, row: usize) -> bool {
        let (bx, _) = self.checkbox_and_thumb_x(row);
        unsafe {
            let mut info: LVHITTESTINFO = std::mem::zeroed();
            info.pt.x = (bx.left + bx.right) / 2;
            info.pt.y = (bx.top + bx.bottom) / 2;
            SendMessageW(self.hwnd, LVM_HITTEST, 0, &mut info as *mut _ as isize);
            info.iItem == row as i32 && info.flags & LVHT_ONITEMSTATEICON != 0
        }
    }

    fn item_rect(&self, row: usize, part: i32) -> RECT {
        let mut rc = RECT { left: part, top: 0, right: 0, bottom: 0 };
        unsafe { SendMessageW(self.hwnd, LVM_GETITEMRECT, row, &mut rc as *mut _ as isize) };
        rc
    }

    /// NM_CUSTOMDRAW from the list view: draw whole tiles ourselves.
    pub fn custom_draw(&self, lparam: isize) -> Option<LRESULT> {
        let cd = unsafe { &*(lparam as *const NMLVCUSTOMDRAW) };
        if cd.nmcd.hdr.hwndFrom != self.hwnd {
            return None;
        }
        match cd.nmcd.dwDrawStage {
            CDDS_PREPAINT => Some(CDRF_NOTIFYITEMDRAW),
            CDDS_ITEMPREPAINT => {
                let row = cd.nmcd.dwItemSpec;
                let views = self.views.try_borrow().ok()?;
                let view = views.get(row)?;
                self.paint_tile(cd.nmcd.hdc, row, view, cd.nmcd.uItemState);
                Some(CDRF_SKIPDEFAULT)
            }
            _ => None,
        }
    }

    fn paint_tile(&self, hdc: HDC, row: usize, view: &TileView, item_state: u32) {
        const CDIS_HOT: u32 = 0x40;
        const LVP_LISTITEM: i32 = 1;
        let p = theme::palette();
        let fonts = skin::fonts();
        let bounds = self.item_rect(row, LVIR_BOUNDS);
        let icon = self.item_rect(row, LVIR_ICON);
        draw::fill(hdc, &bounds, p.list_bg);

        // Hover / selection background from the list's own (light or dark) Explorer theme.
        let state = unsafe { SendMessageW(self.hwnd, LVM_GETITEMSTATE, row, LVIS_SELECTED as isize) } as u32;
        let selected = state & LVIS_SELECTED != 0;
        let hot = item_state & CDIS_HOT != 0;
        let focused = unsafe { GetFocus() } == self.hwnd;
        let part_state = match (selected, hot, focused) {
            (true, true, _) => 6,      // LISS_HOTSELECTED
            (true, false, true) => 3,  // LISS_SELECTED
            (true, false, false) => 5, // LISS_SELECTEDNOTFOCUS
            (false, true, _) => 2,     // LISS_HOT
            _ => 0,
        };
        if part_state != 0 {
            let class: Vec<u16> = "ListView\0".encode_utf16().collect();
            unsafe {
                let theme = OpenThemeData(self.hwnd, class.as_ptr());
                if theme.is_null() {
                    draw::fill(hdc, &bounds, draw::mix(p.accent, p.list_bg, 0.15));
                } else {
                    DrawThemeBackground(theme, hdc, LVP_LISTITEM, part_state, &bounds, std::ptr::null());
                    CloseThemeData(theme);
                }
            }
        }

        // Checkbox over the list view's own state-image slot (so clicks toggle it), then the
        // thumbnail, as the list view lays them out.
        let (bx, thumb_x) = self.checkbox_and_thumb_x(row);
        skin::draw_check_box(hdc, &bx, self.is_checked(row), false, false, true, p.list_bg);

        // Thumbnail (with its badge).
        let image = self.image_with_badge(view.thumb, view.badge);
        let iy = icon.top + ((icon.bottom - icon.top) - self.size) / 2;
        unsafe { ImageList_Draw(self.images, image, hdc, thumb_x, iy, ILD_TRANSPARENT) };

        // Text block, vertically centered, where the list view puts the label.
        let line = draw::line_height(hdc, fonts.body) + px(2);
        let label = self.item_rect(row, LVIR_LABEL);
        let x = label.left.max(thumb_x + self.size + px(10));
        let right = bounds.right - px(8);
        let mut y = (bounds.top + bounds.bottom) / 2 - line * 2;
        let tone = match view.tone {
            Tone::Normal => p.fg2,
            Tone::Success => p.success,
            Tone::Caution => p.caution,
            Tone::Critical => p.critical,
        };
        let lines = [
            (&view.texts[0], fonts.body_strong, p.fg),
            (&view.texts[1], fonts.body, p.fg2),
            (&view.texts[2], fonts.body, tone),
        ];
        for (text, font, color) in lines {
            draw::text(hdc, text, &draw::rect(x, y, right, y + line), font, color);
            y += line;
        }
        if view.running {
            // Indeterminate progress bar in place of the location line.
            let width = px(220).min(right - x);
            let h = px(3);
            let top = y + (line - h) / 2;
            let track = draw::rect(x, top, x + width, top + h);
            draw::round_rect(hdc, &track, h as f32 / 2.0, draw::mix(p.fg, p.list_bg, 0.12), None);
            let seg = width * 3 / 10;
            let t = unsafe { GetTickCount64() } as i64;
            let travel = (width + seg) as i64;
            let offset = ((t / 2) % travel) as i32 - seg;
            let (l, r) = ((x + offset).max(x), (x + offset + seg).min(x + width));
            if r > l {
                draw::round_rect(hdc, &draw::rect(l, top, r, top + h), h as f32 / 2.0, p.accent, None);
            }
        } else {
            draw::text(hdc, &view.texts[3], &draw::rect(x, y, right, y + line), fonts.body, p.fg3);
        }
    }
}

/// Placeholder thumbnail for the current palette.
fn placeholder(size: i32, p: &Palette) -> Vec<u8> {
    let tile = if p.dark { (0x33, 0x33, 0x33) } else { (0xF0, 0xF0, 0xF0) };
    let glyph = if p.dark { (0x80, 0x80, 0x80) } else { (0xA0, 0xA0, 0xA0) };
    icons::placeholder_thumb(size, tile, glyph)
}

fn index_to_state_image(i: u32) -> u32 {
    i << 12
}

/// True if a WM_NOTIFY from `list` reports a checkbox change.
///
/// # Safety
/// `lparam` must be the LPARAM of a WM_NOTIFY message.
pub unsafe fn is_check_change(list: HWND, lparam: isize) -> bool {
    unsafe {
        let hdr = &*(lparam as *const NMHDR);
        if hdr.hwndFrom != list || hdr.code != LVN_ITEMCHANGED {
            return false;
        }
        let nm = &*(lparam as *const NMLISTVIEW);
        nm.uChanged & LVIF_STATE != 0 && (nm.uNewState ^ nm.uOldState) & LVIS_STATEIMAGEMASK != 0
    }
}
