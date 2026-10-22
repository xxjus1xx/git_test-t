//! X-ray / DICOM imaging viewer — egui GUI over the `xray_viewer` engine
//! (synthetic belt sim) plus real DICOM (.dcm) file viewing.
//!
//! Two modes:
//!   * Synthetic — the animated conveyor-belt demo (no files needed).
//!   * DICOM     — open a `.dcm` file (or navigate to one) and display with
//!                 live window/level and an invert toggle.
//!
//! Ways to get a file into the viewer (all of these work on Wayland AND
//! Windows):
//!   * **File browser** (left panel) — navigate to the folder, click a `.dcm`.
//!     This is the reliable path on Wayland, where OS drag-and-drop is not
//!     supported by the egui/winit stack.
//!   * **Type a file path** into the "file" field and hit *load*.
//!   * **Type a folder path** into the "folder" field and hit *scan*.
//!   * **Launch with a path argument**: `xray-viewer /path/to/file.dcm`
//!   * **Drag-and-drop** a `.dcm` (or folder) — works on Windows and X11;
//!     ignored on Wayland (no DnD emitter there).

#![allow(clippy::cast_possible_truncation)]
// On Windows, build as a GUI app (no black console window behind the egui window).
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::path::Path;
use std::time::Instant;

use eframe::egui;
use egui::Color32;
use xray_viewer::dicom::{self, DcmFrame};
use xray_viewer::engine::{Config, Frame, Source, iron_lut};

const IMG_W: usize = 512;
const IMG_H: usize = 256;
const SYN_TEX: &str = "synthetic";
const DCM_TEX: &str = "dicom";

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Synthetic,
    Dicom,
}

/// One row in the file browser.
#[derive(Debug, Clone)]
struct FbEntry {
    name: String,
    is_dir: bool,
    path: String,
}

/// A minimal, dependency-free file browser (pure `std::fs`). Works on
/// Wayland (where OS DnD is unavailable) and on Windows alike.
struct FileBrowser {
    current_dir: String,
    entries: Vec<FbEntry>,
    error: String,
}

impl FileBrowser {
    fn new() -> Self {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".into());
        let mut fb = Self {
            current_dir: home,
            entries: Vec::new(),
            error: String::new(),
        };
        fb.refresh();
        fb
    }

    fn refresh(&mut self) {
        self.entries.clear();
        let dir = Path::new(&self.current_dir);
        match std::fs::read_dir(dir) {
            Ok(rd) => {
                let mut v: Vec<FbEntry> = rd
                    .flatten()
                    .filter_map(|e| {
                        let ft = e.file_type().ok()?;
                        let name = e.file_name().to_string_lossy().to_string();
                        if name.starts_with('.') {
                            return None; // skip hidden files/dirs
                        }
                        Some(FbEntry {
                            name,
                            is_dir: ft.is_dir(),
                            path: e.path().to_string_lossy().to_string(),
                        })
                    })
                    .collect();
                // Directories first, then case-insensitive name order.
                v.sort_by(|a, b| {
                    b.is_dir
                        .cmp(&a.is_dir)
                        .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                });
                self.entries = v;
                self.error.clear();
            }
            Err(e) => {
                self.error = e.to_string();
            }
        }
    }

    fn go_up(&mut self) {
        if let Some(parent) = Path::new(&self.current_dir).parent() {
            if !parent.as_os_str().is_empty() {
                self.current_dir = parent.to_string_lossy().to_string();
                self.refresh();
            }
        }
    }

    fn enter(&mut self, path: &str) {
        self.current_dir = path.to_string();
        self.refresh();
    }

    /// Navigate to the folder containing `file` (so a dropped/arg file's
    /// neighbours show up in the browser).
    fn go_to_parent_of(&mut self, file: &str) {
        if let Some(parent) = Path::new(file).parent() {
            if !parent.as_os_str().is_empty() {
                self.enter(parent.to_string_lossy().as_ref());
            }
        }
    }
}

/// Pure routing decision for a dropped/typed path, split out so it can be
/// unit-tested without a GUI.
#[derive(Debug, PartialEq, Eq)]
enum Route {
    File,
    Folder,
    Ignore,
}

fn route_path(p: &Path) -> Route {
    if p.is_dir() {
        Route::Folder
    } else if p.is_file() {
        Route::File
    } else {
        Route::Ignore
    }
}

