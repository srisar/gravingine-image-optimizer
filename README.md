# Gravingine Image Optimizer

A Windows image optimizer written in Rust, with a [Slint](https://slint.dev) UI in Slint's
Cosmic style. Drag & drop images or whole folders, pick the web formats you want, and get
smaller files.

## Features

- **Drag & drop** files or folders onto the window (or onto the `.exe` icon), or use *Add files* / *Add folder*.
- Folders are scanned recursively; the folder structure is preserved in the output.
- **Presets** (top of the settings): Web, Smallest, Photos, Thumbnails, Lossless and Email set the output formats, their quality and resizing in one click. Change anything and the preset shows *Custom*; *Save as preset…* keeps your own (stored in `%APPDATA%\Gravingine Image Optimizer\presets.json`), which can be deleted again. Presets leave *Save to* as it is.
- **Output formats** (any combination, all encoded in one pass):
  | Format | Encoder | Notes |
  |---|---|---|
  | JPEG | mozjpeg (progressive, optimized scans) | Universal; transparency flattened onto white |
  | WebP | libwebp (lossy or lossless) | Supports transparency |
  | AVIF | ravif / rav1e | Smallest files, slower to encode |
  | PNG  | oxipng (lossless) | Re-compresses PNGs in place when not resized |
- **Resize** with presets: Thumbnails (230 × 230), Small (640 × 480), Medium (800 × 600), Large (1024 × 768), HD (1920 × 1080) and 4K (3840 × 2160), which fit the image within the size turned to its orientation (a portrait photo fits 480 × 640 for Small); or reduce to 25%, 50% or 75%; or a custom maximum width/height or percentage. The aspect ratio is kept and images are never enlarged.
- Strips metadata (EXIF/GPS) and bakes in EXIF orientation.
- *Skip outputs larger than the original* so you never end up with a worse file.
- *Clear images from the list once optimized* (under *Save to*): after a run, images that optimized cleanly leave the list and their savings go into the status line; images with errors stay, so you can read why.
- File list as tiles: thumbnail with a status badge, file name and format, what will be produced (or the colored result) and where it was saved, with a progress bar while optimizing. On the right, *Original* and *Output* columns: dimensions and file size before, and the resized dimensions and smallest saved size after. Tick the images to process with their checkboxes; *Select all* / *Select none*.
- **Before / after preview**: adding a single image opens it straight away; otherwise clicking a tile (or its eye button) opens the original and an output side by side with a split line you drag across, both drawn at the same size so quality is what you compare. Before optimizing, it's a live preview: the current settings are encoded in memory (nothing is saved), and moving a slider or switching a format re-encodes it, so you can tune by eye and then click *Optimize*. Optimized images can switch between *Saved* (the files written) and *Preview*. Choose the format and the zoom (*Fit*, 25%, 50%, 75% or 100%; drag to pan when the picture is bigger than the view); Esc closes it. Every format can be previewed, AVIF included (decoded with [avif-decode](https://github.com/kornelski/avif-decode), pure Rust on the rav1d AV1 decoder).
- Parallel processing on all CPU cores, cancellable, with per-file and total savings, progress on the taskbar button, and a taskbar flash when a run finishes in the background.
- Settings are remembered between runs (`%APPDATA%\Gravingine Image Optimizer\settings.json`).
- *About* (top right): the Gravingine logo, version and build (commit and date), *Check for updates* (opens the GitHub releases), and Gravitide with a link to gravitide.dev. *Licenses* lists the open-source parts used, with the Slint attribution its royalty-free license asks for.
- Slint UI (Cosmic style):
  - follows Windows' **light/dark mode**, live, and tints its accent from the Windows **accent color**; the title bar matches the window;
  - a scrolling settings pane: each output format (and resizing) is its own card with a tinted icon, a short description and a switch; a format's sliders slide open inside its card while it is switched on;
  - the file list highlights while files are dragged over the window;
  - scales per monitor; icons are glyphs from the Windows icon font (Segoe Fluent Icons, or Segoe MDL2 Assets on Windows 10). The window and taskbar icon is the Gravingine logo, drawn at runtime.

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

## Releases

`scripts/build-release.sh` runs the tests, builds the optimized binary and packages it (Git Bash):

```bash
scripts/build-release.sh   # add --skip-tests to only build and package
```

It writes to `dist/`, each with a `.sha256` checksum next to it:

- `gravingine-image-optimizer-v<version>-windows-x64-setup.exe`: the installer. It installs for
  the current user without admin rights (into `%LOCALAPPDATA%\Programs`), adds a Start menu
  shortcut and an optional desktop shortcut, and can be removed from *Settings › Apps*.
  Upgrading closes a running copy first.
- `gravingine-image-optimizer-v<version>-windows-x64.zip`: the portable build (the exe, this
  README and the license).

The installer is built with [Inno Setup 6](https://jrsoftware.org/isinfo.php) from
`installer/gravingine.iss`; the script finds `ISCC.exe` on `PATH`, in `$ISCC` or in its default
install folders, and skips the installer if it isn't installed (`--require-installer` makes that
an error). To install it:

```bash
winget install JRSoftware.InnoSetup
```

The exe embeds the Gravingine logo as its icon and its version information (both generated by
`build.rs`; the icon is drawn by `src/logo.rs`, the same code that draws the window icon).

To publish a release on GitHub, bump `version` in `Cargo.toml`, merge that, then push a matching
tag from `main`:

```bash
git tag v0.1.0
git push origin v0.1.0
```

The *Release* workflow (`.github/workflows/release.yml`) then checks the tag against
`Cargo.toml`, runs the same script on a Windows runner (installing Inno Setup if needed), and
publishes a GitHub release with the installer, the zip, their checksums and notes generated from
the merged pull requests. Neither the installer nor the exe is code-signed, so Windows SmartScreen
may warn on first launch.

## Project layout

- `ui/app.slint` – the whole window: settings cards, file list tiles, toolbar and footer (compiled by `build.rs` with the Cosmic style)
- `src/app.rs` – app state and behavior: adding images, thumbnails, selection, runs, window events
- `src/optimizer.rs` – decoding, resizing and encoding
- `src/presets.rs` – settings presets: the built-in ones, matching settings to a preset, and the user's own in `presets.json`
- `src/taskbar.rs` – taskbar button progress and flash
- `src/logo.rs` – draws the Gravingine logo: the window icon, and (via `build.rs`) the exe's embedded `.ico`
- `src/winicon.rs` – sets the window icon, icon-font detection and title bar tint (Win32)
- `installer/gravingine.iss` – the Inno Setup installer script
- `res/app.manifest` – embedded by `build.rs`: Common Controls v6 (system dialogs) and per-monitor DPI awareness
- `res/gravingine-logo.svg`, `res/gravitide-icon.svg` – the app and Gravitide logos shown in the About dialog

The Win32 (native-windows-gui) version of the UI is kept on the `win32-ui` branch.

Debug builds can render their own window to a PNG and exit, which is handy for checking the layout
(snapshot runs never save settings):

```bash
GIO_SNAPSHOT=window.png cargo run -- "D:\Photos"
GIO_SNAPSHOT=window.png GIO_SNAPSHOT_RUN=1 cargo run -- "D:\Photos"   # optimize first, then capture
GIO_SNAPSHOT=window.png GIO_SNAPSHOT_RUN=1 GIO_SNAPSHOT_MIDRUN=300 cargo run -- "D:\Photos"   # capture while optimizing
GIO_SNAPSHOT=window.png GIO_SNAPSHOT_ALL_ON=1 cargo run   # switch every format and resizing on first
GIO_SNAPSHOT=window.png GIO_SNAPSHOT_RESIZE=2 cargo run -- "D:\Photos"   # resizing on, with the 3rd resize choice (Medium)
GIO_SNAPSHOT=window.png GIO_SNAPSHOT_RUN=1 GIO_SNAPSHOT_CLEAR=1 cargo run -- "D:\Photos"   # optimize with "Clear images ... once optimized" on
GIO_SNAPSHOT=window.png GIO_SNAPSHOT_PRESET=0 cargo run -- "D:\Photos"   # apply the first preset (Web)
GIO_SNAPSHOT=window.png GIO_SNAPSHOT_PRESET_DIALOG=1 cargo run   # the "Save as preset" dialog
GIO_SNAPSHOT=window.png GIO_SNAPSHOT_SAVE_PRESET="My blog" cargo run   # save the settings as a preset (not written to disk)
GIO_SNAPSHOT=window.png GIO_SNAPSHOT_RUN=1 GIO_SNAPSHOT_COMPARE=0 cargo run -- "D:\Photos"   # run, then open the comparison for the first image
GIO_SNAPSHOT=window.png GIO_SNAPSHOT_RUN=1 GIO_SNAPSHOT_COMPARE=0 GIO_SNAPSHOT_ZOOM=100 GIO_SNAPSHOT_SPLIT=0.3 cargo run -- "D:\Photos"   # ...at 100% (or 25, 50, 75), line at 30%
GIO_SNAPSHOT=window.png GIO_SNAPSHOT_ALL_ON=1 GIO_SNAPSHOT_COMPARE=0 GIO_SNAPSHOT_COMPARE_FORMAT=2 cargo run -- "D:\Photos"   # live preview of the AVIF (3rd format chip)
GIO_SNAPSHOT=window.png GIO_SNAPSHOT_COMPARE=0 cargo run -- "D:\Photos"   # live preview of the first image, nothing optimized
GIO_SNAPSHOT=window.png GIO_SNAPSHOT_COMPARE=0 GIO_SNAPSHOT_RETUNE=20 cargo run -- "D:\Photos"   # ...then set WebP/JPEG quality to 20 while it's open
GIO_THEME=dark cargo run   # force dark (or light) instead of following Windows
GIO_SNAPSHOT=window.png GIO_SNAPSHOT_ABOUT=1 cargo run   # the About dialog (GIO_SNAPSHOT_ABOUT=licenses: its Licenses view)
cargo build --release --features snapshot   # the same self-snapshots in an optimized build
```

## License

MIT © 2026 Gravitide Pvt Ltd. See [LICENSE](LICENSE). The app uses open-source components under their own licenses, listed in *About › Licenses*.
