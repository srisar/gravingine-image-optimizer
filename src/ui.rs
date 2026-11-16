//! Building, laying out and theming the main window's controls.

use std::ops::Range;

use native_windows_gui as nwg;
use winapi::shared::windef::{HWND, RECT};

use crate::icons::{self, px};
use crate::optimizer;
use crate::skin::{self, Align, Role, Surface, Text};
use crate::theme::{self, ControlKind};

// Layout in logical pixels (96 DPI); native-windows-gui scales them for the system DPI.
pub const MARGIN: i32 = 16;
pub const PANE_W: i32 = 300;
pub const TOP: i32 = 56;
pub const CONTENT_X: i32 = MARGIN + PANE_W + 20;
pub const FOOTER_H: i32 = 56;
pub const BUTTON_H: i32 = 30;
pub const EMPTY_ICON: i32 = 48;
const HEADING_H: i32 = 30;

/// Scale presets in the resize dropdown, in order after "Fit within max size".
pub const PERCENT_PRESETS: [u32; 3] = [75, 50, 25];
const RESIZE_MODES: [&str; 5] = ["Fit within max size", "75%", "50%", "25%", "Custom percentage"];
const SAVE_MODES: [&str; 2] = ["\"optimized\" folder next to images", "Custom folder"];

/// A trackbar with its value shown at the right (and an optional label at the left).
#[derive(Default)]
pub struct Slider {
    pub label: Option<Text>,
    pub bar: nwg::TrackBar,
    pub value: Text,
}

impl Slider {
    pub fn get(&self) -> u32 {
        self.bar.pos() as u32
    }

    pub fn set(&self, v: u32) {
        self.bar.set_pos(v as usize);
        self.sync();
    }

    /// Update the value text and repaint the custom-drawn bar.
    pub fn sync(&self) {
        self.value.set_text(&self.bar.pos().to_string());
        if let Some(hwnd) = self.bar.handle.hwnd() {
            unsafe { winapi::um::winuser::InvalidateRect(hwnd, std::ptr::null(), 1) };
        }
    }

    pub fn set_enabled(&self, e: bool) {
        if let Some(label) = &self.label {
            label.set_dimmed(!e);
        }
        self.value.set_dimmed(!e);
        // Dim instead of disabling: Windows doesn't custom-draw disabled trackbars.
        if let Some(hwnd) = self.bar.handle.hwnd() {
            skin::set_dimmed(hwnd, !e);
        }
    }
}

/// Icon, title and description at the top of a settings card (the toggle is separate).
#[derive(Default)]
pub struct CardHeader {
    pub name: Text,
    pub desc: Text,
    pub glyph: char,
}

#[derive(Default)]
pub struct Controls {
    pub window: nwg::Window,
    pub notice: nwg::Notice,
    /// Posted by the thumbnail worker when previews are ready.
    pub thumb_notice: nwg::Notice,
    #[cfg(any(debug_assertions, feature = "snapshot"))]
    pub snapshot_notice: nwg::Notice,
    pub files_dialog: nwg::FileDialog,
    pub folder_dialog: nwg::FileDialog,
    pub tooltip: nwg::Tooltip,

    pub add_files: nwg::Button,
    pub add_folder: nwg::Button,
    pub clear: nwg::Button,
    pub select_all: nwg::Button,
    pub select_none: nwg::Button,

    /// Scrolling container for the settings (left column).
    pub pane: nwg::Frame,
    pub headings: Vec<Text>,
    /// Title and description of the WebP, AVIF, JPEG and PNG cards, in that order.
    pub format_cards: Vec<CardHeader>,
    pub resize_card: CardHeader,
    pub lossless_label: Text,
    pub webp: nwg::CheckBox,
    pub webp_lossless: nwg::CheckBox,
    pub webp_quality: Slider,
    pub avif: nwg::CheckBox,
    pub avif_quality: Slider,
    pub avif_speed: Slider,
    pub jpeg: nwg::CheckBox,
    pub jpeg_quality: Slider,
    pub png: nwg::CheckBox,
    pub png_level: Slider,

    pub resize: nwg::CheckBox,
    pub resize_mode: nwg::ComboBox<&'static str>,
    pub max_width_label: Text,
    pub max_width: nwg::TextInput,
    pub max_height_label: Text,
    pub max_height: nwg::TextInput,
    pub percent_label: Text,
    pub percent: nwg::TextInput,
    pub resize_hint: Text,

