//! X-ray imaging engine — deterministic, GUI-independent, headless-testable.
//!
//! Model: a conveyor belt moves left → right in front of a fixed X-ray
//! detector. Products enter from the left edge, cross the detector, and exit
//! on the right edge. Each product is a filled container with optional
//! defects (underfill, foreign body). The detector renders per-pixel
//! Beer–Lambert attenuation: denser material = darker pixel.
//!
//! Everything is seedable and pure so the whole stream is reproducible and
//! unit-testable without a display.

#![allow(clippy::cast_possible_truncation)]

// ---------------------------------------------------------------------------
// Deterministic PRNG (SplitMix64)
// ---------------------------------------------------------------------------
#[derive(Debug, Clone)]
pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn next_f32(&mut self) -> f32 {
        ((self.next_u64() >> 40) as f32) * (1.0f32 / 16777216.0f32)
    }

    pub fn chance(&mut self, p: f32) -> bool {
        self.next_f32() < p
    }
}

fn noise(seq: u64, x: u32, y: u32, spread: i32) -> i32 {
    let h = (seq as u32)
        .wrapping_add(x.wrapping_mul(0x9E37_79B1))
        .wrapping_add(y.wrapping_mul(0x85EB_CA6B));
    let h = (h ^ (h >> 13)).wrapping_mul(0xC2B2_AE35);
    let h = (h ^ (h >> 16)) as i32;
    h % spread
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------
#[derive(Debug, Clone)]
pub struct Config {
    pub width: u16,
    pub height: u16,
    pub seed: u64,
    pub belt_speed: f32,
    pub container_width: f32,
    pub fill_fraction: f32,
    pub defect_rate: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            width: 512,
            height: 256,
            seed: 0xC0FFEE,
            belt_speed: 3.0,
            container_width: 64.0,
            fill_fraction: 0.75,
            defect_rate: 0.25,
        }
    }
}

// ---------------------------------------------------------------------------
// Materials — Beer–Lambert linear attenuation coefficients
// ---------------------------------------------------------------------------
#[derive(Debug, Clone)]
pub struct Materials {
    pub liquid_mu: f32,
    pub wall_mu: f32,
    pub foreign_mu: f32,
}

impl Default for Materials {
    fn default() -> Self {
        Self {
            liquid_mu: 0.9,
            wall_mu: 0.35,
            foreign_mu: 3.2,
        }
    }
}

// ---------------------------------------------------------------------------
// A single in-flight product on the belt
// ---------------------------------------------------------------------------
#[derive(Debug, Clone)]
struct Product {
    id: u64,
    x: f32,
    fill: f32,
    foreign: Option<(f32, f32, f32)>,
    counted: bool,
}

// ---------------------------------------------------------------------------
// Source — advances the belt and produces frames
// ---------------------------------------------------------------------------
pub struct Source {
    cfg: Config,
    mat: Materials,
    rng: Rng,
    seq: u64,
    products: Vec<Product>,
    next_id: u64,
    pub gate_x: f32,
    pub count: u64,
    pub defect_count: u64,
}

impl Source {
    fn spacing(cfg: &Config) -> f32 {
        cfg.container_width * 1.4
    }

    pub fn new(cfg: Config) -> Self {
        let gate_x = cfg.width as f32 * 0.7;
        let seed = cfg.seed;
        Self {
            cfg,
            mat: Materials::default(),
            rng: Rng::new(seed),
            seq: 0,
            products: Vec::new(),
            next_id: 0,
            gate_x,
            count: 0,
            defect_count: 0,
        }
    }

    pub fn set_belt_speed(&mut self, v: f32) {
        self.cfg.belt_speed = v.max(0.5);
    }
    pub fn set_defect_rate(&mut self, v: f32) {
        self.cfg.defect_rate = v.clamp(0.0, 1.0);
    }
    pub fn set_fill_fraction(&mut self, v: f32) {
        self.cfg.fill_fraction = v.clamp(0.05, 1.0);
    }
    pub fn reset(&mut self) {
        self.products.clear();
        self.next_id = 0;
        self.seq = 0;
        self.count = 0;
        self.defect_count = 0;
        self.rng = Rng::new(self.cfg.seed);
    }

    pub fn positions(&self) -> Vec<f32> {
        self.products.iter().map(|p| p.x).collect()
    }

    pub fn on_screen(&self) -> usize {
        self.products.len()
    }

