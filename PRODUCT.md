# Product

<!-- impeccable:product-schema 1 -->

## Platform

windows

<!-- Native Windows desktop app (Win32 common controls). None of web / ios / android / adaptive applies. -->

## Users

Web developers in general: front-end developers and site builders who need batches of web-ready images (for a site, app, repo or CMS) and want them smaller in modern formats without uploading anything or learning codec settings.

## Product Purpose

Gravingine Image Optimizer turns folders of source images into smaller, web-ready files on the user's own Windows machine. Success: drop a folder, press Optimize, and get correctly named, much smaller WebP / AVIF / JPEG / PNG files with the folder structure intact, quickly and without surprises.

## Positioning

- **Offline and private:** images never leave the machine; no uploads, accounts, quotas or file-size limits (unlike web tools such as Squoosh or TinyPNG).
- **Batch folders, fast:** whole folder trees in one drop, structure preserved, encoded in parallel on every CPU core.
- **Native and lightweight:** a small native Windows app built from the real Windows controls; starts instantly and behaves like part of Windows.
- **Simple defaults:** sensible presets so a developer can drop and go; codec details are available but never required.

## Operating Context

- Used at a developer's desk on Windows 10/11, typically mid-project: images arrive from designers, clients, cameras or stock sites and must be prepared before committing them or uploading to a CMS.
- Input arrives by drag-and-drop from Explorer (files or folders), the Add files / Add folder dialogs, or by dropping onto the .exe.
- Output goes to an `optimized` folder next to the sources (mirroring subfolders) or to a chosen folder; the user then opens it in Explorer and copies the files into their project.

## Capabilities and Constraints

- **Inputs:** jpg, jpeg, jfif, png, webp, gif (first frame), bmp, tif/tiff, ico. AVIF input is not supported.
- **Outputs (any combination per run):** WebP (lossy or lossless), AVIF, JPEG (mozjpeg, progressive) and PNG (lossless, oxipng), each with its own quality/effort setting.
- **Resize:** fit within a max width/height, or scale to 75%, 50%, 25% or a custom percentage; keeps aspect ratio and never enlarges.
- **Safety:** EXIF/GPS metadata is stripped and camera orientation applied. Outputs larger than the original are skipped by default. Optional overwrite.
- **Feedback:** per-file results with size and % change per format, totals, errors per file, cancel, and Open output folder. Settings persist in `%APPDATA%\Gravingine Image Optimizer\settings.json`.
- **Technical:** Rust; native Win32 UI via native-windows-gui with an embedded manifest (visual styles, system-DPI aware); system dialog font (Segoe UI). Windows-only.
- **Known limits:** no dark mode (classic Win32 controls), no per-monitor DPI scaling, no in-app image preview or before/after comparison.
- **Distribution:** free public release. License, installer vs. portable .exe, code signing and update mechanism are **undecided**. Keep dependencies compatible with a free public release (GPL-licensed libimagequant was deliberately avoided).

## Brand Commitments

- Product name: **Gravingine Image Optimizer** (window title). No logo, icon or voice guidelines exist yet.

## Evidence on Hand

- Development test runs on Windows sample wallpapers (e.g. a 1.5 MB JPEG → 65 KB WebP at 45% scale; a 9.1 MB PNG → 160 KB WebP). These are internal checks, not published benchmarks.
- No users, testimonials, reviews, download counts, press or published benchmarks exist. Do not fabricate any.

## Product Principles

1. **Local by default:** nothing leaves the machine; never add network calls, telemetry or accounts without an explicit decision.
2. **Drop and go:** the default path (drop, Optimize, open folder) must work with zero configuration; options stay secondary.
3. **Never make things worse:** don't produce larger files, don't enlarge images, never touch the originals.
4. **Feel like Windows:** follow native Windows conventions and controls over custom chrome.
5. **Honest results:** always show exactly what was saved, skipped or failed, per file and per format.