    pub save_mode: nwg::ComboBox<&'static str>,
    pub output_dir: nwg::TextInput,
    pub browse: nwg::Button,
    pub skip_larger: nwg::CheckBox,
    pub overwrite: nwg::CheckBox,
    pub metadata_note: Text,

    pub list: nwg::ListView,
    pub empty_icon: Option<nwg::ControlHandle>,
    pub empty_title: Text,
    pub empty_hint: Text,
    pub empty_formats: Text,

    pub optimize: nwg::Button,
    pub cancel: nwg::Button,
    pub progress: nwg::ProgressBar,
    pub status: Text,
    pub open_output: nwg::Button,

    /// Push buttons with an icon, re-colored when the theme changes.
    pub icon_buttons: Vec<(HWND, char)>,
}

impl Controls {
    fn hwnd(handle: &nwg::ControlHandle) -> HWND {
        handle.hwnd().unwrap_or(std::ptr::null_mut())
    }

    pub fn window_hwnd(&self) -> HWND {
        Self::hwnd(&self.window.handle)
    }

    pub fn list_hwnd(&self) -> HWND {
        Self::hwnd(&self.list.handle)
    }

    pub fn checkboxes(&self) -> [&nwg::CheckBox; 8] {
        [&self.webp, &self.webp_lossless, &self.avif, &self.jpeg, &self.png, &self.resize, &self.skip_larger, &self.overwrite]
    }

    pub fn sliders(&self) -> [&Slider; 5] {
        [&self.webp_quality, &self.avif_quality, &self.avif_speed, &self.jpeg_quality, &self.png_level]
    }

    fn push_buttons(&self) -> Vec<HWND> {
        [&self.add_files, &self.add_folder, &self.clear, &self.select_all, &self.select_none, &self.browse, &self.cancel, &self.open_output]
            .iter()
            .map(|b| Self::hwnd(&b.handle))
            .collect()
    }

    /// Apply the current palette: window frame, control visual styles, icons and list colors.
    pub fn apply_theme(&self, tiles: &crate::tiles::Tiles) {
        let p = theme::refresh();
        theme::init_dark_mode(p.dark);
        skin::reset_brushes();
        let window = self.window_hwnd();
        theme::apply_window_frame(window, &p);

        let mut explorer = self.push_buttons();
        explorer.extend(self.checkboxes().iter().map(|c| Self::hwnd(&c.handle)));
        explorer.extend(self.sliders().iter().map(|s| Self::hwnd(&s.bar.handle)));
        explorer.extend([Self::hwnd(&self.optimize.handle), self.list_hwnd(), Self::hwnd(&self.tooltip.handle), Self::hwnd(&self.pane.handle)]);
        for hwnd in explorer {
            theme::theme_control(hwnd, ControlKind::Explorer, p.dark);
        }
        for hwnd in [&self.resize_mode.handle, &self.save_mode.handle, &self.max_width.handle, &self.max_height.handle, &self.percent.handle, &self.output_dir.handle] {
            theme::theme_control(Self::hwnd(hwnd), ControlKind::Field, p.dark);
        }
        for &(hwnd, glyph) in &self.icon_buttons {
            icons::set_button_icon(hwnd, glyph, p.fg, p.fg3);
        }
        if let Some(hwnd) = self.empty_icon.and_then(|h| h.hwnd()) {
            icons::set_static_icon(hwnd, icons::glyph_icon(icons::DROP, EMPTY_ICON, p.accent, Some(p.list_bg)));
        }
        tiles.apply_palette(&p);
        unsafe {
            use winapi::um::winuser::{RDW_ALLCHILDREN, RDW_ERASE, RDW_FRAME, RDW_INVALIDATE, RedrawWindow};
            RedrawWindow(window, std::ptr::null(), std::ptr::null_mut(), RDW_INVALIDATE | RDW_ERASE | RDW_FRAME | RDW_ALLCHILDREN);
        }
    }
}