/// DICOM viewing state.
struct DicomState {
    entries: Vec<xray_viewer::dicom::DcmEntry>,
    folder: String,
    scan_error: String,
    current: Option<DcmFrame>,
    current_name: String,
    load_error: String,
    win_center: f64,
    win_width: f64,
    invert: bool,
    dirty: bool,
    /// Normalized 256-bin histogram of the *displayed* buffer (peaks at 1.0).
    /// Updated whenever the image is re-rendered.
    hist: Vec<f32>,
}

impl DicomState {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            folder: String::new(),
            scan_error: String::new(),
            current: None,
            current_name: String::new(),
            load_error: String::new(),
            win_center: 1000.0,
            win_width: 2000.0,
            invert: false,
            dirty: false,
            hist: vec![0.0f32; 256],
        }
    }

    fn scan(&mut self, dir: &str) {
        let (entries, err) = dicom::scan_folder(dir);
        self.entries = entries;
        self.folder = dir.to_string();
        self.scan_error = err;
        self.current = None;
        self.current_name.clear();
        self.load_error.clear();
        self.dirty = false;
    }

    fn load(&mut self, path: &str) {
        let label = path.rsplit(['/', '\\']).next().unwrap_or(path);
        match dicom::open_dcm(path) {
            Ok(f) => {
                let (mn, mx) = Self::signed_range(&f.samples);
                self.win_center = (mn + mx) / 2.0;
                self.win_width = (mx - mn).max(1.0);
                self.invert = f.photometric_invert;
                self.current = Some(f);
                self.current_name = label.to_string();
                self.load_error.clear();
                self.dirty = true;
            }
            Err(e) => {
                self.load_error = format!("{label}: {e}");
            }
        }
    }

    /// Route a dropped (or launch-arg) path: directory → scan, file → load.
    /// Returns a one-line status message for the UI.
    fn handle_dropped_path(&mut self, p: &str) -> String {
        let path = Path::new(p);
        match route_path(path) {
            Route::Ignore => format!("ignored (not a file or folder): {p}"),
            Route::File => {
                self.load(p);
                if self.load_error.is_empty() {
                    format!("loaded {p}")
                } else {
                    format!("load failed: {}", self.load_error.clone())
                }
            }
            Route::Folder => {
                self.scan(p);
                if self.entries.is_empty() {
                    format!(
                        "no .dcm files in {p}{}",
                        if self.scan_error.is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", self.scan_error.clone())
                        }
                    )
                } else {
                    format!("{} file(s) in {p}", self.entries.len())
                }
            }
        }
    }

    fn auto_window(&mut self) {
        if let Some(f) = &self.current {
            let (mn, mx) = Self::signed_range(&f.samples);
            self.win_center = (mn + mx) / 2.0;
            self.win_width = (mx - mn).max(1.0);
            self.dirty = true;
        }
    }

    fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    fn signed_range(samples: &[f32]) -> (f64, f64) {
        let mut mn = f64::INFINITY;
        let mut mx = f64::NEG_INFINITY;
        for &s in samples {
            let v = s as f64;
            if v < mn {
                mn = v;
            }
            if v > mx {
                mx = v;
            }
        }
        (mn, mx)
    }

    fn map_sample(&self, v: f64) -> u8 {
        let c = self.win_center;
        let w = self.win_width.max(1.0);
        let lower = c - w / 2.0;
        let upper = c + w / 2.0;
        if upper <= lower {
            return 0;
        }
        let t = (v - lower) / (upper - lower);
        let g = if t <= 0.0 {
            0.0
        } else if t >= 1.0 {
            255.0
        } else {
            t * 255.0
        };
        let g = if self.invert { 255.0 - g } else { g };
        g.clamp(0.0, 255.0) as u8
    }

    /// Build the *displayed* grayscale buffer: window/level + invert applied,
    /// then the post-processing filter chain (gamma → contrast → sharpen).
    /// Shared by the image render and the histogram so both describe the same
    /// pixels. Identity when all parameters are at their neutral defaults.
    fn filtered_buffer(&self, f: &DcmFrame, gamma: f64, contrast_k: f64, sharpen: f64) -> Vec<u8> {
        let n = (f.rows as usize) * (f.cols as usize);
        let spp = (f.spp as usize).max(1);

        // 1) Window/level + invert → grayscale buffer.
        let mut gray = vec![0u8; n];
        if spp == 3 {
            for i in 0..n {
                let r = f.samples.get(i * 3).copied().unwrap_or(0.0) as f64;
                let g = f.samples.get(i * 3 + 1).copied().unwrap_or(0.0) as f64;
                let b = f.samples.get(i * 3 + 2).copied().unwrap_or(0.0) as f64;
                let lum = 0.299 * r + 0.587 * g + 0.114 * b; // BT.601 luma
                gray[i] = self.map_sample(lum);
            }
        } else {
            for i in 0..n {
                let v = f.samples.get(i).copied().unwrap_or(0.0);
                gray[i] = self.map_sample(v as f64);
            }
        }

        // 2) Filter chain (identity at neutral defaults).
        xray_viewer::filters::apply_chain(&gray, f.rows, f.cols, gamma, contrast_k, sharpen)
    }
}

