//! Light/dark palette that follows Windows, the system accent color, dark-mode support for
//! Win32 controls, the tinted title bar, and fonts.

use std::cell::Cell;
use std::sync::OnceLock;

use winapi::shared::minwindef::{BOOL, DWORD};
use winapi::shared::windef::{HFONT, HWND};
use winapi::um::winuser::{SendMessageW, WM_THEMECHANGED};

pub type Rgb = (u8, u8, u8);

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub dark: bool,
    /// Window background.
    pub bg: Rgb,
    /// Settings cards.
    pub card: Rgb,
    pub card_border: Rgb,
    /// File list background.
    pub list_bg: Rgb,
    /// Text fields and dropdowns.
    pub field: Rgb,
    pub fg: Rgb,
    pub fg2: Rgb,
    pub fg3: Rgb,
    pub accent: Rgb,
    pub accent_hover: Rgb,
    pub accent_pressed: Rgb,
    /// Text and glyphs on an accent fill.
    pub on_accent: Rgb,
    pub disabled_fill: Rgb,
    pub disabled_text: Rgb,
    /// Slider rail, unchecked checkbox border.
    pub control_strong: Rgb,
    pub success: Rgb,
    pub caution: Rgb,
    pub critical: Rgb,
}

impl Palette {
    fn new(dark: bool, accent: Rgb) -> Self {
        let bg = if dark { (0x20, 0x20, 0x20) } else { (0xF3, 0xF3, 0xF3) };
        let mix = |a: Rgb, b: Rgb, t: f32| {
            let m = |x: u8, y: u8| (x as f32 * t + y as f32 * (1.0 - t)).round() as u8;
            (m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
        };
        if dark {
            Palette {
                dark,
                bg,
                card: (0x2B, 0x2B, 0x2B),
                card_border: (0x1D, 0x1D, 0x1D),
                list_bg: (0x27, 0x27, 0x27),
                field: (0x2D, 0x2D, 0x2D),
                fg: (0xFF, 0xFF, 0xFF),
                fg2: (0xC5, 0xC5, 0xC5),
                fg3: (0x8B, 0x8B, 0x8B),
                accent,
                accent_hover: mix(accent, bg, 0.9),
                accent_pressed: mix(accent, bg, 0.8),
                on_accent: (0x00, 0x00, 0x00),
                disabled_fill: (0x43, 0x43, 0x43),
                disabled_text: (0xA0, 0xA0, 0xA0),
                control_strong: (0x9A, 0x9A, 0x9A),
                success: (0x6C, 0xCB, 0x5F),
                caution: (0xFC, 0xE1, 0x00),
                critical: (0xFF, 0x99, 0xA4),
            }
        } else {
            Palette {
                dark,
                bg,
                card: (0xFB, 0xFB, 0xFB),
                card_border: (0xE5, 0xE5, 0xE5),
                list_bg: (0xFF, 0xFF, 0xFF),
                field: (0xFF, 0xFF, 0xFF),
                fg: (0x1A, 0x1A, 0x1A),
                fg2: (0x5F, 0x5F, 0x5F),
                fg3: (0x8A, 0x8A, 0x8A),
                accent,
                accent_hover: mix(accent, bg, 0.9),
                accent_pressed: mix(accent, bg, 0.8),
                on_accent: (0xFF, 0xFF, 0xFF),
                disabled_fill: (0xC5, 0xC5, 0xC5),
                disabled_text: (0xFF, 0xFF, 0xFF),
                control_strong: (0x86, 0x86, 0x86),
                success: (0x0F, 0x7B, 0x0F),
                caution: (0x9D, 0x5D, 0x00),
                critical: (0xC4, 0x2B, 0x1C),
            }
        }
    }
}

thread_local! {
    static PALETTE: Cell<Palette> = Cell::new(Palette::new(false, (0x00, 0x5F, 0xB8)));
}

/// The current palette (UI thread).
pub fn palette() -> Palette {
    PALETTE.with(|p| p.get())
}

/// Re-read Windows' light/dark setting and accent color; returns the new palette.
pub fn refresh() -> Palette {
    let dark = system_dark();
    let p = Palette::new(dark, accent_color(dark));
    PALETTE.with(|c| c.set(p));
    p
}

pub fn colorref(c: Rgb) -> DWORD {
    c.0 as DWORD | (c.1 as DWORD) << 8 | (c.2 as DWORD) << 16
}

/// `GIO_THEME=dark|light` overrides Windows' app theme (handy for testing).
fn system_dark() -> bool {
    match std::env::var("GIO_THEME").as_deref() {
        Ok("dark") => return true,
        Ok("light") => return false,
        _ => {}
    }
    read_dword(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize", "AppsUseLightTheme") == Some(0)
}

/// Windows' accent color in the shade Windows 11 uses for buttons in this theme
/// (AccentDark1 on light, AccentLight2 on dark).
fn accent_color(dark: bool) -> Rgb {
    let fallback = if dark { (0x4C, 0xC2, 0xFF) } else { (0x00, 0x5F, 0xB8) };
    let Some(bytes) = read_binary(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent", "AccentPalette") else {
        return fallback;
    };
    // Eight RGBA entries: Light3, Light2, Light1, Base, Dark1, Dark2, Dark3, (unused).
    let entry = if dark { 1 } else { 4 };
    match bytes.get(entry * 4..entry * 4 + 3) {
        Some(c) => (c[0], c[1], c[2]),
        None => fallback,
    }
}

fn read_value(path: &str, name: &str) -> Option<(DWORD, Vec<u8>)> {
    use winapi::um::winreg::{HKEY_CURRENT_USER, RRF_RT_ANY, RegGetValueW};
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let (path, name) = (wide(path), wide(name));
    let mut kind: DWORD = 0;
    let mut size: DWORD = 0;
    unsafe {
        let ok = RegGetValueW(HKEY_CURRENT_USER, path.as_ptr(), name.as_ptr(), RRF_RT_ANY, &mut kind, std::ptr::null_mut(), &mut size);
        if ok != 0 || size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        let ok = RegGetValueW(
            HKEY_CURRENT_USER,
            path.as_ptr(),
            name.as_ptr(),
            RRF_RT_ANY,
            &mut kind,
            data.as_mut_ptr() as _,
            &mut size,
        );
        (ok == 0).then_some((kind, data))
    }
}

fn read_dword(path: &str, name: &str) -> Option<u32> {
    let (_, data) = read_value(path, name)?;
    Some(u32::from_le_bytes(data.get(..4)?.try_into().ok()?))
}

fn read_binary(path: &str, name: &str) -> Option<Vec<u8>> {
    read_value(path, name).map(|(_, data)| data)
}

// ------------------------------------------------------------------ dark mode for Win32 controls

// Undocumented uxtheme exports (by ordinal) that Windows' own apps use for dark Win32 controls.
type SetPreferredAppMode = unsafe extern "system" fn(i32) -> i32;
type AllowDarkModeForWindow = unsafe extern "system" fn(HWND, BOOL) -> BOOL;
type RefreshImmersiveColorPolicyState = unsafe extern "system" fn();
type FlushMenuThemes = unsafe extern "system" fn();

struct DarkApi {
    set_preferred_app_mode: Option<SetPreferredAppMode>,
    allow_dark_mode_for_window: Option<AllowDarkModeForWindow>,
    refresh_policy: Option<RefreshImmersiveColorPolicyState>,
    flush_menu_themes: Option<FlushMenuThemes>,
}

fn dark_api() -> &'static DarkApi {
    static API: OnceLock<DarkApi> = OnceLock::new();
    API.get_or_init(|| unsafe {
        use winapi::um::libloaderapi::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
        let name: Vec<u16> = "uxtheme.dll\0".encode_utf16().collect();
        let lib = LoadLibraryExW(name.as_ptr(), std::ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32);
        let ordinal = |n: usize| if lib.is_null() { std::ptr::null_mut() } else { GetProcAddress(lib, n as *const i8) };
        let f = |n| {
            let p = ordinal(n);
            (!p.is_null()).then_some(p)
        };
        DarkApi {
            set_preferred_app_mode: f(135).map(|p| std::mem::transmute::<_, SetPreferredAppMode>(p)),
            allow_dark_mode_for_window: f(133).map(|p| std::mem::transmute::<_, AllowDarkModeForWindow>(p)),
            refresh_policy: f(104).map(|p| std::mem::transmute::<_, RefreshImmersiveColorPolicyState>(p)),
            flush_menu_themes: f(136).map(|p| std::mem::transmute::<_, FlushMenuThemes>(p)),
        }
    })
}

/// Tell Windows this app can draw dark Win32 controls. Call once before creating windows.
pub fn init_dark_mode(dark: bool) {
    let api = dark_api();
    unsafe {
        if let Some(f) = api.set_preferred_app_mode {
            // 2 = ForceDark, 3 = ForceLight: follow our own detection (which honors GIO_THEME).
            f(if dark { 2 } else { 3 });
        }
        if let Some(f) = api.refresh_policy {
            f();
        }
        if let Some(f) = api.flush_menu_themes {
            f();
        }
    }
}

/// How a control should be themed.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ControlKind {
    /// Buttons, checkboxes, list views, tooltips, scrollbars.
    Explorer,
    /// Edit boxes and dropdowns.
    Field,
}