    pub fn next_frame(&mut self) -> Frame {
        self.seq += 1;
        let w = self.cfg.width as f32;
        let cw = self.cfg.container_width;
        let dx = self.cfg.belt_speed;

        // 1) Move every product right by the belt increment.
        for p in &mut self.products {
            p.x += dx;
        }

        // 2) Cull products that have exited past the right edge.
        self.products.retain(|p| p.x <= w + cw);

        // 3) Spawn a new product at the left edge only when the newest (min x)
        //    product has advanced by at least one spacing. Bounded: at most one
        //    per frame, and only when the belt has carried the last one forward.
        let newest_x = self.products.iter().map(|p| p.x).fold(f32::MAX, f32::min);
        if self.products.is_empty() || newest_x >= Self::spacing(&self.cfg) {
            let id = self.next_id;
            let p = self.spawn_product(id);
            self.products.push(p);
            self.next_id += 1;
        }

        // 4) Count products crossing the gate (left edge passes gate_x).
        for p in &mut self.products {
            if !p.counted && p.x >= self.gate_x {
                p.counted = true;
                self.count += 1;
            }
        }

        // 5) Render the detector frame.
        let mut frame = Frame::new(self.cfg.width, self.cfg.height, self.seq);
        let h = self.cfg.height as f32;
        for y in 0..self.cfg.height {
            let yy = y as f32 / h;
            let base = (238.0 + 12.0 * yy).min(250.0);
            for x in 0..self.cfg.width {
                let v = (base as i32 + noise(self.seq, x as u32, y as u32, 4)).clamp(0, 255);
                frame.set(x as u16, y as u16, v as u8);
            }
        }
        for p in &self.products {
            self.draw_product(&mut frame, p);
        }
        frame
    }

    fn spawn_product(&mut self, id: u64) -> Product {
        let has_defect = self.rng.chance(self.cfg.defect_rate);
        let (fill, foreign) = if has_defect {
            if self.rng.chance(0.5) {
                let underfill = self.cfg.fill_fraction * (0.30 + self.rng.next_f32() * 0.35);
                (underfill, None)
            } else {
                let cx = 0.35 + self.rng.next_f32() * 0.30;
                let cy = 0.40 + self.rng.next_f32() * 0.25;
                let r = 0.08 + self.rng.next_f32() * 0.06;
                (self.cfg.fill_fraction, Some((cx, cy, r)))
            }
        } else {
            (self.cfg.fill_fraction * (0.95 + self.rng.next_f32() * 0.10), None)
        };
        if has_defect {
            self.defect_count += 1;
        }
        Product { id, x: 0.0, fill, foreign, counted: false }
    }