pub fn build() -> Result<Controls, nwg::NwgError> {
    let mut c = Controls::default();
    let fonts = skin::init_fonts();
    nwg::Font::set_global_default(Some(nwg::Font { handle: fonts.body }));
    // Dark-mode support must be enabled before windows are created.
    let p = theme::refresh();
    theme::init_dark_mode(p.dark);

    nwg::Window::builder()
        .title("Gravingine Image Optimizer")
        .size((1140, 760))
        .center(true)
        .accept_files(true)
        // Created hidden; shown once its paint handlers and theme are in place (see app::run).
        .flags(nwg::WindowFlags::MAIN_WINDOW)
        .build(&mut c.window)?;
    let window = c.window_hwnd();
    icons::set_window_icon(window);
    theme::apply_window_frame(window, &p);
    let w = &c.window;
    let parent = w.handle;

    nwg::Notice::builder().parent(w).build(&mut c.notice)?;
    nwg::Notice::builder().parent(w).build(&mut c.thumb_notice)?;
    #[cfg(any(debug_assertions, feature = "snapshot"))]
    nwg::Notice::builder().parent(w).build(&mut c.snapshot_notice)?;
    nwg::FileDialog::builder()
        .title("Add images")
        .action(nwg::FileDialogAction::Open)
        .multiselect(true)
        .filters(format!(
            "Images({})|All files(*.*)",
            optimizer::INPUT_EXTENSIONS.iter().map(|e| format!("*.{e}")).collect::<Vec<_>>().join(";")
        ))
        .build(&mut c.files_dialog)?;
    nwg::FileDialog::builder().title("Choose a folder").action(nwg::FileDialogAction::OpenDirectory).build(&mut c.folder_dialog)?;

    // Command rows: files on the left, selection above the list.
    let mut x = MARGIN;
    for (button, text, glyph) in [
        (&mut c.add_files, "Add files…", icons::ADD_FILES),
        (&mut c.add_folder, "Add folder…", icons::ADD_FOLDER),
        (&mut c.clear, "Clear", icons::CLEAR),
    ] {
        nwg::Button::builder().text(text).position((x, 13)).size((100, BUTTON_H)).parent(w).build(button)?;
        c.icon_buttons.push((Controls::hwnd(&button.handle), glyph));
        icons::set_button_icon(Controls::hwnd(&button.handle), glyph, p.fg, p.fg3);
        x += fit_button(button, 72) + 6;
    }
    let mut x = CONTENT_X;
    for (button, text, glyph) in
        [(&mut c.select_all, "Select all", icons::SELECT_ALL), (&mut c.select_none, "Select none", icons::SELECT_NONE)]
    {
        nwg::Button::builder().text(text).position((x, 13)).size((100, BUTTON_H)).parent(w).build(button)?;
        c.icon_buttons.push((Controls::hwnd(&button.handle), glyph));
        icons::set_button_icon(Controls::hwnd(&button.handle), glyph, p.fg, p.fg3);
        x += fit_button(button, 72) + 6;
    }

    // ---------------- settings pane: a scrolling column of cards ----------------
    // Controls are created here and positioned by `layout_pane`, which runs whenever a
    // section opens/closes, the window resizes or the pane scrolls.
    nwg::Frame::builder().parent(w).flags(nwg::FrameFlags::VISIBLE).build(&mut c.pane)?;
    let pane_hwnd = Controls::hwnd(&c.pane.handle);
    unsafe {
        use winapi::um::winuser::*;
        // Vertical scrollbar (shown only when needed); never paint over the controls.
        let style = GetWindowLongPtrW(pane_hwnd, GWL_STYLE);
        SetWindowLongPtrW(pane_hwnd, GWL_STYLE, style | (WS_VSCROLL | WS_CLIPCHILDREN) as isize);
        SetWindowPos(pane_hwnd, std::ptr::null_mut(), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED);
        ShowScrollBar(pane_hwnd, SB_VERT as i32, 0);
        // Repaint only newly exposed areas when the pane resizes, not every card each time.
        let class_style = GetClassLongPtrW(pane_hwnd, GCL_STYLE);
        SetClassLongPtrW(pane_hwnd, GCL_STYLE, (class_style & !((CS_HREDRAW | CS_VREDRAW) as usize)) as isize);
    }
    skin::set_pane(pane_hwnd);
    let pane = c.pane.handle;

    for text in ["Output formats", "Image size", "Save to"] {
        c.headings.push(Text::new(pane, text, Surface::Window, Role::Heading, Align::Left)?);
    }
    let header = |name: &str, desc: &str, glyph: char| -> Result<CardHeader, nwg::NwgError> {
        Ok(CardHeader {
            name: Text::new(pane, name, Surface::Card, Role::Strong, Align::Left)?,
            desc: Text::new(pane, desc, Surface::Card, Role::Caption, Align::Left)?,
            glyph,
        })
    };

    // Output formats: one card each, toggle on the right, settings inside when on.
    c.format_cards.push(header("WebP", "Small files, keeps transparency", icons::FORMAT_WEBP)?);
    toggle(pane, &mut c.webp, false)?;
    c.webp_quality = slider(pane, "Quality", 1..100, 80)?;
    c.lossless_label = Text::new(pane, "Lossless", Surface::Card, Role::Secondary, Align::Left)?;
    toggle(pane, &mut c.webp_lossless, true)?;
    c.format_cards.push(header("AVIF", "Smallest files, slower to encode", icons::FORMAT_AVIF)?);
    toggle(pane, &mut c.avif, false)?;
    c.avif_quality = slider(pane, "Quality", 1..100, 65)?;
    c.avif_speed = slider(pane, "Speed", 1..10, 6)?;
    c.format_cards.push(header("JPEG", "Works everywhere", icons::FORMAT_JPEG)?);
    toggle(pane, &mut c.jpeg, false)?;
    c.jpeg_quality = slider(pane, "Quality", 1..100, 80)?;
    c.format_cards.push(header("PNG", "Lossless, for logos and icons", icons::FORMAT_PNG)?);
    toggle(pane, &mut c.png, false)?;
    c.png_level = slider(pane, "Effort", 0..6, 2)?;

    // Image size: same card pattern.
    c.resize_card = header("Resize images", "Shrinks large images", icons::RESIZE)?;
    toggle(pane, &mut c.resize, false)?;
    nwg::ComboBox::builder().collection(RESIZE_MODES.to_vec()).selected_index(Some(0)).parent(pane).build(&mut c.resize_mode)?;
    c.max_width_label = field(pane, &mut c.max_width, "Max width (px)", "1920")?;
    c.percent_label = field(pane, &mut c.percent, "Scale (%)", "50")?;
    c.percent.set_limit(3);
    c.max_height_label = field(pane, &mut c.max_height, "Max height (px)", "1920")?;
    c.resize_hint = Text::new(pane, "", Surface::Card, Role::Secondary, Align::Left)?;

    // Save to.
    nwg::ComboBox::builder().collection(SAVE_MODES.to_vec()).selected_index(Some(0)).parent(pane).build(&mut c.save_mode)?;
    nwg::Button::builder().text("Browse…").size((94, BUTTON_H)).parent(pane).build(&mut c.browse)?;
    c.icon_buttons.push((Controls::hwnd(&c.browse.handle), icons::BROWSE));
    icons::set_button_icon(Controls::hwnd(&c.browse.handle), icons::BROWSE, p.fg, p.fg3);
    fit_button(&c.browse, 80);
    // Created at its final height: the text box sets up its vertical padding at creation.
    nwg::TextInput::builder().readonly(true).placeholder_text(Some("No folder selected")).size((180, 26)).parent(pane).build(&mut c.output_dir)?;
    if let Some(h) = c.output_dir.handle.hwnd() {
        skin::style(h, Surface::Field, Role::Primary);
    }
    checkbox(pane, &mut c.skip_larger, "Skip files larger than the original")?;
    checkbox(pane, &mut c.overwrite, "Overwrite existing files")?;

    c.metadata_note = Text::new(pane, "Metadata (EXIF, GPS) is removed and photo orientation is applied.", Surface::Window, Role::Tertiary, Align::Left)?;
    if let Some(h) = c.metadata_note.handle.hwnd() {
        // Allow two lines.
        unsafe {
            use winapi::um::winuser::{GWL_STYLE, GetWindowLongPtrW, SS_ENDELLIPSIS, SetWindowLongPtrW};
            let style = GetWindowLongPtrW(h, GWL_STYLE);
            SetWindowLongPtrW(h, GWL_STYLE, style & !(SS_ENDELLIPSIS as isize));
        }
    }


    // ---------------- file list ----------------
    nwg::ListView::builder()
        .list_style(nwg::ListViewStyle::Detailed)
        .flags(nwg::ListViewFlags::VISIBLE | nwg::ListViewFlags::TAB_STOP | nwg::ListViewFlags::ALWAYS_SHOW_SELECTION)
        .position((CONTENT_X, TOP))
        .size((600, 500))
        .parent(w)
        .build(&mut c.list)?;
    // No system border: the window paints a rounded frame around the list instead.
    unsafe {
        use winapi::um::winuser::*;
        let list = c.list_hwnd();
        let style = GetWindowLongPtrW(list, GWL_STYLE);
        SetWindowLongPtrW(list, GWL_STYLE, style & !(WS_BORDER as isize));
        let ex = GetWindowLongPtrW(list, GWL_EXSTYLE);
        SetWindowLongPtrW(list, GWL_EXSTYLE, ex & !(WS_EX_CLIENTEDGE as isize));
        SetWindowPos(list, std::ptr::null_mut(), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED);
    }
    // Columns hold the tile text: the file name, then the three detail lines.
    for (i, title) in ["Name", "Details", "Output", "Location"].into_iter().enumerate() {
        c.list.insert_column(nwg::InsertListViewColumn {
            index: Some(i as i32),
            fmt: Some(nwg::ListViewColumnFlags::LEFT),
            width: Some(200),
            text: Some(title.into()),
        });
    }

    // Empty state: children of the list view so they sit on its background.
    let list_parent = c.list.handle;
    let empty_icon = icons::glyph_icon(icons::DROP, EMPTY_ICON, p.accent, Some(p.list_bg));
    c.empty_icon = Some(icon_static(list_parent, empty_icon)?);
    c.empty_title = Text::new(list_parent, "Drop images or folders here", Surface::List, Role::Title, Align::Center)?;
    c.empty_hint = Text::new(list_parent, "or use Add files… or Add folder…", Surface::List, Role::Secondary, Align::Center)?;
    c.empty_formats = Text::new(list_parent, "JPG · PNG · WebP · GIF · BMP · TIFF · ICO", Surface::List, Role::Tertiary, Align::Center)?;

    // ---------------- footer ----------------
    nwg::Button::builder().text("Optimize").size((120, BUTTON_H + 2)).parent(w).build(&mut c.optimize)?;
    nwg::Button::builder().text("Cancel").size((100, BUTTON_H)).parent(w).build(&mut c.cancel)?;
    nwg::ProgressBar::builder().range(0..100).size((200, 4)).parent(w).build(&mut c.progress)?;
    c.status = Text::new(parent, "", Surface::Window, Role::Secondary, Align::Left)?;
    nwg::Button::builder().text("Open output folder").size((160, BUTTON_H)).parent(w).build(&mut c.open_output)?;
    skin::accent_button(Controls::hwnd(&c.optimize.handle), icons::OPTIMIZE);
    for (button, glyph) in [(&c.cancel, icons::CANCEL), (&c.open_output, icons::OPEN_FOLDER)] {
        c.icon_buttons.push((Controls::hwnd(&button.handle), glyph));
        icons::set_button_icon(Controls::hwnd(&button.handle), glyph, p.fg, p.fg3);
    }
    fit_button(&c.cancel, 96);
    fit_button(&c.open_output, 120);
    unsafe {
        use winapi::um::winuser::{BM_SETSTYLE, BS_DEFPUSHBUTTON, SendMessageW};
        SendMessageW(Controls::hwnd(&c.optimize.handle), BM_SETSTYLE, BS_DEFPUSHBUTTON as usize, 1);
    }

    // ---------------- tooltips ----------------
    nwg::Tooltip::builder().build(&mut c.tooltip)?;
    let tips: [(&nwg::ControlHandle, &str); 16] = [
        (&c.webp.handle, "WebP: 25–35% smaller than JPEG, keeps transparency. Supported by all modern browsers."),
        (&c.webp_quality.bar.handle, "WebP quality. 75–85 looks like the original for most photos."),
        (&c.webp_lossless.handle, "Lossless WebP: exact pixels, bigger files. Good for graphics and screenshots."),
        (&c.avif.handle, "AVIF: the smallest files (often half of JPEG), slower to encode. Supported by modern browsers."),
        (&c.avif_quality.bar.handle, "AVIF quality. 55–70 is a good range for photos."),
        (&c.avif_speed.bar.handle, "Encoding speed: lower is slower but makes slightly smaller files."),
        (&c.jpeg.handle, "JPEG: works everywhere. Transparent areas become white."),
        (&c.jpeg_quality.bar.handle, "JPEG quality (mozjpeg). 75–85 is a good range for photos."),
        (&c.png.handle, "PNG: lossless. Best for logos, icons and screenshots with sharp edges."),
        (&c.png_level.bar.handle, "Compression effort: higher makes smaller PNGs but takes longer. Never changes pixels."),
        (&c.resize.handle, "Shrink large images. Keeps the aspect ratio and never makes images bigger."),
        (&c.skip_larger.handle, "Don't save an output that would be bigger than the original file."),
        (&c.overwrite.handle, "Replace files with the same name in the output folder. Otherwise a number is added."),
        (&c.select_all.handle, "Tick every image in the list"),
        (&c.select_none.handle, "Untick every image in the list"),
        (&c.open_output.handle, "Open the folder the optimized files were saved to"),
    ];
    for (handle, text) in tips {
        c.tooltip.register(*handle, text);
    }

    Ok(c)
}

