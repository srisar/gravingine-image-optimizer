//! App state and behavior: adding images, thumbnails, selection, running the optimizer,
//! and reacting to window events. The controls themselves are built in [`crate::ui`].

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Instant;

use native_windows_gui as nwg;
use rayon::prelude::*;
use winapi::shared::minwindef::{LPARAM, LRESULT, WPARAM};
use winapi::shared::windef::HWND;

use crate::droptarget::{self, DragEvent};
use crate::geom;
use crate::optimizer::{self, Format, FormatResult, OutputLocation, Outcome, ResizeMode, Settings, Source};
use crate::skin;
use crate::taskbar::{self, Taskbar};
use crate::tiles::{self, Badge, TileView, Tone};
use crate::ui::{self, CONTENT_X, Controls, EMPTY_ICON, FOOTER_H, MARGIN, PERCENT_PRESETS, TOP, checked, set_checked};

/// Timer that animates progress bars on tiles being optimized.
const ANIMATION_TIMER: usize = 0x4701;
/// Posted to the main window: tile checkboxes changed.
const WM_APP_CHECKS: u32 = winapi::um::winuser::WM_APP + 1;
/// Posted to the main window: Windows' light/dark mode or accent color changed.
const WM_APP_THEME: u32 = winapi::um::winuser::WM_APP + 2;

#[derive(Clone, Debug)]
enum Status {
    Pending,
    Running,
    Done(Vec<FormatResult>),
    Failed(String),
}

struct Job {
    id: u64,
    source: Source,
    file_name: String,
    folder: String,
    original_size: u64,
    status: Status,
    /// Ticked in the list: included in the next run.
    selected: bool,
    /// Thumbnail index in the tiles' image list.
    thumb: i32,
    /// "3840 × 2160 · JPEG", once the thumbnail worker has read the image.
    info: Option<String>,
}

/// A decoded thumbnail (or the reason it couldn't be made) for job `id`.
struct ThumbMsg {
    id: u64,
    result: Result<optimizer::Thumbnail, String>,
}

enum Msg {
    Started(usize),
    Finished(usize, Result<Vec<FormatResult>, String>),
    AllDone,
}

struct Run {
    rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    started: Instant,
    total: usize,
}

#[derive(Default)]
struct State {
    jobs: Vec<Job>,
    known: HashSet<PathBuf>,
    run: Option<Run>,
    last_output_dir: Option<PathBuf>,
    elapsed: Option<f32>,
    notice: String,
    next_id: u64,
    /// "To WebP q78 · original size": what the current settings produce, shown on waiting tiles.
    plan: String,
    thumbs: Option<Receiver<ThumbMsg>>,
    thumb_tx: Option<Sender<ThumbMsg>>,
    /// Files are being dragged over the window.
    drag_hover: bool,
}

pub struct App {
    c: Controls,
    tiles: tiles::Tiles,
    taskbar: RefCell<Option<Taskbar>>,
    /// A WM_APP_CHECKS message is already queued.
    checks_pending: std::cell::Cell<bool>,
    /// How far the settings pane is scrolled (logical pixels).
    pane_scroll: std::cell::Cell<i32>,
    // Never hold a borrow while calling into controls: Win32 calls can re-enter the event handler.
    state: RefCell<State>,
}

pub fn run() -> Result<(), nwg::NwgError> {
    nwg::init()?;
    let c = ui::build()?;
    let tiles = tiles::Tiles::new(c.list_hwnd());
    let app = Rc::new(App {
        c,
        tiles,
        taskbar: RefCell::new(None),
        checks_pending: std::cell::Cell::new(false),
        pane_scroll: std::cell::Cell::new(0),
        state: RefCell::new(State::default()),
    });
    app.apply_settings(&load_settings());
    app.c.apply_theme(&app.tiles);
    app.layout();
    app.refresh_rows();

    let weak = Rc::downgrade(&app);
    let handler = nwg::full_bind_event_handler(&app.c.window.handle, move |evt, data, handle| {
        if let Some(app) = weak.upgrade() {
            app.on_event(evt, &data, handle);
        }
    });
    // Raw handlers run before native-windows-gui's own: painting, colors, custom draw.
    let weak = Rc::downgrade(&app);
    let window_raw = nwg::bind_raw_event_handler(&app.c.window.handle, 0x10010, move |hwnd, msg, w, l| {
        weak.upgrade().and_then(|app| app.on_raw(hwnd, msg, w, l))
    })?;
    let list_raw = {
        let window = app.c.window_hwnd();
        nwg::bind_raw_event_handler(&app.c.list.handle, 0x10011, move |hwnd, msg, w, l| skin::handle(window, hwnd, msg, w, l))?
    };
    // Settings pane: its own painting and color messages (from the controls inside it), plus
    // scrolling. Sliders and dropdowns would otherwise eat the mouse wheel; send it to the pane.
    let mut pane_raw = Vec::new();
    if let Some(pane) = app.c.pane.handle.hwnd() {
        let window = app.c.window_hwnd();
        let weak = Rc::downgrade(&app);
        pane_raw.push(nwg::bind_raw_event_handler(&app.c.pane.handle, 0x10012, move |hwnd, msg, w, l| {
            if let Some(app) = weak.upgrade()
                && let Some(r) = app.on_pane_raw(msg, w)
            {
                return Some(r);
            }
            skin::handle(window, hwnd, msg, w, l)
        })?);
        let wheel_takers: Vec<nwg::ControlHandle> = app.c.sliders().iter().map(|s| s.bar.handle).chain([app.c.resize_mode.handle, app.c.save_mode.handle]).collect();
        for handle in wheel_takers {
            pane_raw.push(nwg::bind_raw_event_handler(&handle, 0x10013, move |_hwnd, msg, w, l| {
                (msg == winapi::um::winuser::WM_MOUSEWHEEL).then(|| unsafe { winapi::um::winuser::SendMessageW(pane, msg, w, l) })
            })?);
        }
        // Text boxes: let the library paint its frame, then fix its padding color.
        for handle in [app.c.output_dir.handle, app.c.max_width.handle, app.c.max_height.handle, app.c.percent.handle] {
            pane_raw.push(nwg::bind_raw_event_handler(&handle, 0x10014, move |hwnd, msg, w, l| {
                if msg != winapi::um::winuser::WM_NCPAINT {
                    return None;
                }
                let result = unsafe { winapi::um::commctrl::DefSubclassProc(hwnd, msg, w, l) };
                skin::repaint_field_padding(hwnd);
                Some(result)
            })?);
        }
    }
    // Know when files are dragged over the window, not only when they're dropped.
    let weak = Rc::downgrade(&app);
    let _drop_target = droptarget::register(
        app.c.window_hwnd(),
        Box::new(move |event| weak.upgrade().is_some_and(|app| app.on_drag(event))),
    );

    // Files/folders passed on the command line (e.g. dropped onto the .exe icon).
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if !args.is_empty() {
        app.add_paths(args);
    }

    #[cfg(any(debug_assertions, feature = "snapshot"))]
    snapshot::schedule(&app.c.snapshot_notice);

    // Show the window only now that its paint handlers, theme and layout are in place.
    app.c.window.set_visible(true);
    unsafe {
        use winapi::um::winuser::{RDW_ALLCHILDREN, RDW_ERASE, RDW_FRAME, RDW_INVALIDATE, RedrawWindow, SetForegroundWindow};
        let window = app.c.window_hwnd();
        RedrawWindow(window, std::ptr::null(), std::ptr::null_mut(), RDW_INVALIDATE | RDW_ERASE | RDW_FRAME | RDW_ALLCHILDREN);
        SetForegroundWindow(window);
    }

    nwg::dispatch_thread_events();
    nwg::unbind_raw_event_handler(&window_raw)?;
    nwg::unbind_raw_event_handler(&list_raw)?;
    for h in &pane_raw {
        nwg::unbind_raw_event_handler(h)?;
    }
    nwg::unbind_event_handler(&handler);
    Ok(())
}

