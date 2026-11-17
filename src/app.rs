//! App state and behavior: adding images, thumbnails, selection, running the optimizer and
//! reacting to window events. The window itself is described in `ui/app.slint`.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use rayon::prelude::*;
use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
use slint::{ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};
use winapi::shared::windef::HWND;

use crate::optimizer::{self, Format, FormatResult, OutputLocation, Outcome, ResizeMode, Settings, Source};
use crate::taskbar::{self, Taskbar};
use crate::{AppWindow, CompareFormat, TileData, Ui, winicon};

/// One entry of the resize dropdown.
#[derive(Clone, Copy, PartialEq)]
enum ResizeChoice {
    /// A named size: fit within it, turned to match the image (`ResizeMode::Box`).
    Size(&'static str, u32, u32),
    /// Scale to a fixed percentage.
    Percent(u32),
    /// The max width / height fields (`ResizeMode::FitWithin`).
    CustomSize,
    /// The percent field.
    CustomPercent,
}

/// The resize dropdown, in order. Images are never enlarged.
const RESIZE_CHOICES: [ResizeChoice; 11] = [
    ResizeChoice::Size("Thumbnails", 230, 230),
    ResizeChoice::Size("Small", 640, 480),
    ResizeChoice::Size("Medium", 800, 600),
    ResizeChoice::Size("Large", 1024, 768),
    ResizeChoice::Size("HD", 1920, 1080),
    ResizeChoice::Size("4K", 3840, 2160),
    ResizeChoice::Percent(25),
    ResizeChoice::Percent(50),
    ResizeChoice::Percent(75),
    ResizeChoice::CustomSize,
    ResizeChoice::CustomPercent,
];

impl ResizeChoice {
    fn label(self) -> String {
        match self {
            ResizeChoice::Size(name, w, h) => format!("{name} ({w} × {h})"),
            ResizeChoice::Percent(p) => format!("Reduce to {p}%"),
            ResizeChoice::CustomSize => "Custom size".into(),
            ResizeChoice::CustomPercent => "Custom percentage".into(),
        }
    }
}
/// Thumbnail size in the list (logical pixels).
const THUMB: f32 = 72.0;
const IMAGE_EXTENSIONS: [&str; 10] = ["jpg", "jpeg", "jfif", "png", "webp", "gif", "bmp", "tif", "tiff", "ico"];

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
    thumb: Option<Image>,
    meta: Meta,
    /// Output dimensions of the last run (`None`: kept its size). Set when a run starts.
    run_dims: Option<(u32, u32)>,
}

/// The open before / after comparison: an image's saved outputs, or a live preview of what
/// the current settings would produce (encoded in memory, nothing written to disk).
struct Compare {
    name: String,
    original: PathBuf,
    original_size: u64,
    /// Original pixel size (upright).
    width: u32,
    height: u32,
    /// Outputs a run saved (empty if the image hasn't been optimized yet).
    saved: Vec<CompareOutput>,
    /// Showing the live preview rather than the saved outputs.
    preview: bool,
    /// The formats on show: the saved outputs, or those the settings would produce.
    formats: Vec<Format>,
    /// Index into `formats`; `None` when none of them can be shown.
    current: Option<usize>,
    /// The original, decoded once and shared with the preview encoder.
    decoded: Option<Arc<optimizer::Decoded>>,
    /// The original's pixels are on screen.
    original_shown: bool,
    /// The request a worker is busy with (one at a time); `dirty` asks for another after it.
    in_flight: Option<u64>,
    dirty: bool,
}

struct CompareOutput {
    format: Format,
    path: PathBuf,
    size: u64,
}

/// What a comparison worker sends back.
struct CompareResult {
    request: u64,
    original_path: PathBuf,
    decoded: Result<Arc<optimizer::Decoded>, String>,
    /// The original's pixels, when they weren't on screen yet.
    original: Option<PixelBuffer>,
    /// The output's pixels and file size, unless there was nothing to show.
    output: Option<Result<(PixelBuffer, u64), String>>,
}

/// What a comparison worker should produce on the right of the split line.
enum CompareJob {
    Nothing,
    Saved { path: PathBuf, size: u64 },
    Preview { format: Format, settings: Settings },
}

type PixelBuffer = SharedPixelBuffer<Rgba8Pixel>;

/// Pixels that can cross threads; the Slint image is made on the UI thread.
fn pixel_buffer(img: image::RgbaImage) -> PixelBuffer {
    PixelBuffer::clone_from_slice(img.as_raw(), img.width(), img.height())
}

/// The outputs a finished image saved, in the order they were written.
fn compare_outputs(job: &Job) -> Vec<CompareOutput> {
    let Status::Done(results) = &job.status else { return Vec::new() };
    results
        .iter()
        .filter_map(|r| match &r.outcome {
            Outcome::Written { path, size } => Some(CompareOutput { format: r.format, path: path.clone(), size: *size }),
            _ => None,
        })
        .collect()
}

/// What the thumbnail worker found out about an image.
#[derive(Clone, Copy)]
enum Meta {
    Loading,
    Read { width: u32, height: u32, format: &'static str },
    Unreadable,
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
    /// "To WebP at 78% · original size": what the current settings produce, shown on waiting tiles.
    plan: String,
    thumbs: Option<Receiver<ThumbMsg>>,
    thumb_tx: Option<Sender<ThumbMsg>>,
    /// The before / after comparison, while it's open.
    compare: Option<Compare>,
    /// The image just added on its own: its preview opens once it has been read.
    auto_preview: Option<u64>,
}

/// Wakes the UI thread from a worker, at most once until the UI thread has handled it.
#[derive(Clone)]
struct Waker {
    queued: Arc<AtomicBool>,
    on_wake: fn(&App),
}

impl Waker {
    fn new(on_wake: fn(&App)) -> Self {
        Waker { queued: Arc::new(AtomicBool::new(false)), on_wake }
    }