pub fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
    RECT { left, top, right, bottom }
}

fn checkbox(parent: nwg::ControlHandle, out: &mut nwg::CheckBox, text: &str) -> Result<(), nwg::NwgError> {
    nwg::CheckBox::builder().text(text).size((200, 24)).parent(parent).build(out)?;
    if let Some(h) = out.handle.hwnd() {
        skin::checkbox(h, Surface::Card);
    }
    Ok(())
}

/// A checkbox drawn as a toggle switch.
fn toggle(parent: nwg::ControlHandle, out: &mut nwg::CheckBox, small: bool) -> Result<(), nwg::NwgError> {
    nwg::CheckBox::builder().text("").size((44, 24)).parent(parent).build(out)?;
    if let Some(h) = out.handle.hwnd() {
        skin::toggle(h, Surface::Card, small);
    }
    Ok(())
}

fn slider(parent: nwg::ControlHandle, label: &str, range: Range<usize>, pos: usize) -> Result<Slider, nwg::NwgError> {
    let mut s = Slider { label: Some(Text::new(parent, label, Surface::Card, Role::Secondary, Align::Left)?), ..Default::default() };
    nwg::TrackBar::builder()
        .range(Some(range))
        .pos(Some(pos))
        .flags(nwg::TrackBarFlags::VISIBLE | nwg::TrackBarFlags::NO_TICK)
        .size((150, 28))
        .parent(parent)
        .build(&mut s.bar)?;
    if let Some(h) = s.bar.handle.hwnd() {
        skin::slider(h, Surface::Card);
    }
    s.value = Text::new(parent, &pos.to_string(), Surface::Card, Role::Primary, Align::Right)?;
    Ok(s)
}

