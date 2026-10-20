//! Post-processing filters applied to the *displayed* grayscale buffer, i.e.
//! after window/level + invert (the same buffer the viewer would draw).
//!
//! These are pure functions on an `&[u8]` (0-255) pixel buffer, sized
//! `rows * cols`, so they are fully unit-testable with no GUI or DICOM
//! involved. The GUI owns the parameters; this module owns the math.
//!
//! All filters are single- or local-pass (no large convolution kernels) so they
//! stay fast on multi-megapixel images without extra dependencies.

/// Apply a gamma curve: `out = 255 * (in/255)^gamma`.
///
/// `gamma == 1.0` is an exact identity (returns an equal buffer).
/// `gamma < 1.0` brightens midtones; `gamma > 1.0` darkens them.
/// Monotonic non-decreasing, so ordering is preserved.
pub fn gamma(src: &[u8], gamma: f64) -> Vec<u8> {
    let g = gamma.clamp(0.05, 8.0);
    src.iter()
        .map(|&b| {
            let t = b as f64 / 255.0;
            (255.0 * t.powf(g)).clamp(0.0, 255.0) as u8
        })
        .collect()
}

/// Contrast adjustment about the mid-gray (127.5) center:
/// `out = clamp(127.5 + (in - 127.5) * k)`.
///
/// `k == 1.0` is an exact identity. `k > 1.0` increases contrast (pushes values
/// away from center); `k < 1.0` decreases it. 0 and 255 clamp.
pub fn contrast(src: &[u8], k: f64) -> Vec<u8> {
    let k = k.clamp(0.0, 8.0);
    src.iter()
        .map(|&b| {
            let v = 127.5 + ((b as f64 - 127.5) * k);
            v.clamp(0.0, 255.0) as u8
        })
        .collect()
}

/// Sharpen via an unsharp-mask (edge enhancement). `amount == 0.0` returns an
/// identical buffer (no-op).
///
/// For each pixel: `out = center + amount * (center - mean(4-neighbors))`.
/// - Flat region → `center - mean == 0` → unchanged (flat stays flat).
/// - Bright side of an edge → pushed brighter; dark side → pushed darker.
/// Borders are clamped (nearest). `amount` clamped to [0, 2].
pub fn sharpen(src: &[u8], rows: u32, cols: u32, amount: f64) -> Vec<u8> {
    let a = amount.clamp(0.0, 2.0);
    if a == 0.0 || rows == 0 || cols == 0 {
        return src.to_vec();
    }
    let r = rows as usize;
    let c = cols as usize;
    let mut out = vec![0u8; r * c];
    for y in 0..r {
        for x in 0..c {
            let center = src[y * c + x] as f64;
            // 4-connected neighbor mean (up/down/left/right), clamped at borders.
            let up = src[((y as usize).max(1).saturating_sub(1)) * c + x] as f64;
            let down = src[((y as usize + 1).min(r - 1)) * c + x] as f64;
            let left = src[y * c + (x as usize).max(1).saturating_sub(1)] as f64;
            let right = src[y * c + (x as usize + 1).min(c - 1)] as f64;
            let mean = (up + down + left + right) / 4.0;
            let v = center + a * (center - mean);
            out[y * c + x] = v.clamp(0.0, 255.0) as u8;
        }
    }
    out
}

