# Imaging viewer — v4

X-ray / DICOM (`.dcm`) viewer with window/level + invert + **filters**
(gamma / contrast / sharpen) + a live **histogram** of the displayed image,
plus a synthetic conveyor-belt demo. Rust + egui, single self-contained binary.

## Opening a file

Pick whichever works on your machine — **the file browser is the one that is
guaranteed to work on GNOME Wayland** (Linux) *and* Windows.

| Method | Wayland | Windows | Notes |
|---|---|---|---|
| **File browser** (left panel) | ✅ | ✅ | Navigate to the folder, click a `.dcm`. No deps. |
| **Type a file path** → *load* | ✅ | ✅ | Paste `/path/to/file.dcm`, hit *load*. |
| **Type a folder path** → *scan* | ✅ | ✅ | Lists every `.dcm` in the folder; click one. |
| **Launch with an argument** | ✅ | ✅ | `xray-viewer /path/to/file.dcm` (or a folder). |
| **Drag-and-drop** a file/folder | ❌ | ✅ | Works on Windows and X11. On Wayland the egui/winit stack emits no drop event, so this is inert there — use the file browser. |

## Window / level

- **center / width** — drag to adjust the grayscale window.
- **invert** — MONOCHROME1 / reversed display.
- **auto window** — re-fit to the file's min/max.

## Filters

Post-processing applied after window/level (pure, unit-tested in
`src/filters.rs`; neutral defaults = identity, so the image is untouched until
you move a control):

- **gamma** — brightness curve; < 1 brightens midtones, > 1 darkens.
- **contrast** — spreads values about mid-gray; > 1 increases contrast.
- **sharpen** — edge enhancement (unsharp mask); 0 = off, higher = crisper.
- **reset filters** — returns all three to identity.

## Histogram

A live 256-bin bar chart below the image, showing the pixel-value distribution
of the **displayed** image — i.e. after window/level + invert + filters, exactly
what you're looking at. It updates the instant you change the window, invert,
or any filter, so you can see how a change reshapes the tonal spread. The readout
line shows the first (min) non-empty bin, the peak bin, and the last (max) non-empty
bin. The histogram math is pure and unit-tested in `src/filters.rs`
(`histogram` / `histogram_normalized`).

## Building

```bash
cargo build --release                       # Linux
cargo build --release --target x86_64-pc-windows-gnu   # Windows (mingw cross)
cargo test --release                        # 32 tests (26 lib + 6 bin)
```

## AppImage

```bash
cp target/release/xray-viewer AppDir/usr/bin/xray-viewer
appimagetool AppDir/ XrayViewer-x86_64.AppImage
```