fn field(parent: nwg::ControlHandle, input: &mut nwg::TextInput, text: &str, value: &str) -> Result<Text, nwg::NwgError> {
    let label = Text::new(parent, text, Surface::Card, Role::Primary, Align::Left)?;
    nwg::TextInput::builder()
        .text(value)
        .flags(nwg::TextInputFlags::VISIBLE | nwg::TextInputFlags::NUMBER | nwg::TextInputFlags::TAB_STOP | nwg::TextInputFlags::AUTO_SCROLL)
        .limit(5)
        .size((90, 26))
        .parent(parent)
        .build(input)?;
    Ok(label)
}

// ------------------------------------------------------------------ settings pane layout

/// Position every settings control for the current toggles and modes, `scroll` logical
/// pixels down, and update the painted cards and decorations. Returns the content height and
/// whether the cards changed (sections opened or closed, so controls moved or were hidden).
pub fn layout_pane(c: &Controls, scroll: i32) -> (i32, bool) {
    use crate::geom;
    use skin::Deco;
    let hwnd = |h: &nwg::ControlHandle| h.hwnd().unwrap_or(std::ptr::null_mut());
    let show = |h: &nwg::ControlHandle, v: bool| {
        if let Some(h) = h.hwnd()
            && skin::shown(h) != v
        {
            use winapi::um::winuser::{SW_HIDE, SW_SHOW, ShowWindow};
            unsafe { ShowWindow(h, if v { SW_SHOW } else { SW_HIDE }) };
        }
    };
    let (left, right) = (MARGIN, MARGIN + PANE_W);
    let inner = left + 56; // content column, aligned with card titles
    let mut cards = Vec::new();
    let mut decos = Vec::new();
    let mut y = 8;
    let at = |y: i32| y - scroll;

    let heading = |i: usize, y: &mut i32| {
        c.headings[i].place(left + 2, at(*y), PANE_W, HEADING_H - 6);
        *y += HEADING_H;
    };
    // Card header: icon tile, title, description and toggle. Returns the header bottom.
    let card_header = |h: &CardHeader, toggle: &nwg::CheckBox, top: i32, decos: &mut Vec<Deco>| -> i32 {
        decos.push(Deco::IconTile([left + 12, top + 12, left + 44, top + 44], h.glyph));
        h.name.place(inner, at(top + 9), PANE_W - 56 - 64, 20);
        h.desc.place(inner, at(top + 29), PANE_W - 56 - 64, 18);
        geom::place(hwnd(&toggle.handle), right - 12 - 46, at(top + 16), 46, 24);
        top + 56
    };
    let slider_row = |s: &Slider, y: i32, v: bool| {
        if let Some(label) = &s.label {
            label.place(inner, at(y + 5), 56, 20);
            label.set_visible(v);
        }
        geom::place(hwnd(&s.bar.handle), inner + 50, at(y - 2), right - 12 - 30 - (inner + 50), 28);
        s.value.place(right - 12 - 28, at(y + 3), 28, 20);
        show(&s.bar.handle, v);
        s.value.set_visible(v);
    };

    // ---- Output formats
    heading(0, &mut y);
    let formats: [(&nwg::CheckBox, Vec<&Slider>); 4] = [
        (&c.webp, vec![&c.webp_quality]),
        (&c.avif, vec![&c.avif_quality, &c.avif_speed]),
        (&c.jpeg, vec![&c.jpeg_quality]),
        (&c.png, vec![&c.png_level]),
    ];
    for (i, (toggle, sliders)) in formats.iter().enumerate() {
        let on = checked(toggle);
        let top = y;
        y = card_header(&c.format_cards[i], toggle, top, &mut decos);
        if on {
            decos.push(Deco::Divider { left: left + 1, right: right - 1, y });
            y += 8;
        }
        for s in sliders {
            slider_row(s, y, on);
            if on {
                y += 30;
            }
        }
        if i == 0 {
            // WebP: the lossless switch sits under the quality slider.
            c.lossless_label.place(inner, at(y + 5), 120, 20);
            geom::place(hwnd(&c.webp_lossless.handle), right - 12 - 38, at(y + 3), 38, 22);
            c.lossless_label.set_visible(on);
            show(&c.webp_lossless.handle, on);
            if on {
                y += 30;
            }
        }
        if on {
            y += 4;
        }
        cards.push(rect(left, top, right, y));
        y += 6;
    }
    y += 8;

    // ---- Image size
    heading(1, &mut y);
    let on = checked(&c.resize);
    let top = y;
    y = card_header(&c.resize_card, &c.resize, top, &mut decos);
    let mode = c.resize_mode.selection().unwrap_or(0);
    let (fit, custom) = (mode == 0, mode == 4);
    if on {
        decos.push(Deco::Divider { left: left + 1, right: right - 1, y });
        y += 10;
    }
    geom::place(hwnd(&c.resize_mode.handle), inner, at(y), right - 12 - inner, 26);
    show(&c.resize_mode.handle, on);
    let field_row = |label: &Text, input: &nwg::TextInput, y: i32, v: bool| {
        label.place(inner, at(y + 5), 120, 20);
        geom::place(hwnd(&input.handle), right - 12 - 90, at(y), 90, 26);
        label.set_visible(v);
        show(&input.handle, v);
    };
    let row_a = y + 34;
    let row_b = row_a + 32;
    field_row(&c.max_width_label, &c.max_width, row_a, on && fit);
    field_row(&c.max_height_label, &c.max_height, row_b, on && fit);
    field_row(&c.percent_label, &c.percent, row_a, on && custom);
    let hint_y = if custom { row_b } else { row_a };
    c.resize_hint.place(inner, at(hint_y + 5), right - 12 - inner, 20);
    c.resize_hint.set_visible(on && !fit);
    if on {
        y = if fit || custom { row_b + 32 } else { row_a + 30 } + 4;
    }
    cards.push(rect(left, top, right, y));
    y += 14;

    // ---- Save to
    heading(2, &mut y);
    let top = y;
    y += 12;
    let pad = left + 12;
    geom::place(hwnd(&c.save_mode.handle), pad, at(y), right - 12 - pad, 26);
    y += 34;
    let browse = hwnd(&c.browse.handle);
    let browse_w = geom::size(browse).0;
    geom::move_to(browse, right - 12 - browse_w, at(y - 2));
    geom::place(hwnd(&c.output_dir.handle), pad, at(y), right - 12 - browse_w - 6 - pad, 26);
    y += 34;
    geom::place(hwnd(&c.skip_larger.handle), pad, at(y), PANE_W - 24, 24);
    y += 26;
    geom::place(hwnd(&c.overwrite.handle), pad, at(y), PANE_W - 24, 24);
    y += 34;
    cards.push(rect(left, top, right, y));
    y += 10;

    c.metadata_note.place(left + 2, at(y), PANE_W, 34);
    y += 44;

    let changed = skin::set_pane_content(cards, decos);
    if changed && let Some(pane) = c.pane.handle.hwnd() {
        unsafe { winapi::um::winuser::InvalidateRect(pane, std::ptr::null(), 0) };
    }
    (y, changed)
}