    fn wake(&self) {
        if self.queued.swap(true, Ordering::SeqCst) {
            return;
        }
        let (queued, on_wake) = (self.queued.clone(), self.on_wake);
        let _ = slint::invoke_from_event_loop(move || {
            queued.store(false, Ordering::SeqCst);
            with_app(on_wake);
        });
    }
}

pub struct App {
    ui: AppWindow,
    tiles: Rc<VecModel<TileData>>,
    taskbar: RefCell<Option<Taskbar>>,
    run_waker: Waker,
    thumb_waker: Waker,
    /// Files dropped on the window, added together once the drop is complete.
    dropped: RefCell<Vec<PathBuf>>,
    drop_flush_queued: Cell<bool>,
    /// Re-encodes the before / after preview once settings stop changing.
    preview_timer: slint::Timer,
    /// Last comparison load started (unique across comparisons).
    compare_request: Cell<u64>,
    // Never hold a borrow while calling into the UI: callbacks can re-enter the app.
    state: RefCell<State>,
}

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

/// Run `f` with the app (UI thread only).
fn with_app(f: impl FnOnce(&App)) {
    let app = APP.with(|a| a.borrow().clone());
    if let Some(app) = app {
        f(&app);
    }
}

pub fn run() -> Result<(), slint::PlatformError> {
    // The winit backend gives access to the native window (drops, taskbar, title bar).
    slint::BackendSelector::new().backend_name("winit".into()).select()?;
    let ui = AppWindow::new()?;
    ui.global::<Ui>().set_icon_font(winicon::icon_font_face().into());
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());
    ui.set_app_build(env!("GIO_BUILD").into());
    let resize_options: Vec<slint::SharedString> = RESIZE_CHOICES.iter().map(|c| c.label().into()).collect();
    ui.set_resize_options(ModelRc::new(VecModel::from(resize_options)));
    let index_of = |wanted: ResizeChoice| RESIZE_CHOICES.iter().position(|c| *c == wanted).unwrap_or(0) as i32;
    ui.set_resize_custom_size(index_of(ResizeChoice::CustomSize));
    ui.set_resize_custom_percent(index_of(ResizeChoice::CustomPercent));
    match std::env::var("GIO_THEME").as_deref() {
        Ok("dark") => ui.invoke_set_dark(true),
        Ok("light") => ui.invoke_set_dark(false),
        _ => {}
    }
    let tiles = Rc::new(VecModel::default());
    ui.set_tiles(ModelRc::from(tiles.clone()));
    let app = Rc::new(App {
        ui,
        tiles,
        taskbar: RefCell::new(None),
        run_waker: Waker::new(App::poll_messages),
        thumb_waker: Waker::new(App::poll_thumbnails),
        dropped: RefCell::new(Vec::new()),
        drop_flush_queued: Cell::new(false),
        preview_timer: slint::Timer::default(),
        compare_request: Cell::new(0),
        state: RefCell::new(State::default()),
    });
    APP.with(|a| *a.borrow_mut() = Some(app.clone()));
    app.apply_settings(&load_settings());
    app.bind();
    app.refresh_rows();

    // Files/folders passed on the command line (e.g. dropped onto the .exe icon).
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if !args.is_empty() {
        app.add_paths(args);
    }

    // The native window exists once the event loop runs.
    slint::Timer::single_shot(Duration::ZERO, || with_app(App::on_window_ready));
    #[cfg(any(debug_assertions, feature = "snapshot"))]
    snapshot::schedule();

    app.ui.run()?;
    save_settings(&app.read_settings());
    APP.with(|a| a.borrow_mut().take());
    Ok(())
}

impl App {
    fn bind(&self) {
        let ui = &self.ui;
        ui.on_add_files(|| with_app(App::add_files));
        ui.on_add_folder(|| with_app(App::add_folder));
        ui.on_clear_all(|| with_app(App::clear));
        ui.on_select_all(|| with_app(|a| a.select_all(true)));
        ui.on_select_none(|| with_app(|a| a.select_all(false)));
        ui.on_browse(|| with_app(App::browse_output));
        ui.on_optimize(|| with_app(App::start));
        ui.on_cancel(|| {
            with_app(|a| {
                if let Some(run) = &a.state.borrow().run {
                    run.cancel.store(true, Ordering::SeqCst);
                }
            })
        });
        ui.on_open_output(|| {
            with_app(|a| {
                let dir = a.state.borrow().last_output_dir.clone();
                if let Some(dir) = dir {
                    open_in_explorer(&dir);
                }
            })
        });
        ui.on_settings_changed(|| with_app(App::on_settings_changed));
        ui.on_tile_toggled(|row, on| with_app(|a| a.set_selected(row as usize, on)));
        ui.on_compare(|row| with_app(|a| a.open_compare(row as usize)));
        ui.on_compare_select(|i| with_app(|a| a.select_compare(i as usize)));
        ui.on_compare_close(|| with_app(App::close_compare));
        ui.on_compare_mode(|preview| with_app(|a| a.set_compare_mode(preview)));
        ui.on_open_source(|| open_in_explorer(env!("CARGO_PKG_REPOSITORY")));
        ui.on_open_releases(|| open_in_explorer(concat!(env!("CARGO_PKG_REPOSITORY"), "/releases")));
        ui.on_open_website(|| open_in_explorer("https://gravitide.dev"));
        ui.window().on_winit_window_event(|_, event| {
            with_app(|a| a.on_window_event(event));
            EventResult::Propagate
        });
    }

    fn on_window_ready(&self) {
        if let Some(hwnd) = self.hwnd() {
            winicon::set_window_icon(hwnd);
        }
        self.tint_title_bar();
    }

    fn on_window_event(&self, event: &winit::event::WindowEvent) {
        use winit::event::WindowEvent as E;
        match event {
            E::HoveredFile(_) => {
                if self.state.borrow().run.is_none() && !self.ui.get_drag_hover() {
                    self.ui.set_drag_hover(true);
                }
            }
            E::HoveredFileCancelled => self.ui.set_drag_hover(false),
            // One event per file: collect them and add them together.
            E::DroppedFile(path) => {
                self.ui.set_drag_hover(false);
                self.dropped.borrow_mut().push(path.clone());
                if !self.drop_flush_queued.replace(true) {
                    slint::Timer::single_shot(Duration::ZERO, || {
                        with_app(|a| {
                            a.drop_flush_queued.set(false);
                            let paths = std::mem::take(&mut *a.dropped.borrow_mut());
                            a.add_paths(paths);
                        })
                    });
                }
            }
            E::ThemeChanged(_) => self.tint_title_bar(),
            _ => {}
        }
    }

    fn hwnd(&self) -> Option<HWND> {
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        self.ui.window().with_winit_window(|w| match w.window_handle().ok()?.as_raw() {
            RawWindowHandle::Win32(h) => Some(h.hwnd.get() as HWND),
            _ => None,
        })?
    }