impl App {
    fn on_event(&self, evt: nwg::Event, data: &nwg::EventData, handle: nwg::ControlHandle) {
        use nwg::Event as E;
        let c = &self.c;
        match evt {
            E::OnWindowClose if handle == c.window.handle => {
                save_settings(&self.read_settings());
                nwg::stop_thread_dispatch();
            }
            E::OnResize | E::OnWindowMaximize | E::OnWindowMinimize if handle == c.window.handle => self.layout(),
            E::OnFileDrop => {
                let paths: Vec<PathBuf> = data.on_file_drop().files().into_iter().map(PathBuf::from).collect();
                self.add_paths(paths);
            }
            E::OnNotice if handle == c.notice.handle => self.poll_messages(),
            E::OnNotice if handle == c.thumb_notice.handle => self.poll_thumbnails(),
            #[cfg(any(debug_assertions, feature = "snapshot"))]
            E::OnNotice if handle == c.snapshot_notice.handle => snapshot::on_notice(self),
            E::OnHorizontalScroll | E::TrackBarUpdated => {
                for s in c.sliders() {
                    s.sync();
                }
                self.sync_controls();
            }
            E::OnButtonClick => self.on_click(&handle),
            E::OnComboxBoxSelection if handle == c.save_mode.handle => {
                if c.save_mode.selection() == Some(1) && c.output_dir.text().is_empty() {
                    self.browse_output();
                }
                self.sync_controls();
            }
            E::OnComboxBoxSelection | E::OnTextInput => self.sync_controls(),
            _ => {}
        }
    }