/// Size a push button to its icon and text (Windows' ideal size); returns the logical width.
pub fn fit_button(button: &nwg::Button, min_width: i32) -> i32 {
    use winapi::shared::windef::SIZE;
    use winapi::um::commctrl::BCM_GETIDEALSIZE;
    use winapi::um::winuser::SendMessageW;
    let mut width = min_width;
    if let Some(hwnd) = button.handle.hwnd() {
        let mut ideal = SIZE { cx: 0, cy: 0 };
        unsafe { SendMessageW(hwnd, BCM_GETIDEALSIZE, 0, &mut ideal as *mut _ as isize) };
        width = width.max(crate::geom::logical(ideal.cx) + 16);
        let (_, h) = crate::geom::size(hwnd);
        crate::geom::resize(hwnd, width, h.max(1));
    }
    width
}

/// Size the custom-drawn accent button to its glyph and text; returns the logical width.
pub fn fit_accent_button(button: &nwg::Button, min_width: i32) -> i32 {
    use winapi::um::winuser::{GetDC, ReleaseDC};
    let f = skin::fonts();
    let text = button.text();
    let width = unsafe {
        let dc = GetDC(std::ptr::null_mut());
        let w = crate::draw::text_width(dc, "\u{E945}", f.icon) + px(8) + crate::draw::text_width(dc, &text, f.body_strong) + px(40);
        ReleaseDC(std::ptr::null_mut(), dc);
        w
    };
    let width = crate::geom::logical(width).max(min_width);
    if let Some(hwnd) = button.handle.hwnd() {
        let (_, h) = crate::geom::size(hwnd);
        crate::geom::resize(hwnd, width, h);
    }
    width
}