/// Apply the full filter chain in display order: gamma → contrast → sharpen.
/// Each step's output feeds the next.
pub fn apply_chain(
    src: &[u8],
    rows: u32,
    cols: u32,
    gamma: f64,
    contrast_k: f64,
    sharpen: f64,
) -> Vec<u8> {
    let g = crate::filters::gamma(src, gamma);
    let c = crate::filters::contrast(&g, contrast_k);
    crate::filters::sharpen(&c, rows, cols, sharpen)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2x2 buffer with distinct values.
    fn buf(vals: &[u8]) -> Vec<u8> {
        vals.to_vec()
    }

    #[test]
    fn gamma_identity_at_one() {
        let b = buf(&[0, 40, 120, 200, 255]);
        assert_eq!(gamma(&b, 1.0), b);
    }

    #[test]
    fn gamma_monotonic_and_bounded() {
        let b = (0..=255).collect::<Vec<u8>>();
        for g in [0.5f64, 1.5, 2.5] {
            let o = gamma(&b, g);
            assert_eq!(o.len(), b.len());
            // monotonic non-decreasing
            for i in 1..o.len() {
                assert!(o[i] >= o[i - 1], "gamma({g}) not monotonic at {}", i);
            }
            assert!(o.iter().all(|&v| (0..=255).contains(&v)));
        }
    }

    #[test]
    fn gamma_endpoint_preserved() {
        let b = buf(&[0, 255]);
        for g in [0.5f64, 1.0, 2.0, 5.0] {
            let o = gamma(&b, g);
            assert_eq!(o[0], 0, "gamma({g}) should keep black at 0");
            assert_eq!(o[1], 255, "gamma({g}) should keep white at 255");
        }
    }

    #[test]
    fn gamma_darkens_high_values_when_gt_1() {
        // A mid value should be darker (smaller) for gamma > 1.
        let o = gamma(&[128], 2.0);
        assert!(o[0] < 128, "gamma 2.0 should darken 128, got {}", o[0]);
        // and brighter for gamma < 1.
        let o = gamma(&[128], 0.5);
        assert!(o[0] > 128, "gamma 0.5 should brighten 128, got {}", o[0]);
    }

    #[test]
    fn contrast_identity_at_one() {
        let b = (0..=255).collect::<Vec<u8>>();
        assert_eq!(contrast(&b, 1.0), b);
    }

    #[test]
    fn contrast_increases_spread() {
        // Two mid values 20 apart should be pushed further apart for k>1.
        let b = buf(&[100, 120]);
        let o = contrast(&b, 2.0);
        assert!(
            o[1] as i64 - o[0] as i64 > 20,
            "contrast 2.0 should widen the gap, got {} -> {} (diff {})",
            o[0],
            o[1],
            o[1] as i64 - o[0] as i64
        );
    }

    #[test]
    fn contrast_preserves_mid_and_bounds() {
        // Mid-gray stays ~127 for any k (it's the center).
        let o = contrast(&[127, 128], 3.0);
        assert!(o[0] >= 126 && o[0] <= 129);
        assert!(o[1] >= 126 && o[1] <= 129);
        // Extremes clamp, never overflow.
        let o = contrast(&[0, 255], 5.0);
        assert_eq!(o[0], 0);
        assert_eq!(o[1], 255);
    }

    #[test]
    fn sharpen_noop_at_zero() {
        let b = buf(&[10, 200, 30, 220]);
        assert_eq!(sharpen(&b, 2, 2, 0.0), b);
    }

    #[test]
    fn sharpen_flat_region_unchanged() {
        // A completely flat image has no edges → sharpen leaves it unchanged
        // (within the kernel's constant handling).
        let b = vec![120u8; 25]; // 5x5 flat
        let o = sharpen(&b, 5, 5, 1.0);
        // Center pixels of a flat field should remain ~120.
        let center = o[2 * 5 + 2];
        assert!((center as i64 - 120).abs() <= 2, "flat center drifted: {center}");
    }

    #[test]
    fn sharpen_accentuates_edge() {
        // A step edge: left half dark, right half bright. The pixel just inside
        // the bright side should be pushed brighter than 200 after sharpen.
        let rows = 1u32;
        let cols = 4u32;
        let b = vec![10u8, 10, 200, 200];
        let o = sharpen(&b, rows, cols, 1.0);
        assert!(o.len() == 4);
        // The transition pixel (index 2) should be at least as bright as 200.
        assert!(o[2] >= 200, "edge pixel should be accentuated, got {}", o[2]);
        // And the dark side neighbor should be pushed darker.
        assert!(o[1] <= 10, "dark neighbor should be darkened, got {}", o[1]);
    }

    #[test]
    fn chain_preserves_length_and_bounds() {
        let b = (0..=255).collect::<Vec<u8>>();
        let o = apply_chain(&b, 1, 256, 1.3, 1.1, 0.5);
        assert_eq!(o.len(), b.len());
        assert!(o.iter().all(|&v| (0..=255).contains(&v)));
    }

    #[test]
    fn chain_identity_when_all_neutral() {
        // gamma=1, contrast=1, sharpen=0 → identical buffer.
        let b: Vec<u8> = (0..=255).collect();
        let o = apply_chain(&b, 1, 256, 1.0, 1.0, 0.0);
        assert_eq!(o, b);
    }
}