    /// Messages handled before native-windows-gui sees them.
    fn on_raw(&self, hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> Option<LRESULT> {
        use winapi::um::commctrl::NM_CUSTOMDRAW;
        use winapi::um::winuser::{NMHDR, PostMessageW, WM_NOTIFY, WM_SETTINGCHANGE, WM_SYSCOLORCHANGE, WM_TIMER};
        let window = self.c.window_hwnd();
        match msg {
            // Deferred work posted below (a posted message runs after the current one finishes;
            // native-windows-gui's Notice would run synchronously on this thread).
            WM_APP_CHECKS => {
                self.checks_pending.set(false);
                self.on_checks_changed();
                Some(0)
            }
            WM_APP_THEME => {
                self.c.apply_theme(&self.tiles);
                self.sync_controls();
                Some(0)
            }
            WM_NOTIFY => {
                let list = self.c.list_hwnd();
                if unsafe { tiles::is_check_change(list, l) } {
                    // Many items can change at once (Select all, rebuilding the list): handle once.
                    if !self.checks_pending.replace(true) {
                        unsafe { PostMessageW(window, WM_APP_CHECKS, 0, 0) };
                    }
                    return None;
                }
                let hdr = unsafe { &*(l as *const NMHDR) };
                if hdr.hwndFrom == list && hdr.code == NM_CUSTOMDRAW {
                    return self.tiles.custom_draw(l);
                }
                skin::handle(window, hwnd, msg, w, l)
            }
            // Minimum window size (handled here: native-windows-gui's version leaks a DC per call,
            // and this message arrives continuously while resizing).
            winapi::um::winuser::WM_GETMINMAXINFO if hwnd == window => {
                let info = unsafe { &mut *(l as *mut winapi::um::winuser::MINMAXINFO) };
                info.ptMinTrackSize.x = geom::px(CONTENT_X + 420);
                info.ptMinTrackSize.y = geom::px(560);
                Some(0)
            }
            WM_TIMER if w == ANIMATION_TIMER => {
                self.tiles.animate();
                Some(0)
            }
            WM_SETTINGCHANGE | WM_SYSCOLORCHANGE => {
                // "ImmersiveColorSet": Windows switched light/dark mode or the accent color.
                if msg == WM_SYSCOLORCHANGE || is_immersive_color_change(l) {
                    unsafe { PostMessageW(window, WM_APP_THEME, 0, 0) };
                }
                None
            }
            _ => skin::handle(window, hwnd, msg, w, l),
        }
    }

    fn on_drag(&self, event: DragEvent) -> bool {
        match event {
            DragEvent::Enter => {
                if self.state.borrow().run.is_some() {
                    return false;
                }
                self.state.borrow_mut().drag_hover = true;
                self.show_drag_hover(true);
                true
            }
            DragEvent::Leave => {
                self.state.borrow_mut().drag_hover = false;
                self.show_drag_hover(false);
                true
            }
            DragEvent::Drop(paths) => {
                self.add_paths(paths);
                true
            }
        }
    }

    fn show_drag_hover(&self, on: bool) {
        let c = &self.c;
        skin::set_drag_hover(on);
        c.empty_title.set_text(if on { "Release to add images" } else { "Drop images or folders here" });
        unsafe { winapi::um::winuser::InvalidateRect(c.window_hwnd(), std::ptr::null(), 1) };
    }

    fn on_click(&self, h: &nwg::ControlHandle) {
        let c = &self.c;
        if *h == c.add_files.handle {
            if c.files_dialog.run(Some(&c.window))
                && let Ok(items) = c.files_dialog.get_selected_items()
            {
                self.add_paths(items.into_iter().map(PathBuf::from).collect());
            }
        } else if *h == c.add_folder.handle {
            if c.folder_dialog.run(Some(&c.window))
                && let Ok(dir) = c.folder_dialog.get_selected_item()
            {
                self.add_paths(vec![PathBuf::from(dir)]);
            }
        } else if *h == c.clear.handle {
            self.clear();
        } else if *h == c.select_all.handle || *h == c.select_none.handle {
            let select = *h == c.select_all.handle;
            let count = self.state.borrow().jobs.len();
            for row in 0..count {
                self.tiles.set_checked(row, select);
            }
            self.on_checks_changed();
        } else if *h == c.browse.handle {
            self.browse_output();
            self.sync_controls();
        } else if *h == c.optimize.handle {
            self.start();
        } else if *h == c.cancel.handle {
            if let Some(run) = &self.state.borrow().run {
                run.cancel.store(true, Ordering::SeqCst);
            }
        } else if *h == c.open_output.handle {
            let dir = self.state.borrow().last_output_dir.clone();
            if let Some(dir) = dir {
                open_in_explorer(&dir);
            }
        } else {
            // Settings checkboxes: update enabled controls and the plan shown on waiting tiles.
            self.sync_controls();
        }
    }

    fn browse_output(&self) {
        let c = &self.c;
        if c.folder_dialog.run(Some(&c.window))
            && let Ok(dir) = c.folder_dialog.get_selected_item()
        {
            c.output_dir.set_text(&PathBuf::from(dir).display().to_string());
        } else if c.output_dir.text().is_empty() {
            // Nothing chosen: fall back to the default location.
            c.save_mode.set_selection(Some(0));
        }
    }

    // -------------------------------------------------------------- jobs

    fn add_paths(&self, paths: Vec<PathBuf>) {
        self.read_checks();
        {
            let mut st = self.state.borrow_mut();
            if st.run.is_some() {
                return;
            }
            let before = st.jobs.len();
            match optimizer::collect_sources(&paths) {
                Ok(sources) => {
                    for source in sources {
                        if st.known.insert(source.path.clone()) {
                            st.next_id += 1;
                            let id = st.next_id;
                            st.jobs.push(new_job(id, source));
                        }
                    }
                    let new: Vec<(u64, PathBuf)> = st.jobs[before..].iter().map(|j| (j.id, j.source.path.clone())).collect();
                    if !new.is_empty() {
                        if st.thumb_tx.is_none() {
                            let (tx, rx) = channel();
                            st.thumb_tx = Some(tx);
                            st.thumbs = Some(rx);
                        }
                        if let Some(tx) = st.thumb_tx.clone() {
                            self.queue_thumbnails(tx, new);
                        }
                    }
                    st.notice = match st.jobs.len() - before {
                        0 => "No new supported images found.".into(),
                        1 => "Added 1 image.".into(),
                        n => format!("Added {n} images."),
                    };
                }
                Err(e) => st.notice = format!("Couldn't add: {e:#}"),
            }
            st.elapsed = None;
        }
        self.refresh_rows();
    }

    fn clear(&self) {
        {
            let mut st = self.state.borrow_mut();
            if st.run.is_some() {
                return;
            }
            st.jobs.clear();
            st.known.clear();
            st.elapsed = None;
            st.notice.clear();
        }
        self.refresh_rows();
        self.tiles.reset_images();
    }

    /// Decode thumbnails on worker threads; results come back through `thumb_notice`.
    fn queue_thumbnails(&self, tx: Sender<ThumbMsg>, items: Vec<(u64, PathBuf)>) {
        let notice = self.c.thumb_notice.sender();
        let size = crate::icons::px(tiles::THUMB) as u32;
        std::thread::spawn(move || {
            items.into_par_iter().for_each_with(tx, |tx, (id, path)| {
                let result = optimizer::thumbnail(&path, size).map_err(|e| format!("{e:#}"));
                let _ = tx.send(ThumbMsg { id, result });
                notice.notice();
            });
        });
    }

    fn poll_thumbnails(&self) {
        let msgs: Vec<ThumbMsg> = match &self.state.borrow().thumbs {
            Some(rx) => rx.try_iter().collect(),
            None => return,
        };
        let mut changed = Vec::new();
        for msg in msgs {
            let thumb = match &msg.result {
                Ok(t) => Some((self.tiles.add_thumbnail(&t.bgra), format!("{} × {} · {}", t.width, t.height, t.format))),
                Err(_) => None,
            };
            let mut st = self.state.borrow_mut();
            // The job may have been cleared while its thumbnail was being made.
            if let Some(i) = st.jobs.iter().position(|j| j.id == msg.id) {
                if let Some((index, info)) = thumb {
                    st.jobs[i].thumb = index;
                    st.jobs[i].info = Some(info);
                } else {
                    st.jobs[i].info = Some("Can't read this image".into());
                }
                changed.push(i);
            }
        }
        self.update_rows(&changed);
    }

    /// Copy the tiles' checkbox states into the jobs.
    fn read_checks(&self) {
        let count = self.state.borrow().jobs.len();
        if self.c.list.len() != count {
            return;
        }
        let checks: Vec<bool> = (0..count).map(|row| self.tiles.is_checked(row)).collect();
        for (job, checked) in self.state.borrow_mut().jobs.iter_mut().zip(checks) {
            job.selected = checked;
        }
    }

    fn on_checks_changed(&self) {
        let before: Vec<bool> = self.state.borrow().jobs.iter().map(|j| j.selected).collect();
        self.read_checks();
        let changed: Vec<usize> = {
            let st = self.state.borrow();
            st.jobs.iter().zip(before).enumerate().filter(|(_, (j, b))| j.selected != *b).map(|(i, _)| i).collect()
        };
        self.update_rows(&changed);
        self.sync_controls();
    }

    fn start(&self) {
        self.read_checks();
        let settings = self.read_settings();
        let custom_without_dir = self.c.save_mode.selection() == Some(1) && self.c.output_dir.text().is_empty();
        let notice = {
            let mut st = self.state.borrow_mut();
            if st.run.is_some() || st.jobs.is_empty() {
                return;
            }
            if settings.formats().is_empty() {
                Some("Turn on at least one output format.")
            } else if custom_without_dir {
                Some("Choose a folder to save to.")
            } else if !st.jobs.iter().any(|j| j.selected) {
                Some("Select at least one image.")
            } else {
                // Re-run every selected image so changed settings apply to it.
                for job in st.jobs.iter_mut().filter(|j| j.selected) {
                    job.status = Status::Pending;
                }
                let work: Vec<(usize, Source)> =
                    st.jobs.iter().enumerate().filter(|(_, j)| j.selected).map(|(i, j)| (i, j.source.clone())).collect();
                st.last_output_dir = work.first().map(|(_, s)| optimizer::output_dir(s, &settings));
                let (tx, rx) = channel();
                let cancel = Arc::new(AtomicBool::new(false));
                st.run = Some(Run { rx, cancel: cancel.clone(), started: Instant::now(), total: work.len() });
                st.elapsed = None;
                st.notice.clear();
                let sender = self.c.notice.sender();
                let worker_settings = settings.clone();
                std::thread::spawn(move || run_jobs(work, worker_settings, cancel, tx, sender));
                None
            }
        };
        match notice {
            Some(n) => {
                self.state.borrow_mut().notice = n.into();
                self.sync_controls();
            }
            None => {
                save_settings(&settings);
                self.taskbar_progress(0, 1);
                unsafe { winapi::um::winuser::SetTimer(self.c.window_hwnd(), ANIMATION_TIMER, 40, None) };
                self.refresh_rows();
            }
        }
    }

    fn poll_messages(&self) {
        let mut changed: Vec<usize> = Vec::new();
        let mut finished = false;
        let progress: (u64, u64);
        {
            let mut st = self.state.borrow_mut();
            let Some(run) = &st.run else { return };
            let total = run.total;
            let msgs: Vec<Msg> = run.rx.try_iter().collect();
            for msg in msgs {
                match msg {
                    Msg::Started(i) => {
                        st.jobs[i].status = Status::Running;
                        changed.push(i);
                    }
                    Msg::Finished(i, result) => {
                        st.jobs[i].status = match result {
                            Ok(r) => Status::Done(r),
                            Err(e) => Status::Failed(e),
                        };
                        changed.push(i);
                    }
                    Msg::AllDone => finished = true,
                }
            }
            let done = st.jobs.iter().filter(|j| j.selected && matches!(j.status, Status::Done(_) | Status::Failed(_))).count();
            progress = (done as u64, total as u64);
            if finished && let Some(run) = st.run.take() {
                st.elapsed = Some(run.started.elapsed().as_secs_f32());
                if run.cancel.load(Ordering::SeqCst) {
                    st.notice = "Cancelled.".into();
                    for job in st.jobs.iter_mut().filter(|j| matches!(j.status, Status::Running)) {
                        job.status = Status::Pending;
                    }
                }
            }
        }
        if finished {
            unsafe { winapi::um::winuser::KillTimer(self.c.window_hwnd(), ANIMATION_TIMER) };
            if let Some(t) = self.taskbar.borrow().as_ref() {
                t.clear();
            }
            taskbar::flash_if_background(self.c.window_hwnd());
            // Cancelled images go back to waiting, so redraw every tile.
            self.refresh_rows();
        } else {
            self.taskbar_progress(progress.0, progress.1);
            self.update_rows(&changed);
            self.sync_controls();
        }
        #[cfg(any(debug_assertions, feature = "snapshot"))]
        if finished {
            snapshot::on_run_finished(self);
        }
    }

    fn taskbar_progress(&self, done: u64, total: u64) {
        let mut taskbar = self.taskbar.borrow_mut();
        if taskbar.is_none() {
            *taskbar = Taskbar::new(self.c.window_hwnd());
        }
        if let Some(t) = taskbar.as_ref() {
            t.progress(done, total);
        }
    }

    // -------------------------------------------------------------- view

    /// Rebuild every tile from the jobs.
    fn refresh_rows(&self) {
        let list = &self.c.list;
        self.state.borrow_mut().plan = plan_text(&self.read_settings());
        let views: Vec<TileView> = {
            let st = self.state.borrow();
            st.jobs.iter().map(|j| tile_view(j, &st.plan)).collect()
        };
        list.set_redraw(false);
        self.tiles.clear();
        for (row, view) in views.into_iter().enumerate() {
            self.tiles.set_tile(row, view, true);
        }
        list.set_redraw(true);
        list.invalidate();
        self.sync_controls();
    }

    /// Redraw the given tiles.
    fn update_rows(&self, rows: &[usize]) {
        let views: Vec<(usize, TileView)> = {
            let st = self.state.borrow();
            rows.iter().filter(|&&i| i < st.jobs.len()).map(|&i| (i, tile_view(&st.jobs[i], &st.plan))).collect()
        };
        for (row, view) in views {
            self.tiles.set_tile(row, view, false);
        }
    }

    /// When settings change, update the plan line on tiles that haven't run yet.
    fn refresh_plan(&self) {
        let plan = plan_text(&self.read_settings());
        let waiting: Vec<usize> = {
            let mut st = self.state.borrow_mut();
            if st.plan == plan {
                return;
            }
            st.plan = plan;
            st.jobs.iter().enumerate().filter(|(_, j)| matches!(j.status, Status::Pending)).map(|(i, _)| i).collect()
        };
        self.update_rows(&waiting);
    }

    /// Enable/show controls to match the current settings and run state, and update the footer.
    fn sync_controls(&self) {
        let c = &self.c;
        self.refresh_plan();
        let (running, has_jobs, has_output, status, selected, total, progress) = {
            let st = self.state.borrow();
            let done = st.jobs.iter().filter(|j| j.selected && matches!(j.status, Status::Done(_) | Status::Failed(_))).count();
            let run_total = st.run.as_ref().map_or(0, |r| r.total);
            (
                st.run.is_some(),
                !st.jobs.is_empty(),
                st.last_output_dir.as_ref().is_some_and(|d| d.exists()),
                status_text(&st),
                st.jobs.iter().filter(|j| j.selected).count(),
                st.jobs.len(),
                (done * 100).checked_div(run_total).unwrap_or(0) as u32,
            )
        };
        let idle = !running;

        c.add_files.set_enabled(idle);
        c.add_folder.set_enabled(idle);
        c.clear.set_enabled(idle && has_jobs);
        c.select_all.set_enabled(idle && selected < total);
        c.select_none.set_enabled(idle && selected > 0);

        let empty = !has_jobs;
        if empty && !c.empty_title.visible() {
            // The list may have been resized while the prompt was hidden.
            self.place_empty_state();
        }
        ui::show(&c.empty_icon, empty);
        for t in [&c.empty_title, &c.empty_hint, &c.empty_formats] {
            t.set_visible(empty);
        }

        let webp = checked(&c.webp);
        for cb in [&c.webp, &c.avif, &c.jpeg, &c.png, &c.resize, &c.skip_larger, &c.overwrite] {
            if cb.enabled() != idle {
                cb.set_enabled(idle);
            }
        }
        let enable = |cb: &nwg::CheckBox, e: bool| {
            if cb.enabled() != e {
                cb.set_enabled(e);
            }
        };
        enable(&c.webp_lossless, idle && webp);
        c.webp_quality.set_enabled(idle && webp && !checked(&c.webp_lossless));
        c.avif_quality.set_enabled(idle && checked(&c.avif));
        c.avif_speed.set_enabled(idle && checked(&c.avif));
        c.jpeg_quality.set_enabled(idle && checked(&c.jpeg));
        c.png_level.set_enabled(idle && checked(&c.png));

        // Resize: which rows show is decided by ui::layout_pane; here only enabled state and
        // the example text.
        let resize = idle && checked(&c.resize);
        let mode = c.resize_mode.selection().unwrap_or(0);
        if c.resize_mode.enabled() != resize {
            c.resize_mode.set_enabled(resize);
        }
        for input in [&c.max_width, &c.max_height, &c.percent] {
            if input.enabled() != resize {
                input.set_enabled(resize);
            }
        }
        if mode != 0 {
            let pct = self.read_settings().resize_percent;
            let side = |s: u32| ((s as f64 * pct as f64 / 100.0).round() as u32).max(1);
            c.resize_hint.set_text(&format!("Example: 4000 × 3000 → {} × {}", side(4000), side(3000)));
        }

        let custom = c.save_mode.selection() == Some(1);
        if c.save_mode.enabled() != idle {
            c.save_mode.set_enabled(idle);
        }
        c.output_dir.set_enabled(idle && custom);
        c.browse.set_enabled(idle && custom);

        // Footer.
        let label = match (selected, total) {
            (0, _) => "Optimize".to_string(),
            (s, t) if s == t => if t == 1 { "Optimize".to_string() } else { format!("Optimize all {t}") },
            (s, _) => format!("Optimize {s} selected"),
        };
        if c.optimize.text() != label {
            c.optimize.set_text(&label);
            ui::fit_accent_button(&c.optimize, 120);
        }
        c.optimize.set_visible(idle);
        let can_run = idle && selected > 0 && !self.read_settings().formats().is_empty();
        if c.optimize.enabled() != can_run {
            c.optimize.set_enabled(can_run);
        }
        c.cancel.set_visible(running);
        c.progress.set_visible(running);
        c.progress.set_pos(progress);
        c.open_output.set_visible(idle && has_output);
        c.status.set_text(&status);
        self.layout_footer();
        // A toggle may open or close a section.
        self.relayout_pane();
    }

    fn layout(&self) {
        let c = &self.c;
        let (w, h) = geom::client_size(c.window_hwnd());
        // Settings pane: the left column, full height, scrolling when its cards don't fit.
        geom::place(c.pane.handle.hwnd().unwrap_or(std::ptr::null_mut()), 0, TOP - 8, CONTENT_X - 1, h - (TOP - 8));
        self.relayout_pane();
        // The list sits inside a rounded frame painted on the window background.
        const INSET: i32 = 4;
        let frame_w = (w - CONTENT_X - MARGIN).max(100);
        let frame_h = (h - TOP - FOOTER_H - 4).max(100);
        skin::set_highlight(Some(ui::rect(CONTENT_X, TOP, CONTENT_X + frame_w, TOP + frame_h)));
        let (list_w, list_h) = (frame_w - 2 * INSET, frame_h - 2 * INSET);
        geom::place(c.list_hwnd(), CONTENT_X + INSET, TOP + INSET, list_w, list_h);
        self.tiles.fit_width();

        // The empty-state prompt only matters (and only costs a repaint) while it's shown.
        if c.empty_title.visible() {
            self.place_empty_state();
        }
        // Repaint the background (list frame moved); WM_PAINT draws it, so no erase needed.
        unsafe { winapi::um::winuser::InvalidateRect(c.window_hwnd(), std::ptr::null(), 0) };
        self.layout_footer();
    }

    /// Center the empty-state prompt in the list view (coordinates are inside the list view).
    fn place_empty_state(&self) {
        let c = &self.c;
        let (list_w, list_h) = geom::client_size(c.list_hwnd());
        let cy = list_h / 2 - 20;
        if let Some(icon) = &c.empty_icon {
            ui::set_rect(icon, (list_w - EMPTY_ICON) / 2, cy - EMPTY_ICON - 14, EMPTY_ICON, EMPTY_ICON);
        }
        let label_w = (list_w - 40).max(100);
        c.empty_title.place(20, cy, label_w, 30);
        c.empty_hint.place(20, cy + 36, label_w, 20);
        c.empty_formats.place(20, cy + 58, label_w, 20);
    }

    fn layout_footer(&self) {
        let c = &self.c;
        let hwnd = |h: &nwg::ControlHandle| h.hwnd().unwrap_or(std::ptr::null_mut());
        let (optimize, cancel, open, progress) =
            (hwnd(&c.optimize.handle), hwnd(&c.cancel.handle), hwnd(&c.open_output.handle), hwnd(&c.progress.handle));
        let (w, h) = geom::client_size(c.window_hwnd());
        let (run_w, run_h) = geom::size(optimize);
        let y = h - FOOTER_H + (FOOTER_H - run_h) / 2;
        geom::move_to(optimize, CONTENT_X, y);
        geom::move_to(cancel, CONTENT_X, y + 1);
        let cancel_w = geom::size(cancel).0;
        let open_w = geom::size(open).0;
        let x = CONTENT_X + if skin::shown(cancel) { cancel_w } else { run_w } + 14;
        let right = if skin::shown(open) { w - MARGIN - open_w - 12 } else { w - MARGIN };
        if skin::shown(progress) {
            geom::place(progress, x, y + run_h / 2 + 8, 200, 4);
            c.status.place(x, y + run_h / 2 - 14, (right - x).max(50), 20);
        } else {
            c.status.place(x, y + (run_h - 20) / 2, (right - x).max(50), 20);
        }
        geom::move_to(open, w - MARGIN - open_w, y + 1);
    }

    // -------------------------------------------------------------- settings pane

    /// Lay out the settings cards at the current scroll position and update the scrollbar.
    /// Returns whether controls moved (a section opened or closed, or the scroll was clamped).
    fn layout_pane(&self) -> bool {
        use winapi::um::winuser::{SB_VERT, SCROLLINFO, SIF_PAGE, SIF_POS, SIF_RANGE, SetScrollInfo};
        let Some(pane) = self.c.pane.handle.hwnd() else { return false };
        let (_, view_h) = geom::client_size(pane);
        let mut scroll = self.pane_scroll.get();
        let (mut content, mut changed) = ui::layout_pane(&self.c, scroll);
        // Clamp (e.g. after a section closed or the window grew), then lay out again.
        let max = (content - view_h).max(0);
        if scroll > max {
            scroll = max;
            self.pane_scroll.set(scroll);
            skin::set_pane_scroll(scroll);
            content = ui::layout_pane(&self.c, scroll).0;
            changed = true;
        }
        let info = SCROLLINFO {
            cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
            fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
            nMin: 0,
            nMax: geom::px(content) - 1,
            nPage: geom::px(view_h) as u32,
            nPos: geom::px(scroll),
            nTrackPos: 0,
        };
        unsafe { SetScrollInfo(pane, SB_VERT as i32, &info, 1) };
        changed
    }

    /// Lay out the pane with its drawing frozen, then repaint it once if controls moved. Moving
    /// and hiding many child windows one by one while visible lets Windows copy stale pixels
    /// around (each move blits the control's old pixels, which may already be overwritten).
    fn relayout_pane(&self) {
        use winapi::um::winuser::*;
        let Some(pane) = self.c.pane.handle.hwnd() else { return };
        unsafe { SendMessageW(pane, WM_SETREDRAW, 0, 0) };
        let changed = self.layout_pane();
        unsafe {
            SendMessageW(pane, WM_SETREDRAW, 1, 0);
            if changed {
                RedrawWindow(pane, std::ptr::null(), std::ptr::null_mut(), RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_FRAME);
            }
        }
    }

    /// Scroll the settings pane to `y` (logical pixels, clamped).
    fn scroll_pane_to(&self, y: i32) {
        let Some(pane) = self.c.pane.handle.hwnd() else { return };
        let (_, view_h) = geom::client_size(pane);
        let content = ui::layout_pane(&self.c, self.pane_scroll.get()).0;
        let y = y.clamp(0, (content - view_h).max(0));
        if y == self.pane_scroll.get() {
            return;
        }
        self.pane_scroll.set(y);
        skin::set_pane_scroll(y);
        unsafe {
            use winapi::um::winuser::*;
            // Move every control at once, then repaint the pane in one go.
            SendMessageW(pane, WM_SETREDRAW, 0, 0);
            self.layout_pane();
            SendMessageW(pane, WM_SETREDRAW, 1, 0);
            RedrawWindow(pane, std::ptr::null(), std::ptr::null_mut(), RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_FRAME);
        }
    }

    /// Scrollbar and mouse wheel on the settings pane.
    fn on_pane_raw(&self, msg: u32, w: WPARAM) -> Option<LRESULT> {
        use winapi::um::winuser::*;
        let step = 40;
        match msg {
            WM_MOUSEWHEEL => {
                let delta = GET_WHEEL_DELTA_WPARAM(w) as i32;
                self.scroll_pane_to(self.pane_scroll.get() - delta * step * 3 / 2 / WHEEL_DELTA as i32);
                Some(0)
            }
            WM_VSCROLL => {
                let pane = self.c.pane.handle.hwnd()?;
                let (_, view_h) = geom::client_size(pane);
                let y = self.pane_scroll.get();
                let target = match (w & 0xFFFF) as isize {
                    SB_LINEUP => y - step,
                    SB_LINEDOWN => y + step,
                    SB_PAGEUP => y - view_h,
                    SB_PAGEDOWN => y + view_h,
                    SB_TOP => 0,
                    SB_BOTTOM => i32::MAX / 2,
                    SB_THUMBTRACK | SB_THUMBPOSITION => {
                        let mut info: SCROLLINFO = unsafe { std::mem::zeroed() };
                        info.cbSize = std::mem::size_of::<SCROLLINFO>() as u32;
                        info.fMask = SIF_TRACKPOS;
                        unsafe { GetScrollInfo(pane, SB_VERT as i32, &mut info) };
                        geom::logical(info.nTrackPos)
                    }
                    _ => return Some(0),
                };
                self.scroll_pane_to(target);
                Some(0)
            }
            _ => None,
        }
    }

    // -------------------------------------------------------------- settings

    fn apply_settings(&self, s: &Settings) {
        let c = &self.c;
        set_checked(&c.jpeg, s.jpeg);
        set_checked(&c.webp, s.webp);
        set_checked(&c.avif, s.avif);
        set_checked(&c.png, s.png);
        c.jpeg_quality.set(s.jpeg_quality as u32);
        c.webp_quality.set(s.webp_quality as u32);
        set_checked(&c.webp_lossless, s.webp_lossless);
        c.avif_quality.set(s.avif_quality as u32);
        c.avif_speed.set(s.avif_speed as u32);
        c.png_level.set(s.png_level as u32);
        set_checked(&c.resize, s.resize);
        let percent = s.resize_percent.clamp(1, 100);
        c.resize_mode.set_selection(Some(match s.resize_mode {
            ResizeMode::FitWithin => 0,
            ResizeMode::Percent => PERCENT_PRESETS.iter().position(|p| *p == percent).map_or(4, |i| i + 1),
        }));
        c.percent.set_text(&percent.to_string());
        c.max_width.set_text(&s.max_width.to_string());
        c.max_height.set_text(&s.max_height.to_string());
        match &s.output {
            OutputLocation::Subfolder => c.save_mode.set_selection(Some(0)),
            OutputLocation::Custom(dir) => {
                c.save_mode.set_selection(Some(1));
                c.output_dir.set_text(&dir.display().to_string());
            }
        }
        set_checked(&c.skip_larger, s.skip_if_larger);
        set_checked(&c.overwrite, s.overwrite);
    }

    fn read_settings(&self) -> Settings {
        let c = &self.c;
        let num = |t: &nwg::TextInput, default: u32| t.text().trim().parse::<u32>().unwrap_or(default);
        let custom_percent = num(&c.percent, 50).clamp(1, 100);
        let (resize_mode, resize_percent) = match c.resize_mode.selection().unwrap_or(0) {
            0 => (ResizeMode::FitWithin, custom_percent),
            i @ 1..=3 => (ResizeMode::Percent, PERCENT_PRESETS[i - 1]),
            _ => (ResizeMode::Percent, custom_percent),
        };
        let dir = c.output_dir.text();
        Settings {
            jpeg: checked(&c.jpeg),
            webp: checked(&c.webp),
            avif: checked(&c.avif),
            png: checked(&c.png),
            jpeg_quality: c.jpeg_quality.get().clamp(1, 100) as u8,
            webp_quality: c.webp_quality.get().clamp(1, 100) as u8,
            webp_lossless: checked(&c.webp_lossless),
            avif_quality: c.avif_quality.get().clamp(1, 100) as u8,
            avif_speed: c.avif_speed.get().clamp(1, 10) as u8,
            png_level: c.png_level.get().min(6) as u8,
            resize: checked(&c.resize),
            resize_mode,
            resize_percent,
            max_width: num(&c.max_width, 0),
            max_height: num(&c.max_height, 0),
            output: if c.save_mode.selection() == Some(1) && !dir.is_empty() {
                OutputLocation::Custom(PathBuf::from(dir))
            } else {
                OutputLocation::Subfolder
            },
            skip_if_larger: checked(&c.skip_larger),
            overwrite: checked(&c.overwrite),
        }
    }
}

fn is_immersive_color_change(l: LPARAM) -> bool {
    if l == 0 {
        return false;
    }
    let text = unsafe {
        let p = l as *const u16;
        let len = (0..64).take_while(|&i| *p.add(i) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
    };
    text == "ImmersiveColorSet"
}

fn status_text(st: &State) -> String {
    let total = st.jobs.len();
    let failed = st.jobs.iter().filter(|j| row_error(j).is_some()).count();
    if let Some(run) = &st.run {
        let done = st.jobs.iter().filter(|j| j.selected && matches!(j.status, Status::Done(_) | Status::Failed(_))).count();
        return format!("Optimizing {done} of {}…", run.total);
    }
    if total == 0 {
        return if st.notice.is_empty() { "Add images to get started.".into() } else { st.notice.clone() };
    }
    let mut parts = Vec::new();
    match totals(&st.jobs) {
        Some((orig, best)) => {
            let saved = orig.saturating_sub(best);
            let pct = if orig == 0 { 0.0 } else { saved as f64 * 100.0 / orig as f64 };
            parts.push(format!("Saved {} ({pct:.0}%): {} → {}", human_size(saved), human_size(orig), human_size(best)));
            if let Some(secs) = st.elapsed {
                let done = st.jobs.iter().filter(|j| matches!(j.status, Status::Done(_) | Status::Failed(_))).count();
                parts.push(format!("{done} files in {secs:.1}s"));
            }
            if failed > 0 {
                parts.push(format!("{failed} with errors"));
            }
        }
        None => {
            let size: u64 = st.jobs.iter().map(|j| j.original_size).sum();
            parts.push(format!("{total} image{} · {}", if total == 1 { "" } else { "s" }, human_size(size)));
        }
    }
    if !st.notice.is_empty() {
        parts.push(st.notice.clone());
    }
    parts.join(" · ")
}

fn new_job(id: u64, source: Source) -> Job {
    let original_size = std::fs::metadata(&source.path).map(|m| m.len()).unwrap_or(0);
    let file_name = source.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let folder = source.path.parent().map(|p| p.display().to_string()).unwrap_or_default();
    Job {
        id,
        source,
        file_name,
        folder,
        original_size,
        status: Status::Pending,
        selected: true,
        thumb: tiles::PLACEHOLDER,
        info: None,
    }
}

fn tile_view(job: &Job, plan: &str) -> TileView {
    let results: &[FormatResult] = match &job.status {
        Status::Done(r) => r,
        _ => &[],
    };
    let details = match &job.info {
        Some(info) => format!("{info} · {}", human_size(job.original_size)),
        None => human_size(job.original_size),
    };
    let written = results.iter().any(|r| matches!(r.outcome, Outcome::Written { .. }));
    let error = row_error(job);
    let (output, tone, badge) = match &job.status {
        Status::Pending if !job.selected => ("Not selected".to_string(), Tone::Normal, Badge::None),
        Status::Pending => (plan.to_string(), Tone::Normal, Badge::None),
        Status::Running => ("Optimizing…".to_string(), Tone::Normal, Badge::Running),
        Status::Failed(_) => ("Couldn't optimize this image".to_string(), Tone::Critical, Badge::Error),
        Status::Done(_) => {
            let (tone, badge) = if error.is_some() {
                (Tone::Critical, Badge::Error)
            } else if written {
                (Tone::Success, Badge::Done)
            } else {
                (Tone::Caution, Badge::Skipped)
            };
            (result_text(results, job.original_size), tone, badge)
        }
    };
    // Last line: an error, where the files went, or where the image is.
    let location = match error {
        Some(e) => e,
        None => results
            .iter()
            .find_map(|r| match &r.outcome {
                Outcome::Written { path, .. } => path.parent().map(|d| format!("Saved to {}", d.display())),
                _ => None,
            })
            .unwrap_or_else(|| job.folder.clone()),
    };
    TileView {
        texts: [job.file_name.clone(), details, output, location],
        tone,
        running: matches!(job.status, Status::Running),
        thumb: job.thumb,
        badge,
        selected: job.selected,
    }
}

/// "WebP 68.7 KB (−85%) · AVIF skipped, 4% larger"
fn result_text(results: &[FormatResult], original: u64) -> String {
    results
        .iter()
        .map(|r| match &r.outcome {
            Outcome::Written { size, .. } => {
                format!("{} {} ({:+.0}%)", r.format.label(), human_size(*size), -savings_pct(original, *size))
            }
            Outcome::Larger { size } => format!("{} skipped, {:.0}% larger", r.format.label(), -savings_pct(original, *size)),
            Outcome::Failed(_) => format!("{} failed", r.format.label()),
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// "To WebP q78, AVIF q65 · 50%": what the current settings will produce.
fn plan_text(s: &Settings) -> String {
    let formats: Vec<String> = s
        .formats()
        .into_iter()
        .map(|f| match f {
            Format::Jpeg => format!("JPEG q{}", s.jpeg_quality),
            Format::WebP if s.webp_lossless => "WebP lossless".into(),
            Format::WebP => format!("WebP q{}", s.webp_quality),
            Format::Avif => format!("AVIF q{}", s.avif_quality),
            Format::Png => "PNG lossless".into(),
        })
        .collect();
    if formats.is_empty() {
        return "No output format selected".into();
    }
    let size = match (s.resize, s.resize_mode) {
        (false, _) => "original size".to_string(),
        (true, ResizeMode::Percent) => format!("{}%", s.resize_percent),
        (true, ResizeMode::FitWithin) => {
            let side = |v: u32| if v == 0 { "any".to_string() } else { v.to_string() };
            format!("fit {} × {}", side(s.max_width), side(s.max_height))
        }
    };
    format!("To {} · {size}", formats.join(", "))
}

/// First error for a job: the whole file failed, or one of its formats did.
fn row_error(job: &Job) -> Option<String> {
    match &job.status {
        Status::Failed(e) => Some(e.clone()),
        Status::Done(results) => results.iter().find_map(|r| match &r.outcome {
            Outcome::Failed(e) => Some(format!("{}: {e}", r.format.label())),
            _ => None,
        }),
        _ => None,
    }
}

/// Total original bytes vs. total bytes of the smallest written output per finished image.
fn totals(jobs: &[Job]) -> Option<(u64, u64)> {
    let mut any = false;
    let (mut orig, mut best) = (0, 0);
    for job in jobs {
        if let Status::Done(results) = &job.status {
            let smallest = results
                .iter()
                .filter_map(|r| match r.outcome {
                    Outcome::Written { size, .. } => Some(size),
                    _ => None,
                })
                .min()
                .unwrap_or(job.original_size);
            orig += job.original_size;
            best += smallest.min(job.original_size);
            any = true;
        }
    }
    any.then_some((orig, best))
}

fn run_jobs(work: Vec<(usize, Source)>, settings: Settings, cancel: Arc<AtomicBool>, tx: Sender<Msg>, notice: nwg::NoticeSender) {
    work.into_par_iter().for_each_with(tx.clone(), |tx, (i, source)| {
        if cancel.load(Ordering::SeqCst) {
            return;
        }
        let _ = tx.send(Msg::Started(i));
        notice.notice();
        let result = optimizer::process(&source, &settings).map_err(|e| format!("{e:#}"));
        let _ = tx.send(Msg::Finished(i, result));
        notice.notice();
    });
    let _ = tx.send(Msg::AllDone);
    notice.notice();
}

// ------------------------------------------------------------------ settings file

fn settings_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA").map(PathBuf::from)?;
    Some(base.join("Gravingine Image Optimizer").join("settings.json"))
}

fn load_settings() -> Settings {
    settings_path()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_settings(settings: &Settings) {
    // Self-snapshot runs (which may flip settings for testing) never touch the saved settings.
    #[cfg(any(debug_assertions, feature = "snapshot"))]
    if std::env::var_os("GIO_SNAPSHOT").is_some() {
        return;
    }
    let Some(path) = settings_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_vec_pretty(settings) {
        let _ = std::fs::write(path, json);
    }
}

fn savings_pct(original: u64, new: u64) -> f64 {
    if original == 0 { 0.0 } else { (original as f64 - new as f64) * 100.0 / original as f64 }
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{v:.1} {}", UNITS[unit]) }
}

fn open_in_explorer(dir: &Path) {
    let _ = std::process::Command::new("explorer").arg(dir).spawn();
}

/// Debug-only self-check: with `GIO_SNAPSHOT=<file.png>` the app renders its own window
/// to a PNG and exits (`GIO_SNAPSHOT_RUN=1` optimizes the loaded files first,
/// `GIO_SNAPSHOT_UNCHECK=n` unticks row n first, `GIO_THEME=dark|light` forces a theme).
#[cfg(any(debug_assertions, feature = "snapshot"))]
mod snapshot {
    use super::*;

    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    pub fn schedule(notice: &nwg::Notice) {
        if std::env::var_os("GIO_SNAPSHOT").is_none() {
            return;
        }
        // Give the window time to paint before capturing (PrintWindow returns the last frame).
        let sender = notice.sender();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(2000));
            sender.notice();
        });
    }

    /// GIO_BENCH_RESIZE=1: resize the window step by step, painting each frame, and report
    /// the average time per frame (what interactive resizing costs).
    fn bench_resize(app: &App) {
        use winapi::um::winuser::{GetWindowRect, RDW_ALLCHILDREN, RDW_UPDATENOW, RedrawWindow, SWP_NOMOVE, SWP_NOZORDER, SetWindowPos};
        let Some(hwnd) = app.c.window.handle.hwnd() else { return };
        let mut rc = unsafe { std::mem::zeroed() };
        unsafe { GetWindowRect(hwnd, &mut rc) };
        let (w0, h0) = (rc.right - rc.left, rc.bottom - rc.top);
        let steps = 60;
        let start = std::time::Instant::now();
        let mut worst = std::time::Duration::ZERO;
        let (mut layout, mut paint) = (std::time::Duration::ZERO, std::time::Duration::ZERO);
        for i in 0..steps {
            let d = (i % 30) * 6;
            let t = std::time::Instant::now();
            unsafe { SetWindowPos(hwnd, std::ptr::null_mut(), 0, 0, w0 - d, h0 - d / 2, SWP_NOMOVE | SWP_NOZORDER) };
            let t2 = std::time::Instant::now();
            unsafe { RedrawWindow(hwnd, std::ptr::null(), std::ptr::null_mut(), RDW_UPDATENOW | RDW_ALLCHILDREN) };
            layout += t2 - t;
            paint += t2.elapsed();
            worst = worst.max(t.elapsed());
        }
        let n = steps as u32;
        println!("bench: resize {steps} frames, avg {:?}/frame (layout {:?}, paint {:?}), worst {:?}", start.elapsed() / n, layout / n, paint / n, worst);
        app.c.window.close();
    }

    pub fn on_notice(app: &App) {
        if std::env::var_os("GIO_BENCH_RESIZE").is_some() {
            bench_resize(app);
            return;
        }
        // GIO_SNAPSHOT_ALL_ON=1 turns every format and Resize on; GIO_SNAPSHOT_SCROLL=n scrolls
        // the settings pane n logical pixels (both before capturing).
        if std::env::var_os("GIO_SNAPSHOT_ALL_ON").is_some() {
            for t in [&app.c.webp, &app.c.avif, &app.c.jpeg, &app.c.png, &app.c.resize] {
                set_checked(t, true);
            }
            app.sync_controls();
        }
        if let Some(y) = std::env::var("GIO_SNAPSHOT_SCROLL").ok().and_then(|v| v.parse().ok()) {
            app.scroll_pane_to(y);
        }
        if std::env::var_os("GIO_SNAPSHOT_RUN").is_some() && !STARTED.swap(true, Ordering::SeqCst) {
            if let Some(row) = std::env::var("GIO_SNAPSHOT_UNCHECK").ok().and_then(|v| v.parse().ok()) {
                app.tiles.set_checked(row, false);
                app.on_checks_changed();
            }
            app.start();
            // GIO_SNAPSHOT_MIDRUN=ms captures while optimizing instead of after.
            if let Some(ms) = std::env::var("GIO_SNAPSHOT_MIDRUN").ok().and_then(|v| v.parse::<u64>().ok()) {
                let sender = app.c.snapshot_notice.sender();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(ms));
                    sender.notice();
                });
            }
        } else {
            capture_and_exit(app);
        }
    }

    pub fn on_run_finished(app: &App) {
        if std::env::var_os("GIO_SNAPSHOT").is_some() {
            schedule(&app.c.snapshot_notice);
        }
    }

    fn capture_and_exit(app: &App) {
        let rows = app.state.borrow().jobs.len();
        if rows > 0 {
            let ok = (0..rows).filter(|&r| app.tiles.checkbox_hit_ok(r)).count();
            println!("snapshot: tile checkbox hit-test {ok}/{rows} rows land on the list view's checkbox");
        }
        if let (Some(path), Some(hwnd)) = (std::env::var_os("GIO_SNAPSHOT"), app.c.window.handle.hwnd()) {
            // Finish any pending painting first, so the capture shows the current state.
            // GIO_SNAPSHOT_RAW=1 skips this, to see exactly what is on screen.
            if std::env::var_os("GIO_SNAPSHOT_RAW").is_none() { unsafe {
                use winapi::um::winuser::{RDW_ALLCHILDREN, RDW_UPDATENOW, RedrawWindow};
                RedrawWindow(hwnd, std::ptr::null(), std::ptr::null_mut(), RDW_UPDATENOW | RDW_ALLCHILDREN);
            } }
            match capture(hwnd) {
                Some(img) => match img.save(&path) {
                    Ok(()) => println!("snapshot: wrote {}", PathBuf::from(&path).display()),
                    Err(e) => eprintln!("snapshot: {e}"),
                },
                None => eprintln!("snapshot: capture failed"),
            }
        }
        app.c.window.close();
    }

    /// Render this app's own window (not the screen) with PrintWindow.
    fn capture(hwnd: winapi::shared::windef::HWND) -> Option<image::RgbaImage> {
        use winapi::um::wingdi::*;
        use winapi::um::winuser::{GetDC, GetWindowRect, PrintWindow, ReleaseDC};
        unsafe {
            let mut rc = std::mem::zeroed();
            GetWindowRect(hwnd, &mut rc);
            let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
            let screen = GetDC(std::ptr::null_mut());
            let mem = CreateCompatibleDC(screen);
            let bmp = CreateCompatibleBitmap(screen, w, h);
            let old = SelectObject(mem, bmp as _);
            let ok = PrintWindow(hwnd, mem, 2 /* PW_RENDERFULLCONTENT */) != 0;
            let mut info: BITMAPINFO = std::mem::zeroed();
            info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            info.bmiHeader.biWidth = w;
            info.bmiHeader.biHeight = -h;
            info.bmiHeader.biPlanes = 1;
            info.bmiHeader.biBitCount = 32;
            info.bmiHeader.biCompression = BI_RGB;
            let mut buf = vec![0u8; (w * h * 4) as usize];
            GetDIBits(mem, bmp, 0, h as u32, buf.as_mut_ptr() as _, &mut info, DIB_RGB_COLORS);
            SelectObject(mem, old);
            DeleteObject(bmp as _);
            DeleteDC(mem);
            ReleaseDC(std::ptr::null_mut(), screen);
            if !ok {
                return None;
            }
            for px in buf.as_chunks_mut::<4>().0 {
                px.swap(0, 2);
                px[3] = 255;
            }
            image::RgbaImage::from_raw(w as u32, h as u32, buf)
        }
    }
}