/// A static control (SS_ICON) showing `icon`, as a child of `parent`.
fn icon_static(parent: nwg::ControlHandle, icon: winapi::shared::windef::HICON) -> Result<nwg::ControlHandle, nwg::NwgError> {
    use winapi::um::winuser::{SS_ICON, WS_CHILD, WS_VISIBLE};
    let handle = nwg::ControlBase::build_hwnd()
        .class_name("STATIC")
        .text("")
        .size((16, 16))
        .flags(WS_CHILD | WS_VISIBLE | SS_ICON)
        .parent(Some(parent))
        .build()?;
    if let Some(hwnd) = handle.hwnd() {
        skin::style(hwnd, Surface::List, Role::Primary);
        icons::set_static_icon(hwnd, icon);
    }
    Ok(handle)
}

pub fn show(handle: &Option<nwg::ControlHandle>, visible: bool) {
    use winapi::um::winuser::{SW_HIDE, SW_SHOW, ShowWindow};
    if let Some(hwnd) = handle.as_ref().and_then(|h| h.hwnd()) {
        unsafe { ShowWindow(hwnd, if visible { SW_SHOW } else { SW_HIDE }) };
    }
}

/// Move/resize a raw control, in logical pixels.
pub fn set_rect(handle: &nwg::ControlHandle, x: i32, y: i32, w: i32, h: i32) {
    if let Some(hwnd) = handle.hwnd() {
        crate::geom::place(hwnd, x, y, w, h);
    }
}


pub fn checked(cb: &nwg::CheckBox) -> bool {
    cb.check_state() == nwg::CheckBoxState::Checked
}

pub fn set_checked(cb: &nwg::CheckBox, v: bool) {
    cb.set_check_state(if v { nwg::CheckBoxState::Checked } else { nwg::CheckBoxState::Unchecked });
}
