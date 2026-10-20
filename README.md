# Imaging viewer — v4

X-ray / DICOM (`.dcm`) viewer with window/level + invert + **filters**
(gamma / contrast / sharpen), plus a synthetic conveyor-belt demo. Rust + egui,
single self-contained binary.

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

## Building

```bash
cargo build --release                       # Linux
cargo build --release --target x86_64-pc-windows-gnu   # Windows (mingw cross)
cargo test --release                        # 28 tests (22 lib + 6 bin)
```

## AppImage

```bash
cp target/release/xray-viewer AppDir/usr/bin/xray-viewer
appimagetool AppDir/ XrayViewer-x86_64.AppImage
```
