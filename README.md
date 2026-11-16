# Gravingine Image Optimizer

A native Windows image optimizer written in Rust, with a classic Win32 UI built from the real
Windows common controls ([native-windows-gui](https://github.com/gabdube/native-windows-gui)).
Drag & drop images or whole folders, pick the web formats you want, and get smaller files.

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
- File list as tiles: thumbnail with a status badge, bold file name, image size and format, what will be produced (or the colored result), and where it was saved, with a progress bar while optimizing. Tick the images to process; *Select all* / *Select none*.
- Parallel processing on all CPU cores, cancellable, with per-file and total savings, progress on the taskbar button, and a taskbar flash when a run finishes in the background.
- Settings are remembered between runs (`%APPDATA%\Gravingine Image Optimizer\settings.json`).
- Windows 11 look built on real Win32 controls:
  - follows Windows' **light/dark mode** and **accent color**, live (dark title bar, dark scrollbars, fields and dropdowns);
  - a scrolling settings pane: each output format (and resizing) is its own card with a tinted icon, a short description and a toggle switch; a format's sliders appear inside its card only while it is switched on;
  - Windows 11 style toggles, checkboxes and sliders (Segoe UI Variable), an accent *Optimize* button, tooltips on every setting;
  - the file list highlights while files are dragged over the window;
  - DPI aware; icons drawn at runtime from the Windows icon font (Segoe Fluent Icons, or Segoe MDL2 Assets on Windows 10), so no image files are shipped.

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

- `src/app.rs` – app state and behavior: adding images, thumbnails, selection, runs, window events
- `src/ui.rs` – building and laying out the controls; applying the theme
- `src/theme.rs` – light/dark palette and accent color from Windows, dark mode for Win32 controls, title bar, fonts
- `src/skin.rs` – window background, settings pane cards and icon tiles, control colors, custom-drawn toggles, checkboxes, sliders and accent button
- `src/geom.rs` – window geometry in logical pixels (cached DPI scale, skips no-op moves)
- `src/tiles.rs` – the file list (a native list view in Tiles view, custom drawn)
- `src/draw.rs` – anti-aliased GDI drawing helpers
- `src/icons.rs` – renders icon-font glyphs into native icons (buttons, badges, window)
- `src/droptarget.rs` – OLE drop target (drag-hover highlight), `src/taskbar.rs` – taskbar progress
- `src/optimizer.rs` – decoding, resizing and encoding
- `res/app.manifest` – embedded by `build.rs`: Common Controls v6 (visual styles) and DPI awareness

Dark mode for Win32 controls uses the same undocumented `uxtheme.dll` functions that Windows' own
apps use; a future Windows update could change them.

Debug builds can render their own window to a PNG and exit, which is handy for checking the layout
(snapshot runs never save settings):

```bash
GIO_SNAPSHOT=window.png cargo run -- "D:\Photos"
GIO_SNAPSHOT=window.png GIO_SNAPSHOT_RUN=1 cargo run -- "D:\Photos"   # optimize first, then capture
GIO_SNAPSHOT=window.png GIO_SNAPSHOT_RUN=1 GIO_SNAPSHOT_MIDRUN=300 cargo run -- "D:\Photos"   # capture while optimizing
GIO_SNAPSHOT=window.png GIO_SNAPSHOT_RUN=1 GIO_SNAPSHOT_UNCHECK=0 cargo run -- "D:\Photos"   # untick the first image first
GIO_THEME=dark cargo run   # force dark (or light) instead of following Windows
GIO_SNAPSHOT_RAW=1 GIO_SNAPSHOT=window.png cargo run   # capture exactly what is on screen (no forced repaint)
GIO_SNAPSHOT=window.png GIO_SNAPSHOT_ALL_ON=1 cargo run   # switch every format and resizing on first
GIO_SNAPSHOT=window.png GIO_SNAPSHOT_ALL_ON=1 GIO_SNAPSHOT_SCROLL=400 cargo run   # ...and scroll the settings pane
cargo build --release --features snapshot   # the same self-snapshots in an optimized build
GIO_SNAPSHOT=x GIO_BENCH_RESIZE=1 ./target/release/gravingine_image_optimizer.exe   # time resize frames (snapshot build)
```