struct XrayApp {
    mode: Mode,
    // Synthetic
    source: Source,
    syn_tex: egui::TextureHandle,
    grayscale: bool,
    belt: f32,
    defect_rate: f32,
    fill: f32,
    seed: u64,
    // Shared
    last_step: Instant,
    acc: f32,
    // Post-processing filters (DICOM view). Neutral defaults = identity.
    gamma: f64,
    contrast_k: f64,
    sharpen: f64,
    // DICOM
    dcm: DicomState,
    dcm_tex: egui::TextureHandle,
    file_path: String,
    fb: FileBrowser,
    status: String,
}

impl XrayApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let cfg = Config {
            width: IMG_W as u16,
            height: IMG_H as u16,
            seed: 0xC0FFEE,
            belt_speed: 3.0,
            container_width: 64.0,
            fill_fraction: 0.75,
            defect_rate: 0.25,
        };
        let source = Source::new(cfg);

        let blank = egui::ColorImage::new([IMG_W, IMG_H], Color32::from_rgb(235, 235, 235));
        let syn_tex = cc.egui_ctx.load_texture(SYN_TEX, blank, egui::TextureOptions::LINEAR);
        let black = egui::ColorImage::new([2, 2], Color32::BLACK);
        let dcm_tex = cc.egui_ctx.load_texture(DCM_TEX, black, egui::TextureOptions::LINEAR);

        let mut app = Self {
            mode: Mode::Dicom,
            source,
            syn_tex,
            grayscale: false,
            belt: 3.0,
            defect_rate: 0.25,
            fill: 0.75,
            seed: 0xC0FFEE,
            last_step: Instant::now(),
            acc: 0.0,
            gamma: 1.0,
            contrast_k: 1.0,
            sharpen: 0.0,
            dcm: DicomState::new(),
            dcm_tex,
            file_path: String::new(),
            fb: FileBrowser::new(),
            status: String::new(),
        };

        // If launched with a file/folder path argument, use it immediately.
        for arg in std::env::args().skip(1) {
            if Path::new(&arg).exists() {
                app.status = app.dcm.handle_dropped_path(&arg);
                app.fb.go_to_parent_of(&arg);
                break;
            }
        }

        app
    }

    /// Consume any files dropped onto the window this frame (OS-level DnD,
    /// surfaced by egui via `raw.dropped_files`). On Wayland this list stays
    /// empty (no DnD emitter), which is harmless — the file browser covers it.
    fn handle_dropped(&mut self, ctx: &egui::Context) {
        let dropped: Vec<std::path::PathBuf> = ctx
            .input(|i| {
                i.raw
                    .dropped_files
                    .iter()
                    .filter_map(|f| f.path.clone())
                    .collect()
            });
        for p in dropped {
            let ps = p.to_string_lossy().to_string();
            self.status = self.dcm.handle_dropped_path(&ps);
            self.fb.go_to_parent_of(&ps);
        }
    }

    fn frame_to_image(&self, f: &Frame) -> egui::ColorImage {
        let px = f.pixels();
        let mut out = Vec::with_capacity(IMG_W * IMG_H);
        for &v in px {
            let c = if self.grayscale {
                Color32::from_rgb(v, v, v)
            } else {
                let (r, g, b) = iron_lut(v);
                Color32::from_rgb(r, g, b)
            };
            out.push(c);
        }
        egui::ColorImage {
            size: [IMG_W, IMG_H],
            pixels: out,
        }
    }
}