    /// Title bar in the window's own background color (Cosmic's palette), light or dark.
    fn tint_title_bar(&self) {
        let Some(hwnd) = self.hwnd() else { return };
        let dark = match std::env::var("GIO_THEME").as_deref() {
            Ok("dark") => true,
            Ok("light") => false,
            _ => self.ui.window().with_winit_window(|w| w.theme()).flatten() == Some(winit::window::Theme::Dark),
        };
        if dark {
            winicon::tint_title_bar(hwnd, true, (0x1B, 0x1B, 0x1B), (0xC4, 0xC4, 0xC4));
        } else {
            winicon::tint_title_bar(hwnd, false, (0xD7, 0xD7, 0xD7), (0x29, 0x29, 0x29));
        }
    }

    // -------------------------------------------------------------- dialogs

    fn add_files(&self) {
        let files = self.ui.window().with_winit_window(|w| {
            rfd::FileDialog::new().set_title("Add images").add_filter("Images", &IMAGE_EXTENSIONS).set_parent(w).pick_files()
        });
        if let Some(files) = files.flatten() {
            self.add_paths(files);
        }
    }

    fn add_folder(&self) {
        if let Some(dir) = self.pick_folder("Add a folder of images") {
            self.add_paths(vec![dir]);
        }
    }

    fn browse_output(&self) {
        if let Some(dir) = self.pick_folder("Save optimized images to") {
            self.ui.set_output_dir(dir.display().to_string().into());
            self.ui.set_save_mode(1);
        } else if self.ui.get_output_dir().is_empty() {
            // Nothing chosen: fall back to the default location.
            self.ui.set_save_mode(0);
        }
        self.sync();
    }

    fn pick_folder(&self, title: &str) -> Option<PathBuf> {
        self.ui.window().with_winit_window(|w| rfd::FileDialog::new().set_title(title).set_parent(w).pick_folder()).flatten()
    }

    // -------------------------------------------------------------- jobs

    fn add_paths(&self, paths: Vec<PathBuf>) {
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
                    // Opening a single image shows its preview right away.
                    st.auto_preview = match new[..] {
                        [(id, _)] => Some(id),
                        _ => None,
                    };
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
        self.close_compare();
        self.refresh_rows();
    }

    fn set_selected(&self, row: usize, on: bool) {
        {
            let mut st = self.state.borrow_mut();
            if st.run.is_some() {
                return;
            }
            match st.jobs.get_mut(row) {
                Some(job) => job.selected = on,
                None => return,
            }
        }
        self.update_rows(&[row]);
        self.sync();
    }

    fn select_all(&self, on: bool) {
        for job in self.state.borrow_mut().jobs.iter_mut() {
            job.selected = on;
        }
        self.refresh_rows();
    }

    /// Decode thumbnails on worker threads; results come back through `poll_thumbnails`.
    fn queue_thumbnails(&self, tx: Sender<ThumbMsg>, items: Vec<(u64, PathBuf)>) {
        let size = (THUMB * self.ui.window().scale_factor()).round() as u32;
        let waker = self.thumb_waker.clone();
        std::thread::spawn(move || {
            items.into_par_iter().for_each_with(tx, |tx, (id, path)| {
                let result = optimizer::thumbnail(&path, size).map_err(|e| format!("{e:#}"));
                let _ = tx.send(ThumbMsg { id, result });
                waker.wake();
            });
        });
    }

