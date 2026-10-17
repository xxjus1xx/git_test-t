//! X-ray imaging viewer — egui GUI over the headless `xray_viewer` engine.
//!
//! Renders a live detector image of a conveyor belt, with controls for belt
//! speed, defect rate, and fill, plus a live count of products passing the
//! scan gate and a defects tally.

#![allow(clippy::cast_possible_truncation)]
// On Windows, build as a GUI app (no black console window behind the egui window).
// No effect on Linux/macOS.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::time::Instant;

use eframe::egui;
use egui::Color32;
use xray_viewer::engine::{Config, Frame, Source, iron_lut};

const IMG_W: usize = 512;
const IMG_H: usize = 256;
const TEX_NAME: &str = "xray_detector";

struct XrayApp {
    source: Source,
    tex: egui::TextureHandle,
    grayscale: bool,
    fps_ema: f32,
    last_step: Instant,
    // Time accumulator so the engine advances at wall-clock speed regardless
    // of the UI refresh rate (the engine steps once per 1/60 s tick).
    acc: f32,
    seed: u64,
    // The engine exposes setters only, so the app owns the "current value" of
    // each knob and pushes changes downstream.
    belt: f32,
    defect_rate: f32,
    fill: f32,
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

        // One persistent texture; we .set() it every frame so the GPU upload
        // is an in-place update rather than an alloc per frame.
        let blank = egui::ColorImage::new([IMG_W, IMG_H], Color32::from_rgb(235, 235, 235));
        let tex = cc.egui_ctx.load_texture(
            TEX_NAME,
            blank,
            egui::TextureOptions::LINEAR,
        );

        Self {
            source,
            tex,
            grayscale: false,
            fps_ema: 0.0,
            last_step: Instant::now(),
            acc: 0.0,
            seed: 0xC0FFEE,
            belt: 3.0,
            defect_rate: 0.25,
            fill: 0.75,
        }
    }

    /// Map one engine frame into a GPU-ready image (grayscale or warm LUT).
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

        // EMA of the UI refresh rate.
        if dt > 0.0 {
            let inst = 1.0 / dt;
            self.fps_ema = if self.fps_ema == 0.0 { inst } else { 0.9 * self.fps_ema + 0.1 * inst };
        }

        // Advance the engine at wall-clock speed: one engine tick per 1/60 s.
        // Accumulate elapsed time and step a whole number of ticks, so the
        // conveyor speed is correct at any UI refresh rate (no per-frame
        // truncation to 0, no over-stepping on high-FPS frames).
        self.acc += dt;
        let tick = 1.0 / 60.0;
        let mut steps = (self.acc / tick).floor() as u32;
        self.acc -= steps as f32 * tick;
        // Cap catch-up so a long stall (window minimized) can't spike.
        let steps = steps.min(300);
        if steps > 0 {
            let mut latest = None;
            for _ in 0..steps {
                latest = Some(self.source.next_frame());
            }
            if let Some(f) = latest {
                self.tex.set(self.frame_to_image(&f), egui::TextureOptions::LINEAR);
            }
        }

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.strong("X-ray imaging viewer");
                ui.separator();
                ui.label(format!("FPS {:.0}", self.fps_ema));
                ui.separator();
                ui.label(format!("count {}", self.source.count));
                ui.label(format!("defects {}", self.source.defect_count));
                ui.separator();
                ui.checkbox(&mut self.grayscale, "grayscale");
                if ui.button("reset").clicked() {
                    self.source.reset();
                }
                if ui.button("reseed").clicked() {
                    let new_seed = self.seed.wrapping_add(1);
                    self.seed = new_seed;
                    self.source = Source::new(Config {
                        seed: new_seed,
                        width: IMG_W as u16,
                        height: IMG_H as u16,
                        belt_speed: self.belt,
                        container_width: 64.0,
                        fill_fraction: self.fill,
                        defect_rate: self.defect_rate,
                    });
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.label("belt speed");
                let mut speed = self.belt;
                ui.add(egui::Slider::new(&mut speed, 0.5..=12.0).suffix(" px/f"));
                self.belt = speed;
                self.source.set_belt_speed(speed);

                ui.separator();
                ui.label("defect rate");
                let mut dr = self.defect_rate;
                let dr_label = format!("{:.2}", dr);
                ui.add(egui::Slider::new(&mut dr, 0.0..=1.0).text(dr_label));
                self.defect_rate = dr;
                self.source.set_defect_rate(dr);

                ui.separator();
                ui.label("fill");
                let mut fill = self.fill;
                let fill_label = format!("{:.2}", fill);
                ui.add(egui::Slider::new(&mut fill, 0.10..=1.0).text(fill_label));
                self.fill = fill;
                self.source.set_fill_fraction(fill);
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add(
                    egui::Image::new((self.tex.id(), self.tex.size_vec2()))
                        .max_size(egui::vec2(IMG_W as f32, IMG_H as f32)),
                );
                ui.small(format!(
                    "seed {:X} · {} on belt",
                    self.seed,
                    self.source.on_screen()
                ));
            });
        });
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([IMG_W as f32 + 8.0, IMG_H as f32 + 130.0])
            .with_title("X-ray imaging viewer"),
        ..Default::default()
    };
    eframe::run_native(
        "X-ray imaging viewer",
        options,
        Box::new(|cc| Ok(Box::new(XrayApp::new(cc)))),
    )
}