impl eframe::App for XrayApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_dropped(ctx);

        let now = Instant::now();
        let dt = now.duration_since(self.last_step).as_secs_f32();
        self.last_step = now;

        if self.mode == Mode::Synthetic {
            self.acc += dt;
            let tick = 1.0 / 60.0;
            let steps = (self.acc / tick).floor() as u32;
            self.acc -= steps as f32 * tick;
            let steps = steps.min(300);
            if steps > 0 {
                let mut latest = None;
                for _ in 0..steps {
                    latest = Some(self.source.next_frame());
                }
                if let Some(f) = latest {
                    self.syn_tex.set(self.frame_to_image(&f), egui::TextureOptions::LINEAR);
                }
            }
        }

        self.top_panel(ctx);

        if self.mode == Mode::Dicom {
            self.dicom_body(ctx);
        } else {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add(
                        egui::Image::new((self.syn_tex.id(), self.syn_tex.size_vec2()))
                            .max_size(egui::vec2(IMG_W as f32, IMG_H as f32)),
                    );
                    ui.small(format!("seed {:X} · {} on belt", self.seed, self.source.on_screen()));
                });
            });
        }
    }
}

impl XrayApp {
    fn top_panel(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.strong("Imaging viewer");
                ui.separator();
                if ui.selectable_label(self.mode == Mode::Synthetic, "Synthetic").clicked() {
                    self.mode = Mode::Synthetic;
                }
                if ui.selectable_label(self.mode == Mode::Dicom, "DICOM").clicked() {
                    self.mode = Mode::Dicom;
                }
                if self.mode == Mode::Synthetic {
                    ui.separator();
                    ui.label(format!("count {}", self.source.count));
                    ui.label(format!("defects {}", self.source.defect_count));
                    ui.checkbox(&mut self.grayscale, "grayscale");
                    if ui.button("reset").clicked() {
                        self.source.reset();
                    }
                    if ui.button("reseed").clicked() {
                        let s = self.seed.wrapping_add(1);
                        self.seed = s;
                        self.source = Source::new(Config {
                            seed: s,
                            width: IMG_W as u16,
                            height: IMG_H as u16,
                            belt_speed: self.belt,
                            container_width: 64.0,
                            fill_fraction: self.fill,
                            defect_rate: self.defect_rate,
                        });
                    }
                } else {
                    ui.separator();
                    if !self.dcm.current_name.is_empty() {
                        ui.strong(&self.dcm.current_name);
                    }
                }
            });

            if self.mode == Mode::Synthetic {
                ui.horizontal_wrapped(|ui| {
                    ui.label("belt speed");
                    let mut speed = self.belt;
                    let spd = format!("{speed:.2} px/f");
                    ui.add(egui::Slider::new(&mut speed, 0.5..=12.0).text(spd));
                    self.belt = speed;
                    self.source.set_belt_speed(speed);
                    ui.separator();
                    ui.label("defect rate");
                    let mut dr = self.defect_rate;
                    let drt = format!("{dr:.2}");
                    ui.add(egui::Slider::new(&mut dr, 0.0..=1.0).text(drt));
                    self.defect_rate = dr;
                    self.source.set_defect_rate(dr);
                    ui.separator();
                    ui.label("fill");
                    let mut fill = self.fill;
                    let flt = format!("{fill:.2}");
                    ui.add(egui::Slider::new(&mut fill, 0.10..=1.0).text(flt));
                    self.fill = fill;
                    self.source.set_fill_fraction(fill);
                });
            } else {
                // Row 1: manual file path + folder scan (both work on Wayland).
                ui.horizontal_wrapped(|ui| {
                    ui.strong("Open");
                    ui.label("file");
                    let mut fp = self.file_path.clone();
                    ui.add(egui::TextEdit::singleline(&mut fp).desired_width(320.0));
                    self.file_path = fp.clone();
                    if ui.button("load").clicked() {
                        if !fp.trim().is_empty() {
                            self.status = self.dcm.handle_dropped_path(fp.trim());
                            self.fb.go_to_parent_of(fp.trim());
                        }
                    }
                    ui.separator();
                    ui.label("folder");
                    let mut dir = self.dcm.folder.clone();
                    ui.add(egui::TextEdit::singleline(&mut dir).desired_width(240.0));
                    if ui.button("scan").clicked() {
                        if !dir.trim().is_empty() {
                            self.dcm.scan(dir.trim());
                            self.fb.enter(dir.trim());
                        }
                    }
                });

                // Row 2: window/level controls.
                ui.horizontal_wrapped(|ui| {
                    ui.label("center");
                    let mut c = self.dcm.win_center;
                    ui.add(egui::DragValue::new(&mut c).speed(10.0));
                    self.dcm.win_center = c;
                    self.dcm.mark_dirty();
                    ui.separator();
                    ui.label("width");
                    let mut w = self.dcm.win_width;
                    ui.add(egui::DragValue::new(&mut w).speed(10.0));
                    self.dcm.win_width = w.max(1.0);
                    self.dcm.mark_dirty();
                    ui.separator();
                    let mut inv = self.dcm.invert;
                    ui.checkbox(&mut inv, "invert");
                    if inv != self.dcm.invert {
                        self.dcm.invert = inv;
                        self.dcm.mark_dirty();
                    }
                    if ui.button("auto window").clicked() {
                        self.dcm.auto_window();
                    }
                });

                // Row 3: post-processing filters (gamma / contrast / sharpen).
                ui.horizontal_wrapped(|ui| {
                    ui.strong("Filters");
                    ui.separator();
                    ui.label("gamma");
                    let mut g = self.gamma;
                    ui.add(egui::DragValue::new(&mut g).speed(0.05).range(0.1..=4.0));
                    if g != self.gamma {
                        self.gamma = g;
                        self.dcm.mark_dirty();
                    }
                    ui.separator();
                    ui.label("contrast");
                    let mut k = self.contrast_k;
                    ui.add(egui::DragValue::new(&mut k).speed(0.05).range(0.2..=4.0));
                    if k != self.contrast_k {
                        self.contrast_k = k;
                        self.dcm.mark_dirty();
                    }
                    ui.separator();
                    ui.label("sharpen");
                    let mut s = self.sharpen;
                    ui.add(egui::DragValue::new(&mut s).speed(0.02).range(0.0..=2.0));
                    if s != self.sharpen {
                        self.sharpen = s;
                        self.dcm.mark_dirty();
                    }
                    if ui.button("reset filters").clicked() {
                        self.gamma = 1.0;
                        self.contrast_k = 1.0;
                        self.sharpen = 0.0;
                        self.dcm.mark_dirty();
                    }
                });

                // Status + errors.
                ui.horizontal_wrapped(|ui| {
                    if !self.status.is_empty() {
                        ui.colored_label(egui::Color32::from_rgb(140, 200, 255), &self.status);
                        ui.separator();
                    }
                    if !self.dcm.scan_error.is_empty() {
                        ui.colored_label(
                            egui::Color32::from_rgb(255, 180, 90),
                            &self.dcm.scan_error,
                        );
                        ui.separator();
                    }
                    if !self.dcm.load_error.is_empty() {
                        ui.colored_label(
                            egui::Color32::from_rgb(255, 100, 100),
                            &self.dcm.load_error,
                        );
                    }
                });
            }
        });
    }

    /// Render the 256-bin histogram of the currently displayed image as a
    /// bar chart, plus a min/peak/max readout. Reads the live `dcm.hist`.
    fn histogram_panel(&self, ui: &mut egui::Ui) {
        let h = &self.dcm.hist;
        let w = ui.available_width().max(40.0);
        let ht = 120.0f32;
        let bins = 256usize;
        let bar = w / bins as f32;

        // Stats: first/last non-empty bin + peak bin index.
        let first = h.iter().position(|&v| v > 0.0).unwrap_or(0);
        let last = h.iter().rposition(|&v| v > 0.0).unwrap_or(0);
        let by_val = |a: &(usize, f32), b: &(usize, f32)| {
            a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)
        };
        let peak_bin = h.iter().cloned().enumerate().max_by(by_val).map(|(i, _)| i).unwrap_or(0);

        ui.horizontal(|ui| {
            ui.strong("Histogram");
            ui.small(format!("min {first} · peak {peak_bin} · max {last}"));
        });

        let (rect, _resp) = ui.allocate_exact_size(egui::vec2(w, ht), egui::Sense::hover());
        let painter = ui.painter_at(rect);
        // Background.
        painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(18, 20, 24));
        // Bars: one per bin, height ∝ normalized count.
        for i in 0..bins {
            let v = h[i];
            if v <= 0.0 {
                continue;
            }
            let hgt = (v * (ht - 2.0)).max(0.0);
            let x0 = i as f32 * bar;
            let x1 = (i + 1) as f32 * bar;
            let y1 = ht;
            let y0 = ht - hgt;
            painter.rect_filled(
                egui::Rect::from_min_max(egui::pos2(x0, y0), egui::pos2(x1, y1)),
                0.0,
                egui::Color32::from_rgb(120, 190, 255),
            );
        }
        // Baseline (thin filled rect — avoids the line_segment stroke API).
        painter.rect_filled(
            egui::Rect::from_min_max(egui::pos2(0.0, ht - 1.0), egui::pos2(w, ht)),
            0.0,
            egui::Color32::from_rgb(70, 75, 82),
        );
        // Axis end labels.
        painter.text(
            egui::pos2(2.0, ht - 2.0),
            egui::Align2::LEFT_BOTTOM,
            "0",
            egui::FontId::monospace(10.0),
            egui::Color32::GRAY,
        );
        painter.text(
            egui::pos2(w - 2.0, ht - 2.0),
            egui::Align2::RIGHT_BOTTOM,
            "255",
            egui::FontId::monospace(10.0),
            egui::Color32::GRAY,
        );
        ui.add_space(4.0);
    }

    /// DICOM: file browser (left) + image (center).
    fn dicom_body(&mut self, ctx: &egui::Context) {
        // Re-upload the image texture only when the file / window / invert
        // actually changed (dirty flag), not every frame.
        if self.dcm.dirty {
            if let Some(f) = &self.dcm.current {
                // One buffer pass feeds both the image and the histogram, so
                // they always describe the same displayed pixels.
                let buf = self.dcm.filtered_buffer(f, self.gamma, self.contrast_k, self.sharpen);
                let out: Vec<Color32> = buf.iter().map(|&v| Color32::from_rgb(v, v, v)).collect();
                let img = egui::ColorImage {
                    size: [f.cols as usize, f.rows as usize],
                    pixels: out,
                };
                self.dcm.hist = xray_viewer::filters::histogram_normalized(&buf);
                self.dcm_tex.set(img, egui::TextureOptions::LINEAR);
                self.dcm.dirty = false;
            }
        }

        egui::SidePanel::left("browser").exact_width(280.0).show(ctx, |ui| {
            ui.heading("Files");
            ui.horizontal(|ui| {
                if ui.button("↑ up").clicked() {
                    self.fb.go_up();
                }
                if ui.button("home").clicked() {
                    let home = std::env::var("HOME")
                        .or_else(|_| std::env::var("USERPROFILE"))
                        .unwrap_or_else(|_| ".".into());
                    self.fb.enter(&home);
                }
            });
            // Current dir (small text).
            ui.strong(&self.fb.current_dir);
            ui.small("(file browser — navigate to a folder, click a .dcm)");
            if !self.fb.error.is_empty() {
                ui.colored_label(
                    egui::Color32::from_rgb(255, 100, 100),
                    format!("can't read: {}", self.fb.error),
                );
            }
            ui.separator();
            // Snapshot entries so we can call fb/dcm (mutable) while iterating.
            let entries = self.fb.entries.clone();
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                if entries.is_empty() {
                    ui.small("(empty folder)");
                }
                for e in entries {
                    if e.is_dir {
                        if ui
                            .selectable_label(false, format!("📁 {}", e.name))
                            .clicked()
                        {
                            self.fb.enter(&e.path);
                        }
                    } else {
                        let is_dcm = e.name.to_lowercase().ends_with(".dcm");
                        let is_sel = self.dcm.current_name == e.name;
                        let label = if is_dcm {
                            format!("📄 {}", e.name)
                        } else {
                            format!("· {}", e.name)
                        };
                        if ui.selectable_label(is_sel, label).clicked() {
                            self.dcm.load(&e.path);
                        }
                    }
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            match &self.dcm.current {
                Some(f) => {
                    ui.horizontal(|ui| {
                        ui.small(format!(
                            "{}×{}  spp={}  {}",
                            f.cols,
                            f.rows,
                            f.spp,
                            f.transfer_syntax
                        ));
                        if f.photometric_invert {
                            ui.small("MONOCHROME1");
                        } else {
                            ui.small("MONOCHROME2/RGB");
                        }
                    });
                    ui.separator();
                    ui.vertical_centered(|ui| {
                        ui.add(egui::Image::new((self.dcm_tex.id(), self.dcm_tex.size_vec2())));
                        // Live histogram of the displayed image (tracks window/level + filters).
                        if self.dcm.hist.iter().any(|&v| v > 0.0) {
                            ui.add_space(8.0);
                            self.histogram_panel(ui);
                        }
                    });
                }
                None => {
                    ui.vertical_centered(|ui| {
                        ui.add_space(32.0);
                        ui.strong("No file selected.");
                        ui.small("Use the file browser on the left to find a .dcm,");
                        ui.small("or type a file/folder path above and hit load / scan.");
                        ui.small("On Windows you can also drag a .dcm onto this window.");
                    });
                }
            }
        });
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1040.0, 620.0])
            .with_title("Imaging viewer — DICOM / synthetic"),
        ..Default::default()
    };
    eframe::run_native(
        "Imaging viewer",
        options,
        Box::new(|cc| Ok(Box::new(XrayApp::new(cc)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_existing_file_is_file() {
        let tmp = std::env::temp_dir().join("xray_route_test_file");
        std::fs::write(&tmp, b"hi").unwrap();
        let r = route_path(&tmp);
        let _ = std::fs::remove_file(&tmp);
        assert_eq!(r, Route::File);
    }

    #[test]
    fn route_existing_dir_is_folder() {
        let tmp = std::env::temp_dir().join("xray_route_test_dir");
        std::fs::create_dir_all(&tmp).unwrap();
        let r = route_path(&tmp);
        let _ = std::fs::remove_dir_all(&tmp);
        assert_eq!(r, Route::Folder);
    }

    #[test]
    fn route_missing_is_ignore() {
        let r = route_path(Path::new("/definitely/not/a/real/path/xray_test"));
        assert_eq!(r, Route::Ignore);
    }

    /// FileBrowser lists real files/dirs, dirs-first, skips hidden.
    #[test]
    fn file_browser_lists_and_sorts() {
        let base = std::env::temp_dir().join("xray_fb_test");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("zeta")).unwrap();
        std::fs::create_dir_all(base.join("alpha")).unwrap();
        std::fs::write(base.join("b.dcm"), b"x").unwrap();
        std::fs::write(base.join("a.dcm"), b"x").unwrap();
        std::fs::write(base.join(".hidden"), b"x").unwrap();

        let mut fb = FileBrowser {
            current_dir: base.to_string_lossy().to_string(),
            entries: Vec::new(),
            error: String::new(),
        };
        fb.refresh();

        // Hidden file excluded.
        assert!(!fb.entries.iter().any(|e| e.name == ".hidden"));
        // Two dirs + two files.
        assert_eq!(fb.entries.iter().filter(|e| e.is_dir).count(), 2);
        assert_eq!(fb.entries.iter().filter(|e| !e.is_dir).count(), 2);
        // Dirs come first.
        assert!(fb.entries[0].is_dir && fb.entries[1].is_dir);
        // Dirs sorted case-insensitively: alpha before zeta.
        assert_eq!(fb.entries[0].name, "alpha");
        assert_eq!(fb.entries[1].name, "zeta");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// FileBrowser.go_up moves to the parent.
    #[test]
    fn file_browser_go_up() {
        let base = std::env::temp_dir().join("xray_fb_up_parent");
        let child = base.join("child");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&child).unwrap();
        let mut fb = FileBrowser {
            current_dir: child.to_string_lossy().to_string(),
            entries: Vec::new(),
            error: String::new(),
        };
        let base_str = base.to_string_lossy().to_string();
        fb.go_up();
        assert_eq!(fb.current_dir, base_str, "go_up should move to the parent");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// DicomState::handle_dropped_path on a real .dcm file loads it
    /// (exercises the same path a file-browser click / drop takes).
    #[test]
    fn handle_dropped_real_dcm_file_loads() {
        let dir = std::path::Path::new("/home/russ/dcm_test");
        if !dir.is_dir() {
            eprintln!("(skipped: no /home/russ/dcm_test)");
            return;
        }
        let file = dir
            .read_dir()
            .unwrap()
            .flatten()
            .find(|e| {
                e.path()
                    .extension()
                    .and_then(|x| x.to_str())
                    .map(|x| x.eq_ignore_ascii_case("dcm"))
                    .unwrap_or(false)
            })
            .map(|e| e.path())
            .expect("no .dcm in dcm_test");
        let mut st = DicomState::new();
        let msg = st.handle_dropped_path(&file.to_string_lossy());
        assert!(
            st.current.is_some(),
            "expected current=Some, got None. msg: {msg}"
        );
    }
}