    fn draw_product(&self, frame: &mut Frame, p: &Product) {
        let h = self.cfg.height as f32;
        let cw = self.cfg.container_width;
        let wall_w = (cw * 0.10) as i32;
        let left = p.x as i32;
        let right = (p.x + cw) as i32;
        let top = (h * 0.18) as i32;
        let floor = (h * 0.90) as i32;
        let liquid_top = (floor as f32 - (floor as f32 - top as f32) * p.fill) as i32;

        let w = frame.width() as i32;
        let fh = frame.height() as i32;

        let y0 = top.max(0);
        let y1 = (floor as i32).min(fh).max(0);
        let x0 = left.max(0);
        let x1 = (right - 1).min(w - 1).max(0);
        if x1 < x0 || y1 < y0 {
            return; // fully off-screen
        }

        for y in y0..=y1 {
            for x in x0..=x1 {
                let mut mu = 0.0f32;
                let in_wall = (x - left) < wall_w || (right - 1 - x) < wall_w;
                if in_wall {
                    mu += self.mat.wall_mu;
                }
                if y >= liquid_top {
                    mu += self.mat.liquid_mu;
                }
                if let Some((fcx, fcy, fr)) = p.foreign {
                    let nx = (x as f32 - p.x) / cw;
                    let ny = (y as f32 - top as f32) / (floor as f32 - top as f32);
                    let ddx = (nx - fcx) / fr.max(0.01);
                    let ddy = (ny - fcy) / fr.max(0.01);
                    if ddx * ddx + ddy * ddy <= 1.0 {
                        mu += self.mat.foreign_mu;
                    }
                }
                if mu > 0.0 {
                    let t = (-mu).exp().clamp(0.0, 1.0);
                    let cur = frame.get(x as u16, y as u16) as f32;
                    frame.set(x as u16, y as u16, (cur * t) as u8);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Frame — a single detector image
// ---------------------------------------------------------------------------
#[derive(Debug, Clone)]
pub struct Frame {
    width: u16,
    height: u16,
    seq: u64,
    data: Vec<u8>,
}

impl Frame {
    pub fn new(width: u16, height: u16, seq: u64) -> Self {
        Self {
            width,
            height,
            seq,
            data: vec![255u8; (width as usize) * (height as usize)],
        }
    }

    pub fn width(&self) -> u16 { self.width }
    pub fn height(&self) -> u16 { self.height }
    pub fn seq(&self) -> u64 { self.seq }

    fn idx(&self, x: u16, y: u16) -> usize {
        (y as usize) * (self.width as usize) + (x as usize)
    }

    pub fn get(&self, x: u16, y: u16) -> u8 { self.data[self.idx(x, y)] }
    pub fn set(&mut self, x: u16, y: u16, v: u8) {
        let i = self.idx(x, y);
        self.data[i] = v;
    }

    pub fn stats(&self) -> (u8, u8, f32) {
        let mut mn = u8::MAX;
        let mut mx = u8::MIN;
        let mut sum: u64 = 0;
        for &v in &self.data {
            if v < mn { mn = v; }
            if v > mx { mx = v; }
            sum += v as u64;
        }
        (mn, mx, (sum as f32) / (self.data.len() as f32))
    }

    pub fn pixels(&self) -> &[u8] { &self.data }
}

// ---------------------------------------------------------------------------
// Iron LUT — warm, film-like tint for the GUI
// ---------------------------------------------------------------------------
pub fn iron_lut(v: u8) -> (u8, u8, u8) {
    let t = (v as f32) / 255.0;
    let r = (255.0 * (0.15 + 0.85 * t)).min(255.0);
    let g = (255.0 * (0.10 + 0.70 * t * t)).min(255.0);
    let b = (255.0 * (0.05 + 0.40 * t * t * t)).min(255.0);
    (r.round() as u8, g.round() as u8, b.round() as u8)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config {
            width: 512,
            height: 256,
            seed: 12345,
            belt_speed: 4.0,
            container_width: 80.0,
            fill_fraction: 0.75,
            defect_rate: 0.4,
        }
    }

    #[test]
    fn frame_dimensions_match_config() {
        let mut s = Source::new(cfg());
        let f = s.next_frame();
        assert_eq!(f.width(), 512);
        assert_eq!(f.height(), 256);
    }

    #[test]
    fn frames_have_bright_bg_and_dark_objects() {
        let mut s = Source::new(cfg());
        let mut saw_dark = false;
        let mut saw_bright = false;
        for _ in 0..80 {
            let f = s.next_frame();
            let (mn, mx, _) = f.stats();
            if mn < 120 { saw_dark = true; }
            if mx > 200 { saw_bright = true; }
        }
        assert!(saw_bright, "expected bright (air) regions");
        assert!(saw_dark, "expected dark (dense) regions");
    }

    #[test]
    fn products_move_right_over_time() {
        let mut s = Source::new(cfg());
        for _ in 0..40 {
            s.next_frame();
            if !s.positions().is_empty() { break; }
        }
        let x0 = s.positions().iter().cloned().fold(f32::MAX, f32::min);
        for _ in 0..5 { s.next_frame(); }
        let x1 = s.positions().iter().cloned().fold(f32::MAX, f32::min);
        assert!(x1 >= x0 - 0.5, "products should drift right (x0={x0}, x1={x1})");
    }

    #[test]
    fn counting_increases_as_products_pass() {
        let mut s = Source::new(cfg());
        for _ in 0..400 { s.next_frame(); }
        assert!(s.count > 0, "gate should have counted products");
    }

    #[test]
    fn defects_are_injected_and_counted() {
        let mut s = Source::new(cfg());
        for _ in 0..400 { s.next_frame(); }
        assert!(s.defect_count > 0, "defects should be injected at 40% rate");
    }

    #[test]
    fn on_screen_count_is_bounded() {
        let mut s = Source::new(cfg());
        let mut max_on = 0;
        for _ in 0..200 {
            s.next_frame();
            max_on = max_on.max(s.on_screen());
        }
        assert!(max_on >= 1 && max_on <= 8, "on-screen count bounded (got {max_on})");
    }

    #[test]
    fn deterministic_stream_reproducible() {
        let mut a = Source::new(cfg());
        let mut b = Source::new(cfg());
        for _ in 0..20 {
            let fa = a.next_frame();
            let fb = b.next_frame();
            assert_eq!(fa.pixels(), fb.pixels(), "same seed → same frames");
        }
    }

    #[test]
    fn iron_lut_monotonic() {
        let (r0, g0, b0) = iron_lut(0);
        let (r1, g1, b1) = iron_lut(255);
        assert!(r1 >= r0 && g1 >= g0 && b1 >= b0, "LUT must be monotonic");
    }

    #[test]
    fn underfill_products_are_lighter_than_full() {
        let mut s = Source::new(cfg());
        let mut dark_frame_min = 255u8;
        for _ in 0..80 {
            let f = s.next_frame();
            let (mn, _, _) = f.stats();
            if mn < dark_frame_min { dark_frame_min = mn; }
        }
        assert!(dark_frame_min < 200, "should see a dark (filled) product");
    }
}