    fn poll_thumbnails(&self) {
        let msgs: Vec<ThumbMsg> = match &self.state.borrow().thumbs {
            Some(rx) => rx.try_iter().collect(),
            None => return,
        };
        let mut changed = Vec::new();
        {
            let mut st = self.state.borrow_mut();
            for msg in msgs {
                // The job may have been cleared while its thumbnail was being made.
                let Some(i) = st.jobs.iter().position(|j| j.id == msg.id) else { continue };
                match msg.result {
                    Ok(t) => {
                        st.jobs[i].meta = Meta::Read { width: t.width, height: t.height, format: t.format };
                        st.jobs[i].thumb = Some(Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&t.rgba, t.size, t.size)));
                    }
                    Err(_) => st.jobs[i].meta = Meta::Unreadable,
                }
                changed.push(i);
            }
        }
        self.update_rows(&changed);

        // A single image was added: show its preview as soon as it has been read.
        let open = {
            let mut st = self.state.borrow_mut();
            match st.auto_preview.and_then(|id| st.jobs.iter().position(|j| j.id == id)) {
                Some(row) if !matches!(st.jobs[row].meta, Meta::Loading) => {
                    st.auto_preview = None;
                    let readable = matches!(st.jobs[row].meta, Meta::Read { .. });
                    (readable && st.run.is_none()).then_some(row)
                }
                Some(_) => None, // still loading
                None => {
                    st.auto_preview = None; // cleared from the list meanwhile
                    None
                }
            }
        };
        if let Some(row) = open {
            self.open_compare(row);
        }
    }

    fn start(&self) {
        let settings = self.read_settings();
        let custom_without_dir = self.ui.get_save_mode() == 1 && self.ui.get_output_dir().trim().is_empty();
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
                    job.run_dims = match job.meta {
                        Meta::Read { width, height, .. } => optimizer::target_size(width, height, &settings),
                        _ => None,
                    };
                }
                let work: Vec<(usize, Source)> =
                    st.jobs.iter().enumerate().filter(|(_, j)| j.selected).map(|(i, j)| (i, j.source.clone())).collect();
                st.last_output_dir = work.first().map(|(_, s)| optimizer::output_dir(s, &settings));
                let (tx, rx) = channel();
                let cancel = Arc::new(AtomicBool::new(false));
                st.run = Some(Run { rx, cancel: cancel.clone(), started: Instant::now(), total: work.len() });
                st.elapsed = None;
                st.notice.clear();
                let waker = self.run_waker.clone();
                let worker_settings = settings.clone();
                std::thread::spawn(move || run_jobs(work, worker_settings, cancel, tx, waker));
                None
            }
        };
        match notice {
            Some(n) => {
                self.state.borrow_mut().notice = n.into();
                self.sync();
            }
            None => {
                save_settings(&settings);
                // The run rewrites the outputs being compared.
                self.close_compare();
                self.taskbar_progress(0, 1);
                self.refresh_rows();
                #[cfg(any(debug_assertions, feature = "snapshot"))]
                snapshot::on_run_started();
            }
        }
    }

    fn poll_messages(&self) {
        let mut changed: Vec<usize> = Vec::new();
        let mut finished = false;
        let progress: (u64, u64);
        let clear_when_done = self.ui.get_clear_when_done();
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
                let secs = run.started.elapsed().as_secs_f32();
                st.elapsed = Some(secs);
                let cancelled = run.cancel.load(Ordering::SeqCst);
                if cancelled {
                    st.notice = "Cancelled.".into();
                    for job in st.jobs.iter_mut().filter(|j| matches!(j.status, Status::Running)) {
                        job.status = Status::Pending;
                    }
                }
                if clear_when_done {
                    clear_optimized(&mut st, secs, cancelled);
                }
            }
        }
        if finished {
            if let Some(t) = self.taskbar.borrow().as_ref() {
                t.clear();
            }
            if let Some(hwnd) = self.hwnd() {
                taskbar::flash_if_background(hwnd);
            }
            // Cancelled images go back to waiting, so rebuild every tile.
            self.refresh_rows();
            #[cfg(any(debug_assertions, feature = "snapshot"))]
            snapshot::on_run_finished();
        } else {
            self.taskbar_progress(progress.0, progress.1);
            self.update_rows(&changed);
            self.sync();
        }
    }

    fn taskbar_progress(&self, done: u64, total: u64) {
        let mut taskbar = self.taskbar.borrow_mut();
        if taskbar.is_none() {
            *taskbar = self.hwnd().and_then(Taskbar::new);
        }
        if let Some(t) = taskbar.as_ref() {
            t.progress(done, total);
        }
    }

    // -------------------------------------------------------------- before / after

    /// Open the comparison for an image: its saved outputs if it has any that can be shown,
    /// otherwise a live preview of the current settings.
    fn open_compare(&self, row: usize) {
        let compare = {
            let st = self.state.borrow();
            let Some(job) = st.jobs.get(row) else { return };
            let Meta::Read { width, height, .. } = job.meta else { return };
            let saved = compare_outputs(job);
            Compare {
                name: job.file_name.clone(),
                original: job.source.path.clone(),
                original_size: job.original_size,
                width,
                height,
                preview: saved.is_empty(),
                saved,
                formats: Vec::new(),
                current: None,
                decoded: None,
                original_shown: false,
                in_flight: None,
                dirty: false,
            }
        };
        let ui = &self.ui;
        ui.set_compare_name(compare.name.as_str().into());
        ui.set_compare_width(compare.width as i32);
        ui.set_compare_height(compare.height as i32);
        ui.set_compare_original_size(human_size(compare.original_size).into());
        ui.set_compare_original_dims(format!("{} × {}", compare.width, compare.height).into());
        ui.set_compare_has_saved(!compare.saved.is_empty());
        ui.set_compare_original(Image::default());
        ui.set_compare_output(Image::default());
        ui.set_compare_output_label("".into());
        self.clear_compare_output_figures();
        ui.set_compare_zoom(0.0);
        ui.set_compare_split(0.5);
        self.state.borrow_mut().compare = Some(compare);
        ui.set_compare_open(true);
        self.refresh_compare();
    }

    /// Blank the output side of the comparison footer.
    fn clear_compare_output_figures(&self) {
        let ui = &self.ui;
        ui.set_compare_output_caption("".into());
        ui.set_compare_output_size("".into());
        ui.set_compare_output_dims("".into());
        ui.set_compare_saving("".into());
        ui.set_compare_saving_detail("".into());
        ui.set_compare_output_tone(0);
    }

    /// Switch between the saved outputs and the live preview.
    fn set_compare_mode(&self, preview: bool) {
        match self.state.borrow_mut().compare.as_mut() {
            Some(c) if c.preview != preview => c.preview = preview,
            _ => return,
        }
        self.refresh_compare();
    }

    fn select_compare(&self, index: usize) {
        {
            let mut st = self.state.borrow_mut();
            let Some(c) = st.compare.as_mut() else { return };
            if c.current == Some(index) || index >= c.formats.len() {
                return;
            }
            c.current = Some(index);
        }
        self.ui.set_compare_current(index as i32);
        self.load_compare();
    }

    /// Settings changed: re-encode the preview once they settle (sliders send many changes).
    fn on_settings_changed(&self) {
        self.sync();
        if self.state.borrow().compare.as_ref().is_some_and(|c| c.preview) {
            self.preview_timer.start(slint::TimerMode::SingleShot, Duration::from_millis(250), || {
                with_app(App::refresh_compare)
            });
        }
    }

    /// Rebuild the format chips for the mode and settings, keeping the chosen format when it's
    /// still there, then load what's chosen.
    fn refresh_compare(&self) {
        let settings = self.read_settings();
        let (chips, current, preview) = {
            let mut st = self.state.borrow_mut();
            let Some(c) = st.compare.as_mut() else { return };
            let previous = c.current.and_then(|i| c.formats.get(i).copied());
            c.formats = if c.preview { settings.formats() } else { c.saved.iter().map(|o| o.format).collect() };
            c.current = previous
                .and_then(|p| c.formats.iter().position(|f| *f == p))
                .or((!c.formats.is_empty()).then_some(0));
            let chips: Vec<CompareFormat> = c
                .formats
                .iter()
                .map(|&format| CompareFormat {
                    label: format.label().into(),
                    detail: match (c.preview, c.saved.iter().find(|o| o.format == format)) {
                        (false, Some(o)) => human_size(o.size).into(),
                        _ => "".into(),
                    },
                })
                .collect();
            (chips, c.current, c.preview)
        };
        let ui = &self.ui;
        ui.set_compare_formats(ModelRc::new(VecModel::from(chips)));
        ui.set_compare_current(current.map_or(-1, |i| i as i32));
        ui.set_compare_preview(preview);
        self.load_compare();
    }

    fn close_compare(&self) {
        if self.state.borrow_mut().compare.take().is_none() {
            return;
        }
        self.preview_timer.stop();
        let ui = &self.ui;
        ui.set_compare_open(false);
        // Free the full-size pictures.
        ui.set_compare_original(Image::default());
        ui.set_compare_output(Image::default());
    }

    /// Decode the original (once) and produce the chosen output on a worker thread: read the
    /// saved file, or encode a preview in memory. One worker at a time per comparison.
    fn load_compare(&self) {
        let settings = self.read_settings();
        let request = self.compare_request.get() + 1;
        let (path, decoded, need_original, job, busy) = {
            let mut st = self.state.borrow_mut();
            let Some(c) = st.compare.as_mut() else { return };
            if c.in_flight.is_some() {
                c.dirty = true;
                return;
            }
            c.in_flight = Some(request);
            c.dirty = false;
            let job = match c.current.map(|i| c.formats[i]) {
                None => CompareJob::Nothing,
                Some(format) if c.preview => CompareJob::Preview { format, settings },
                Some(format) => match c.saved.iter().find(|o| o.format == format) {
                    Some(o) => CompareJob::Saved { path: o.path.clone(), size: o.size },
                    None => CompareJob::Nothing,
                },
            };
            let busy = if !c.original_shown {
                "Loading…"
            } else if matches!(job, CompareJob::Preview { .. }) {
                "Updating preview…"
            } else {
                "Loading…"
            };
            (c.original.clone(), c.decoded.clone(), !c.original_shown, job, busy)
        };
        self.compare_request.set(request);
        let ui = &self.ui;
        ui.set_compare_busy(if matches!(job, CompareJob::Nothing) && !need_original { "".into() } else { busy.into() });
        std::thread::spawn(move || {
            let text = |e: anyhow::Error| format!("{e:#}");
            let decoded = match decoded {
                Some(d) => Ok(d),
                None => optimizer::Decoded::open(&path).map(Arc::new).map_err(text),
            };
            let original = if need_original { decoded.as_ref().ok().map(|d| pixel_buffer(d.rgba())) } else { None };
            let output = match job {
                CompareJob::Nothing => None,
                CompareJob::Saved { path, size } => {
                    Some(optimizer::load_rgba(&path).map(|p| (pixel_buffer(p), size)).map_err(text))
                }
                CompareJob::Preview { format, settings } => Some(match &decoded {
                    Ok(d) => optimizer::preview(d, format, &settings).map(|(size, p)| (pixel_buffer(p), size)).map_err(text),
                    Err(e) => Err(e.clone()),
                }),
            };
            let result = CompareResult { request, original_path: path, decoded, original, output };
            let _ = slint::invoke_from_event_loop(move || with_app(|a| a.compare_loaded(result)));
        });
    }

    fn compare_loaded(&self, result: CompareResult) {
        let settings = self.read_settings();
        let (label, original_size, preview, again) = {
            let mut st = self.state.borrow_mut();
            // Closed, or another image opened since.
            let Some(c) = st.compare.as_mut().filter(|c| c.original == result.original_path) else { return };
            if c.in_flight == Some(result.request) {
                c.in_flight = None;
            }
            if let Ok(d) = &result.decoded {
                c.decoded.get_or_insert_with(|| d.clone());
            }
            let format = c.current.map(|i| c.formats[i]);
            let label = match format {
                Some(f) if c.preview => format_setting(f, &settings),
                Some(f) => f.label().to_string(),
                None => String::new(),
            };
            (label, c.original_size, c.preview, c.dirty)
        };
        let ui = &self.ui;
        let mut problem = None;
        if let Some(buffer) = result.original {
            ui.set_compare_original(Image::from_rgba8(buffer));
            if let Some(c) = self.state.borrow_mut().compare.as_mut() {
                c.original_shown = true;
            }
        } else if let Err(e) = &result.decoded {
            problem = Some(format!("Couldn't open the original: {e}"));
        }
        match result.output {
            Some(Ok((buffer, size))) => {
                let larger = size >= original_size;
                let pct = savings_pct(original_size, size).abs().round();
                let (saving, detail) = if larger {
                    let detail = if preview && settings.skip_if_larger {
                        "would be skipped".to_string()
                    } else {
                        format!("{} larger", human_size(size - original_size))
                    };
                    (format!("+{pct:.0}%"), detail)
                } else {
                    (format!("\u{2212}{pct:.0}%"), format!("saves {}", human_size(original_size - size)))
                };
                let caption = if preview { format!("{label} · not saved yet") } else { format!("{label} · saved") };
                ui.set_compare_output_caption(caption.into());
                ui.set_compare_output_size(human_size(size).into());
                ui.set_compare_output_dims(format!("{} × {}", buffer.width(), buffer.height()).into());
                ui.set_compare_saving(saving.into());
                ui.set_compare_saving_detail(detail.into());
                ui.set_compare_output_tone(if larger { 2 } else { 1 });
                ui.set_compare_output_label(if preview { format!("Preview: {label}") } else { label }.into());
                ui.set_compare_output(Image::from_rgba8(buffer));
            }
            Some(Err(e)) => problem = problem.or(Some(format!("Couldn't show the {label} output: {e}"))),
            None => {
                ui.set_compare_output(Image::default());
                ui.set_compare_output_label("".into());
                self.clear_compare_output_figures();
                problem = problem.or(Some(if preview {
                    "Turn on an output format to preview".into()
                } else {
                    "None of the saved outputs can be shown".into()
                }));
            }
        }
        ui.set_compare_message(problem.unwrap_or_default().into());
        ui.set_compare_busy("".into());
        if again {
            // Settings or the choice changed while this was being made.
            self.load_compare();
        }
    }

    // -------------------------------------------------------------- view

    /// Rebuild every tile from the jobs.
    fn refresh_rows(&self) {
        let settings = self.read_settings();
        let plan = plan_text(&settings);
        let views: Vec<TileData> = {
            let mut st = self.state.borrow_mut();
            st.plan = plan;
            st.jobs.iter().map(|j| tile_view(j, &st.plan, &settings)).collect()
        };
        self.tiles.set_vec(views);
        self.sync();
    }

    /// Update the given tiles.
    fn update_rows(&self, rows: &[usize]) {
        use slint::Model;
        let settings = self.read_settings();
        let views: Vec<(usize, TileData)> = {
            let st = self.state.borrow();
            rows.iter().filter(|&&i| i < st.jobs.len()).map(|&i| (i, tile_view(&st.jobs[i], &st.plan, &settings))).collect()
        };
        for (row, view) in views {
            if row < self.tiles.row_count() {
                self.tiles.set_row_data(row, view);
            }
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

    /// Bring the buttons, footer and hints up to date with the settings and run state.
    fn sync(&self) {
        self.refresh_plan();
        let ui = &self.ui;
        let settings = self.read_settings();
        let (running, has_output, status, selected, total, progress) = {
            let st = self.state.borrow();
            let done = st.jobs.iter().filter(|j| j.selected && matches!(j.status, Status::Done(_) | Status::Failed(_))).count();
            let run_total = st.run.as_ref().map_or(0, |r| r.total);
            (
                st.run.is_some(),
                st.last_output_dir.as_ref().is_some_and(|d| d.exists()),
                status_text(&st),
                st.jobs.iter().filter(|j| j.selected).count(),
                st.jobs.len(),
                if run_total == 0 { 0.0 } else { done as f32 / run_total as f32 },
            )
        };
        let idle = !running;
        ui.set_running(running);
        ui.set_can_clear(idle && total > 0);
        ui.set_can_select_all(idle && selected < total);
        ui.set_can_select_none(idle && selected > 0);
        ui.set_optimize_label(
            match (selected, total) {
                (0, _) => "Optimize".to_string(),
                (s, t) if s == t => if t == 1 { "Optimize".to_string() } else { format!("Optimize all {t}") },
                (s, _) => format!("Optimize {s} selected"),
            }
            .into(),
        );
        ui.set_can_optimize(idle && selected > 0 && !settings.formats().is_empty());
        ui.set_progress(progress);
        ui.set_has_output(has_output);
        ui.set_status(status.into());
        ui.set_resize_hint(
            match self.resize_choice() {
                ResizeChoice::Size(_, w, h) if w == h => format!("Fits within {w} × {h}. Never enlarges."),
                ResizeChoice::Size(_, w, h) => {
                    format!("Fits within {w} × {h}, or {h} × {w} for portrait images. Never enlarges.")
                }
                ResizeChoice::Percent(_) | ResizeChoice::CustomPercent => {
                    let pct = settings.resize_percent;
                    let side = |s: u32| ((s as f64 * pct as f64 / 100.0).round() as u32).max(1);
                    format!("Example: 4000 × 3000 → {} × {}", side(4000), side(3000))
                }
                ResizeChoice::CustomSize => String::new(),
            }
            .into(),
        );
    }

    /// The resize dropdown's current choice.
    fn resize_choice(&self) -> ResizeChoice {
        RESIZE_CHOICES.get(self.ui.get_resize_mode() as usize).copied().unwrap_or(ResizeChoice::CustomSize)
    }

    // -------------------------------------------------------------- settings

    fn apply_settings(&self, s: &Settings) {
        let ui = &self.ui;
        ui.set_jpeg(s.jpeg);
        ui.set_webp(s.webp);
        ui.set_avif(s.avif);
        ui.set_png(s.png);
        ui.set_jpeg_quality(s.jpeg_quality as f32);
        ui.set_webp_quality(s.webp_quality as f32);
        ui.set_webp_lossless(s.webp_lossless);
        ui.set_avif_quality(s.avif_quality as f32);
        ui.set_avif_speed(s.avif_speed as f32);
        ui.set_png_level(s.png_level as f32);
        ui.set_resize(s.resize);
        let percent = s.resize_percent.clamp(1, 100);
        let index_of = |wanted: ResizeChoice| RESIZE_CHOICES.iter().position(|c| *c == wanted).unwrap_or(0);
        let saved = RESIZE_CHOICES.iter().position(|c| match (*c, s.resize_mode) {
            (ResizeChoice::Size(_, w, h), ResizeMode::Box) => (w, h) == (s.box_width, s.box_height),
            (ResizeChoice::Percent(p), ResizeMode::Percent) => p == percent,
            (ResizeChoice::CustomSize, ResizeMode::FitWithin) => true,
            _ => false,
        });
        ui.set_resize_mode(match (saved, s.resize_mode) {
            (Some(i), _) => i,
            (None, ResizeMode::Percent) => index_of(ResizeChoice::CustomPercent),
            (None, _) => index_of(ResizeChoice::CustomSize),
        } as i32);
        ui.set_percent(percent.to_string().into());
        ui.set_limit_width(s.max_width.to_string().into());
        ui.set_limit_height(s.max_height.to_string().into());
        match &s.output {
            OutputLocation::Subfolder => ui.set_save_mode(0),
            OutputLocation::Custom(dir) => {
                ui.set_save_mode(1);
                ui.set_output_dir(dir.display().to_string().into());
            }
        }
        ui.set_skip_larger(s.skip_if_larger);
        ui.set_overwrite(s.overwrite);
        ui.set_clear_when_done(s.clear_when_done);
    }

    fn read_settings(&self) -> Settings {
        let ui = &self.ui;
        let num = |t: slint::SharedString, default: u32| t.trim().parse::<u32>().unwrap_or(default);
        let custom_percent = num(ui.get_percent(), 50).clamp(1, 100);
        let defaults = Settings::default();
        let (resize_mode, resize_percent, (box_width, box_height)) = match self.resize_choice() {
            ResizeChoice::Size(_, w, h) => (ResizeMode::Box, custom_percent, (w, h)),
            ResizeChoice::Percent(p) => (ResizeMode::Percent, p, (defaults.box_width, defaults.box_height)),
            ResizeChoice::CustomSize => (ResizeMode::FitWithin, custom_percent, (defaults.box_width, defaults.box_height)),
            ResizeChoice::CustomPercent => (ResizeMode::Percent, custom_percent, (defaults.box_width, defaults.box_height)),
        };
        let slider = |v: f32, lo: f32, hi: f32| v.round().clamp(lo, hi) as u8;
        let dir = ui.get_output_dir().trim().to_string();
        Settings {
            jpeg: ui.get_jpeg(),
            webp: ui.get_webp(),
            avif: ui.get_avif(),
            png: ui.get_png(),
            jpeg_quality: slider(ui.get_jpeg_quality(), 1.0, 100.0),
            webp_quality: slider(ui.get_webp_quality(), 1.0, 100.0),
            webp_lossless: ui.get_webp_lossless(),
            avif_quality: slider(ui.get_avif_quality(), 1.0, 100.0),
            avif_speed: slider(ui.get_avif_speed(), 1.0, 10.0),
            png_level: slider(ui.get_png_level(), 0.0, 6.0),
            resize: ui.get_resize(),
            resize_mode,
            resize_percent,
            box_width,
            box_height,
            max_width: num(ui.get_limit_width(), 0),
            max_height: num(ui.get_limit_height(), 0),
            output: if ui.get_save_mode() == 1 && !dir.is_empty() {
                OutputLocation::Custom(PathBuf::from(dir))
            } else {
                OutputLocation::Subfolder
            },
            skip_if_larger: ui.get_skip_larger(),
            overwrite: ui.get_overwrite(),
            clear_when_done: ui.get_clear_when_done(),
        }
    }
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
        thumb: None,
        meta: Meta::Loading,
        run_dims: None,
    }
}