/// Apply the light or dark visual style to one control.
pub fn theme_control(hwnd: HWND, kind: ControlKind, dark: bool) {
    use winapi::um::uxtheme::SetWindowTheme;
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let name = match (kind, dark) {
        (ControlKind::Explorer, true) => Some("DarkMode_Explorer"),
        (ControlKind::Explorer, false) => Some("Explorer"),
        (ControlKind::Field, true) => Some("DarkMode_CFD"),
        (ControlKind::Field, false) => None,
    };
    unsafe {
        if let Some(f) = dark_api().allow_dark_mode_for_window {
            f(hwnd, dark as BOOL);
        }
        match name {
            Some(n) => {
                let n = wide(n);
                SetWindowTheme(hwnd, n.as_ptr(), std::ptr::null());
            }
            None => {
                SetWindowTheme(hwnd, std::ptr::null(), std::ptr::null());
            }
        }
        SendMessageW(hwnd, WM_THEMECHANGED, 0, 0);
    }
}

/// Dark title bar in dark mode, and a title bar tinted to the window background (Windows 11).
pub fn apply_window_frame(hwnd: HWND, p: &Palette) {
    use winapi::um::dwmapi::DwmSetWindowAttribute;
    const DWMWA_USE_IMMERSIVE_DARK_MODE: DWORD = 20;
    const DWMWA_BORDER_COLOR: DWORD = 34;
    const DWMWA_CAPTION_COLOR: DWORD = 35;
    const DWMWA_TEXT_COLOR: DWORD = 36;
    unsafe {
        if let Some(f) = dark_api().allow_dark_mode_for_window {
            f(hwnd, p.dark as BOOL);
        }
        let set = |attr: DWORD, value: DWORD| {
            DwmSetWindowAttribute(hwnd, attr, &value as *const _ as _, std::mem::size_of::<DWORD>() as DWORD);
        };
        set(DWMWA_USE_IMMERSIVE_DARK_MODE, p.dark as DWORD);
        set(DWMWA_CAPTION_COLOR, colorref(p.bg));
        set(DWMWA_TEXT_COLOR, colorref(p.fg));
        set(DWMWA_BORDER_COLOR, colorref(if p.dark { (0x3A, 0x3A, 0x3A) } else { (0xC8, 0xC8, 0xC8) }));
    }
}

