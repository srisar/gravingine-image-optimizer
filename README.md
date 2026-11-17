# Gravingine Image Optimizer

A Windows image optimizer written in Rust, with a [Slint](https://slint.dev) UI in Slint's
Cosmic style. Drag & drop images or whole folders, pick the web formats you want, and get
smaller files.

## Features

- **Drag & drop** files or folders onto the window (or onto the `.exe` icon), or use *Add files* / *Add folder*.
- Folders are scanned recursively; the folder structure is preserved in the output.
- **Output formats** (any combination, all encoded in one pass):
  | Format | Encoder | Notes |
  |---|---|---|
  | JPEG | mozjpeg (progressive, optimized scans) | Universal; transparency flattened onto white |
  | WebP | libwebp (lossy or lossless) | Supports transparency |
  | AVIF | ravif / rav1e | Smallest files, slower to encode |
  | PNG  | oxipng (lossless) | Re-compresses PNGs in place when not resized |
- **Resize** to fit a maximum width/height, or scale to 75%, 50%, 25% or a custom percentage (aspect ratio kept, never upscales).
- Strips metadata (EXIF/GPS) and bakes in EXIF orientation.
- *Skip outputs larger than the original* so you never end up with a worse file.
- File list as tiles: thumbnail with a status badge, file name and format, what will be produced (or the colored result) and where it was saved, with a progress bar while optimizing. On the right, *Original* and *Output* columns: dimensions and file size before, and the resized dimensions and smallest saved size after. Tick the images to process; *Select all* / *Select none*.
- Parallel processing on all CPU cores, cancellable, with per-file and total savings, progress on the taskbar button, and a taskbar flash when a run finishes in the background.
- Settings are remembered between runs (`%APPDATA%\Gravingine Image Optimizer\settings.json`).
- Slint UI (Cosmic style):
  - follows Windows' **light/dark mode**, live, and tints its accent from the Windows **accent color**; the title bar matches the window;
  - a scrolling settings pane: each output format (and resizing) is its own card with a tinted icon, a short description and a switch; a format's sliders slide open inside its card while it is switched on;
  - the file list highlights while files are dragged over the window;
  - scales per monitor; icons are glyphs from the Windows icon font (Segoe Fluent Icons, or Segoe MDL2 Assets on Windows 10), so no image files are shipped.

Inputs: jpg, jpeg, jfif, png, webp, gif, bmp, tif, tiff, ico.

## Output location

- **Default:** an `optimized` folder next to the images (for a dropped folder: `<folder>/optimized/…`).
  `optimized` folders are ignored when scanning, so re-dropping a folder is safe.
- **Custom folder:** `<custom>/<dropped folder name>/…`.

## Build & run

Requires the Rust toolchain (MSVC on Windows). `nasm` on `PATH` speeds up the JPEG/AVIF encoders.

```bash
cargo run --release
```

The binary is at `target/release/gravingine_image_optimizer.exe`.

```bash
cargo test     # encoder + pipeline tests
```

## Project layout

- `ui/app.slint` – the whole window: settings cards, file list tiles, toolbar and footer (compiled by `build.rs` with the Cosmic style)
- `src/app.rs` – app state and behavior: adding images, thumbnails, selection, runs, window events
- `src/optimizer.rs` – decoding, resizing and encoding
- `src/taskbar.rs` – taskbar button progress and flash
- `src/winicon.rs` – app icon, icon-font detection and title bar tint (Win32)
- `res/app.manifest` – embedded by `build.rs`: Common Controls v6 (system dialogs) and per-monitor DPI awareness

The Win32 (native-windows-gui) version of the UI is kept on the `win32-ui` branch.

Debug builds can render their own window to a PNG and exit, which is handy for checking the layout
(snapshot runs never save settings):

```bash
GIO_SNAPSHOT=window.png cargo run -- "D:\Photos"
GIO_SNAPSHOT=window.png GIO_SNAPSHOT_RUN=1 cargo run -- "D:\Photos"   # optimize first, then capture
GIO_SNAPSHOT=window.png GIO_SNAPSHOT_RUN=1 GIO_SNAPSHOT_MIDRUN=300 cargo run -- "D:\Photos"   # capture while optimizing
GIO_SNAPSHOT=window.png GIO_SNAPSHOT_ALL_ON=1 cargo run   # switch every format and resizing on first
GIO_THEME=dark cargo run   # force dark (or light) instead of following Windows
cargo build --release --features snapshot   # the same self-snapshots in an optimized build
```