/// Tile tones and badges (see `TileData` in ui/app.slint).
const TONE_NORMAL: i32 = 0;
const TONE_SUCCESS: i32 = 1;
const TONE_CAUTION: i32 = 2;
const TONE_CRITICAL: i32 = 3;
const BADGE_NONE: i32 = 0;
const BADGE_DONE: i32 = 1;
const BADGE_RUNNING: i32 = 2;
const BADGE_SKIPPED: i32 = 3;
const BADGE_ERROR: i32 = 4;

/// `settings` are the current ones: images that haven't run yet show what they would produce.
fn tile_view(job: &Job, plan: &str, settings: &Settings) -> TileData {
    let results: &[FormatResult] = match &job.status {
        Status::Done(r) => r,
        _ => &[],
    };
    let details = match job.meta {
        Meta::Loading => "Reading…",
        Meta::Read { format, .. } => format,
        Meta::Unreadable => "Can't read this image",
    };

    // Original / Output columns.
    let dims = |(w, h): (u32, u32)| format!("{w} × {h}");
    let (orig_dims, out_dims) = match job.meta {
        Meta::Read { width, height, .. } => {
            // Waiting images follow the current settings; others show what their run used.
            let target = match job.status {
                Status::Pending => optimizer::target_size(width, height, settings),
                _ => job.run_dims,
            };
            (dims((width, height)), target)
        }
        Meta::Loading => ("…".to_string(), None),
        Meta::Unreadable => ("—".to_string(), None),
    };
    let smallest = results
        .iter()
        .filter_map(|r| match r.outcome {
            Outcome::Written { size, .. } => Some(size),
            _ => None,
        })
        .min();
    let (out_size, out_tone) = match (&job.status, smallest) {
        (Status::Done(_), Some(size)) => (human_size(size), TONE_SUCCESS),
        (Status::Done(_), None) if results.iter().any(|r| matches!(r.outcome, Outcome::Larger { .. })) => {
            ("skipped".to_string(), TONE_CAUTION)
        }
        (Status::Done(_), None) | (Status::Failed(_), _) => ("failed".to_string(), TONE_CRITICAL),
        (Status::Running, _) => ("…".to_string(), TONE_NORMAL),
        (Status::Pending, _) => ("—".to_string(), TONE_NORMAL),
    };

    let written = results.iter().any(|r| matches!(r.outcome, Outcome::Written { .. }));
    let error = row_error(job);
    let (output, tone, badge) = match &job.status {
        Status::Pending if !job.selected => ("Not selected".to_string(), TONE_NORMAL, BADGE_NONE),
        Status::Pending => (plan.to_string(), TONE_NORMAL, BADGE_NONE),
        Status::Running => ("Optimizing…".to_string(), TONE_NORMAL, BADGE_RUNNING),
        Status::Failed(_) => ("Couldn't optimize this image".to_string(), TONE_CRITICAL, BADGE_ERROR),
        Status::Done(_) => {
            let (tone, badge) = if error.is_some() {
                (TONE_CRITICAL, BADGE_ERROR)
            } else if written {
                (TONE_SUCCESS, BADGE_DONE)
            } else {
                (TONE_CAUTION, BADGE_SKIPPED)
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
    TileData {
        name: job.file_name.as_str().into(),
        details: details.into(),
        output: output.into(),
        location: location.into(),
        tone,
        badge,
        running: matches!(job.status, Status::Running),
        selected: job.selected,
        thumb: job.thumb.clone().unwrap_or_default(),
        has_thumb: job.thumb.is_some(),
        // Any readable image: a preview before optimizing, the saved outputs after.
        can_compare: matches!(job.meta, Meta::Read { .. }),
        orig_dims: orig_dims.into(),
        orig_size: human_size(job.original_size).into(),
        out_resized: out_dims.is_some(),
        out_dims: out_dims.map_or_else(|| "same".to_string(), dims).into(),
        out_size: out_size.into(),
        out_tone,
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

/// "WebP at 80%", "PNG lossless": a format with its main setting (quality as a percentage).
fn format_setting(format: Format, s: &Settings) -> String {
    match format {
        Format::Jpeg => format!("JPEG at {}%", s.jpeg_quality),
        Format::WebP if s.webp_lossless => "WebP lossless".into(),
        Format::WebP => format!("WebP at {}%", s.webp_quality),
        Format::Avif => format!("AVIF at {}%", s.avif_quality),
        Format::Png => "PNG lossless".into(),
    }
}

/// "To WebP at 78%, AVIF at 65% · resized to 50%": what the current settings will produce.
fn plan_text(s: &Settings) -> String {
    let formats: Vec<String> = s.formats().into_iter().map(|f| format_setting(f, s)).collect();
    if formats.is_empty() {
        return "No output format selected".into();
    }
    let size = match (s.resize, s.resize_mode) {
        (false, _) => "original size".to_string(),
        (true, ResizeMode::Percent) => format!("resized to {}%", s.resize_percent),
        (true, ResizeMode::Box) => format!("fit {} × {}", s.box_width, s.box_height),
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

/// "Clear images from the list once optimized": images that finished without errors leave
/// the list (failures stay, so their errors can be read; unticked images weren't part of the
/// run). Their totals move into the status line, since their tiles are gone.
fn clear_optimized(st: &mut State, secs: f32, cancelled: bool) {
    let (cleared, kept): (Vec<Job>, Vec<Job>) =
        std::mem::take(&mut st.jobs).into_iter().partition(|j| matches!(j.status, Status::Done(_)) && row_error(j).is_none());
    st.jobs = kept;
    if cleared.is_empty() {
        return;
    }
    for job in &cleared {
        st.known.remove(&job.source.path);
    }
    // The run's time goes in this summary, not after the totals of what's left.
    st.elapsed = None;
    let n = cleared.len();
    // "Cleared 2 optimized images · saved 143.8 KB (77%) in 0.2s"
    let mut notice = format!(
        "{}Cleared {n} optimized image{}",
        if cancelled { "Cancelled. " } else { "" },
        if n == 1 { "" } else { "s" },
    );
    if let Some((orig, best)) = totals(&cleared) {
        let saved = orig.saturating_sub(best);
        let pct = if orig == 0 { 0.0 } else { saved as f64 * 100.0 / orig as f64 };
        notice.push_str(&format!(" · saved {} ({pct:.0}%)", human_size(saved)));
    }
    notice.push_str(&format!(" in {secs:.1}s"));
    st.notice = notice;
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

fn run_jobs(work: Vec<(usize, Source)>, settings: Settings, cancel: Arc<AtomicBool>, tx: Sender<Msg>, waker: Waker) {
    work.into_par_iter().for_each_with(tx.clone(), |tx, (i, source)| {
        if cancel.load(Ordering::SeqCst) {
            return;
        }
        let _ = tx.send(Msg::Started(i));
        waker.wake();
        let result = optimizer::process(&source, &settings).map_err(|e| format!("{e:#}"));
        let _ = tx.send(Msg::Finished(i, result));
        waker.wake();
    });
    let _ = tx.send(Msg::AllDone);
    waker.wake();
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

/// Open a folder in Explorer, or a web address in the default browser.
fn open_in_explorer(target: impl AsRef<std::ffi::OsStr>) {
    let _ = std::process::Command::new("explorer").arg(target).spawn();
}

/// Debug self-check: with `GIO_SNAPSHOT=<file.png>` the app renders its own window to a PNG
/// and exits.
/// - `GIO_SNAPSHOT_RUN=1` optimizes the loaded files first (`GIO_SNAPSHOT_MIDRUN=<ms>`
///   captures that long after the run starts instead of at the end);
/// - `GIO_SNAPSHOT_COMPARE=<row>` (with `GIO_SNAPSHOT_RUN`) opens the before / after view for
///   that row once the run is done; `GIO_SNAPSHOT_ZOOM=50` shows it at 50% (default Fit),
///   `GIO_SNAPSHOT_SPLIT=0.3` moves the split line;
/// - `GIO_SNAPSHOT_ALL_ON=1` switches every format and resizing on first;
/// - `GIO_THEME=dark|light` forces a theme.
#[cfg(any(debug_assertions, feature = "snapshot"))]
mod snapshot {
    use super::*;

    fn var(name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|v| !v.is_empty())
    }

    pub fn schedule() {
        if var("GIO_SNAPSHOT").is_none() {
            return;
        }
        if var("GIO_SNAPSHOT_ABOUT").is_some() {
            with_app(|a| {
                a.ui.set_about_open(true);
                a.ui.set_about_licenses(var("GIO_SNAPSHOT_ABOUT").as_deref() == Some("licenses"));
            });
        }
        if var("GIO_SNAPSHOT_ALL_ON").is_some() {
            with_app(|a| {
                let ui = &a.ui;
                for set in [AppWindow::set_webp, AppWindow::set_avif, AppWindow::set_jpeg, AppWindow::set_png, AppWindow::set_resize] {
                    set(ui, true);
                }
                a.sync();
            });
        }
        // GIO_SNAPSHOT_RESIZE=<n>: resizing on, with the n-th entry of the resize dropdown.
        // GIO_SNAPSHOT_CLEAR=1: "Clear images from the list once optimized" on.
        if var("GIO_SNAPSHOT_CLEAR").is_some() {
            with_app(|a| a.ui.set_clear_when_done(true));
        }
        if let Some(index) = var("GIO_SNAPSHOT_RESIZE").and_then(|v| v.parse::<i32>().ok()) {
            with_app(|a| {
                a.ui.set_resize(true);
                a.ui.set_resize_mode(index);
                a.sync();
            });
        }
        // Give thumbnails time to load and the window time to render.
        slint::Timer::single_shot(Duration::from_millis(1500), || {
            if var("GIO_SNAPSHOT_RUN").is_some() {
                with_app(App::start);
            } else if !compare_then_capture() {
                capture();
            }
        });
    }

    pub fn on_run_started() {
        if let Some(ms) = var("GIO_SNAPSHOT_MIDRUN").and_then(|v| v.parse::<u64>().ok()) {
            slint::Timer::single_shot(Duration::from_millis(ms), capture);
        }
    }

    pub fn on_run_finished() {
        if var("GIO_SNAPSHOT").is_none() || var("GIO_SNAPSHOT_MIDRUN").is_some() {
            return;
        }
        if !compare_then_capture() {
            // Let the final state render first.
            slint::Timer::single_shot(Duration::from_millis(400), capture);
        }
    }

    /// With `GIO_SNAPSHOT_COMPARE=<row>`: open the before / after view, optionally retune the
    /// preview (`GIO_SNAPSHOT_RETUNE=<WebP and JPEG quality>`), then capture. Returns whether
    /// it did.
    fn compare_then_capture() -> bool {
        let Some(row) = var("GIO_SNAPSHOT_COMPARE").and_then(|v| v.parse::<usize>().ok()) else { return false };
        with_app(|a| {
            a.open_compare(row);
            // GIO_SNAPSHOT_COMPARE_FORMAT=<n>: the n-th format chip (e.g. 2 = AVIF with every format on).
            if let Some(chip) = var("GIO_SNAPSHOT_COMPARE_FORMAT").and_then(|v| v.parse::<usize>().ok()) {
                a.select_compare(chip);
            }
            let zoom = var("GIO_SNAPSHOT_ZOOM").and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
            a.ui.set_compare_zoom(zoom / 100.0);
            if let Some(split) = var("GIO_SNAPSHOT_SPLIT").and_then(|v| v.parse::<f32>().ok()) {
                a.ui.set_compare_split(split);
            }
        });
        let retune = var("GIO_SNAPSHOT_RETUNE").and_then(|v| v.parse::<f32>().ok());
        if let Some(quality) = retune {
            // Change settings while the preview is open, as moving a slider would.
            slint::Timer::single_shot(Duration::from_millis(1500), move || {
                with_app(|a| {
                    a.ui.set_webp_quality(quality);
                    a.ui.set_jpeg_quality(quality);
                    a.on_settings_changed();
                })
            });
        }
        // Decoding and encoding full-size images takes a moment.
        let wait = if retune.is_some() { 4000 } else { 2000 };
        slint::Timer::single_shot(Duration::from_millis(wait), capture);
        true
    }

    fn capture() {
        let Some(path) = var("GIO_SNAPSHOT") else { return };
        with_app(|a| {
            match a.ui.window().take_snapshot() {
                Ok(buf) => {
                    let (w, h) = (buf.width(), buf.height());
                    match image::RgbaImage::from_raw(w, h, buf.as_bytes().to_vec()).map(|img| img.save(&path)) {
                        Some(Ok(())) => eprintln!("snapshot: wrote {path}"),
                        Some(Err(e)) => eprintln!("snapshot: {e}"),
                        None => eprintln!("snapshot: bad buffer"),
                    }
                }
                Err(e) => eprintln!("snapshot: {e}"),
            }
        });
        let _ = slint::quit_event_loop();
    }
}