// ------------------------------------------------------------------ fonts

/// Fonts derived from the system message font (Segoe UI 9pt by default), preferring
/// Windows 11's Segoe UI Variable when it is installed.
pub struct Fonts {
    pub body: HFONT,
    pub body_strong: HFONT,
    pub caption: HFONT,
    pub heading: HFONT,
    pub title: HFONT,
    pub icon: HFONT,
    pub icon_small: HFONT,
}

pub fn create_fonts() -> Fonts {
    use winapi::um::wingdi::{CreateFontIndirectW, LOGFONTW};
    use winapi::um::winuser::{NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS, SystemParametersInfoW};
    unsafe {
        let mut metrics: NONCLIENTMETRICSW = std::mem::zeroed();
        metrics.cbSize = std::mem::size_of::<NONCLIENTMETRICSW>() as u32;
        SystemParametersInfoW(SPI_GETNONCLIENTMETRICS, metrics.cbSize, &mut metrics as *mut _ as _, 0);
        let base = metrics.lfMessageFont;
        let make = |face: Option<&str>, scale: f64, weight: i32| -> HFONT {
            let mut lf: LOGFONTW = base;
            lf.lfHeight = (lf.lfHeight as f64 * scale).round() as i32;
            lf.lfWeight = weight;
            if let Some(face) = face.filter(|f| font_installed(f)) {
                lf.lfFaceName = [0; 32];
                for (i, c) in face.encode_utf16().take(31).enumerate() {
                    lf.lfFaceName[i] = c;
                }
            }
            CreateFontIndirectW(&lf)
        };
        let icon_face = if font_installed("Segoe Fluent Icons") { "Segoe Fluent Icons" } else { "Segoe MDL2 Assets" };
        Fonts {
            body: make(Some("Segoe UI Variable Text"), 1.0, 400),
            body_strong: make(Some("Segoe UI Variable Text"), 1.0, 600),
            caption: make(Some("Segoe UI Variable Small"), 0.92, 400),
            heading: make(Some("Segoe UI Variable Display"), 1.2, 600),
            title: make(Some("Segoe UI Variable Display"), 1.45, 600),
            icon: make(Some(icon_face), 1.33, 400),
            icon_small: make(Some(icon_face), 0.95, 400),
        }
    }
}

fn font_installed(face: &str) -> bool {
    use winapi::um::wingdi::{CreateCompatibleDC, CreateFontW, DeleteDC, DeleteObject, GetTextFaceW, SelectObject};
    let wide: Vec<u16> = face.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let font = CreateFontW(-12, 0, 0, 0, 400, 0, 0, 0, 1, 0, 0, 0, 0, wide.as_ptr());
        let dc = CreateCompatibleDC(std::ptr::null_mut());
        let old = SelectObject(dc, font as _);
        let mut buf = [0u16; 64];
        let n = GetTextFaceW(dc, buf.len() as i32, buf.as_mut_ptr());
        SelectObject(dc, old);
        DeleteDC(dc);
        DeleteObject(font as _);
        n > 0 && String::from_utf16_lossy(&buf[..(n as usize - 1)]).eq_ignore_ascii_case(face)
    }
}
