//! X-ray / DICOM imaging viewer — egui GUI over the `xray_viewer` engine
//! (synthetic belt sim) plus real DICOM (.dcm) file viewing.
//!
//! Two modes:
//!   * Synthetic — the animated conveyor-belt demo (no files needed).
//!   * DICOM     — open a folder of `.dcm`, list them, display with live
//!                 window/level and an invert toggle.

#![allow(clippy::cast_possible_truncation)]
// On Windows, build as a GUI app (no black console window behind the egui window).
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

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

/// DICOM viewing state.
struct DicomState {
    entries: Vec<xray_viewer::dicom::DcmEntry>,
    folder: String,
    scan_error: String,
    current: Option<DcmFrame>,
    current_name: String,
    load_error: String,
    // window/level
    win_center: f64,
    win_width: f64,
    invert: bool,
    /// Re-upload the DICOM texture when this is true.
    dirty: bool,
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
        let label = path.rsplit('/').next().unwrap_or(path);
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

    /// Min/max of the (already rescaled, f32) samples.
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

    /// Map one sample (as f64) to 8-bit gray using window/level + invert.
    fn map_sample(&self, v: f64) -> u8 {
        let c = self.win_center;
        // Prevent division by zero - width must be at least 1.0
        let w = self.win_width.max(1.0);
        let lower = c - w / 2.0;
        let upper = c + w / 2.0;
        // Clamp to avoid NaN from division when upper == lower
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
        // Clamp to u8 range to prevent overflow/underflow
        g.clamp(0.0, 255.0) as u8
    }

    /// Render the current frame to an 8-bit gray ColorImage.
    /// For RGB (spp==3), we convert to grayscale using standard luma coefficients.
    fn to_image(&self, f: &DcmFrame) -> egui::ColorImage {
        let n = (f.rows as usize) * (f.cols as usize);
        let spp = (f.spp as usize).max(1);
        let mut out = Vec::with_capacity(n);
        
        if spp == 3 {
            // RGB mode - convert to grayscale using luma coefficients
            for i in 0..n {
                let r = f.samples.get(i * 3).copied().unwrap_or(0.0) as f64;
                let g = f.samples.get(i * 3 + 1).copied().unwrap_or(0.0) as f64;
                let b = f.samples.get(i * 3 + 2).copied().unwrap_or(0.0) as f64;
                // Standard ITU-R BT.601 luma coefficients
                let gray = 0.299 * r + 0.587 * g + 0.114 * b;
                let g_val = self.map_sample(gray);
                out.push(Color32::from_rgb(g_val, g_val, g_val));
            }
        } else {
            // Grayscale mode (spp == 1)
            for i in 0..n {
                let v = f.samples.get(i).copied().unwrap_or(0.0);
                let g = self.map_sample(v as f64);
                out.push(Color32::from_rgb(g, g, g));
            }
        }
        
        egui::ColorImage {
            size: [f.cols as usize, f.rows as usize],
            pixels: out,
        }
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
    fps_ema: f32,
    last_step: Instant,
    acc: f32,
    // DICOM
    dcm: DicomState,
    dcm_tex: egui::TextureHandle,
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

        Self {
            mode: Mode::Dicom,
            source,
            syn_tex,
            grayscale: false,
            belt: 3.0,
            defect_rate: 0.25,
            fill: 0.75,
            seed: 0xC0FFEE,
            fps_ema: 0.0,
            last_step: Instant::now(),
            acc: 0.0,
            dcm: DicomState::new(),
            dcm_tex,
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
        let now = Instant::now();
        let dt = now.duration_since(self.last_step).as_secs_f32();
        self.last_step = now;

        if dt > 0.0 {
            let inst = 1.0 / dt;
            self.fps_ema = if self.fps_ema == 0.0 {
                inst
            } else {
                0.9 * self.fps_ema + 0.1 * inst
            };
        }

        // Synthetic mode: advance the engine at wall-clock speed.
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
                ui.separator();
                ui.label(format!("FPS {:.0}", self.fps_ema));
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
                    ui.label(format!("{} file(s)", self.dcm.entries.len()));
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
                ui.horizontal_wrapped(|ui| {
                    ui.label("folder");
                    let mut dir = self.dcm.folder.clone();
                    ui.add(egui::TextEdit::singleline(&mut dir).desired_width(340.0));
                    if ui.button("scan").clicked() {
                        if !dir.trim().is_empty() {
                            self.dcm.scan(dir.trim());
                        }
                    }
                    ui.separator();
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
                if !self.dcm.scan_error.is_empty() {
                    ui.colored_label(egui::Color32::from_rgb(255, 180, 90), &self.dcm.scan_error);
                }
                if !self.dcm.load_error.is_empty() {
                    ui.colored_label(egui::Color32::from_rgb(255, 100, 100), &self.dcm.load_error);
                }
            }
        });
    }

    /// DICOM: file list (left) + image (center).
    fn dicom_body(&mut self, ctx: &egui::Context) {
        // Re-upload the image texture only when the file / window / invert
        // actually changed (dirty flag), not every frame.
        if self.dcm.dirty {
            if let Some(f) = &self.dcm.current {
                let img = self.dcm.to_image(f);
                self.dcm_tex.set(img, egui::TextureOptions::LINEAR);
                self.dcm.dirty = false;
            }
        }

        egui::SidePanel::left("files").exact_width(240.0).show(ctx, |ui| {
            ui.heading("Files");
            ui.separator();
            if self.dcm.entries.is_empty() {
                ui.small("Scan a folder of .dcm files to begin.");
            } else {
                // Snapshot so we can call load() (mutable) while iterating.
                let entries: Vec<_> = self
                    .dcm
                    .entries
                    .iter()
                    .map(|e| (e.name.clone(), e.path.clone(), e.rows, e.cols))
                    .collect();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (name, path, rows, cols) in entries {
                        let label = format!("{name}  ({}×{})", cols, rows);
                        let is_sel = self.dcm.current_name == name;
                        if ui.selectable_label(is_sel, label).clicked() {
                            self.dcm.load(&path);
                        }
                    }
                });
            }
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
                    });
                }
                None => {
                    ui.vertical_centered(|ui| {
                        ui.add_space(32.0);
                        ui.label("No file selected.");
                        ui.small("Type a folder path above, hit scan, then pick a file.");
                    });
                }
            }
        });
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([940.0, 560.0])
            .with_title("Imaging viewer — DICOM / synthetic"),
        ..Default::default()
    };
    eframe::run_native(
        "Imaging viewer",
        options,
        Box::new(|cc| Ok(Box::new(XrayApp::new(cc)))),
    )
}
