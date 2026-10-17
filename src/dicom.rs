//! DICOM file loading — read a folder of `.dcm` files, extract frame-0 pixel
//! data (raw samples), and report enough metadata to list them.
//!
//! The GUI owns window/level and inversion so it can be adjusted live; this
//! module only decodes samples and dimensions.

use dicom::encoding::adapters::PixelDataObject;
use dicom::object::open_file;
use dicom::pixeldata::PixelDecoder;

/// One file in the folder listing (metadata only; pixels loaded on demand).
#[derive(Debug, Clone)]
pub struct DcmEntry {
    pub path: String,
    pub name: String,
    pub rows: u16,
    pub cols: u16,
    pub transfer_syntax: String,
}

/// A loaded DICOM frame ready for display.
#[derive(Debug)]
pub struct DcmFrame {
    /// Pixel samples, row-major, `rows * cols * spp`, rescaled per the file's
    /// Modality LUT (Rescale Slope/Intercept). Stored as `f32` so both signed
    /// (negative) and unsigned 16-bit data are preserved, and window/level in
    /// the GUI can operate on the real numeric range.
    pub samples: Vec<f32>,
    pub rows: u32,
    pub cols: u32,
    pub spp: u32,
    /// True when the file is MONOCHROME1 (min = white). X-ray convention is
    /// light-on-dark, so the viewer inverts by default in that case.
    pub photometric_invert: bool,
    pub transfer_syntax: String,
}

/// Open a single `.dcm` file and extract frame 0's raw pixel samples.
///
/// Returns `Err(String)` with a human-readable message on failure (unsupported
/// transfer syntax, missing pixel data, etc.).
pub fn open_dcm(path: &str) -> Result<DcmFrame, String> {
    // `obj` must outlive `px` (the decoded data borrows from it).
    let obj = open_file(path).map_err(|e| e.to_string())?;
    let px = obj
        .decode_pixel_data()
        .map_err(|e| format!("decode pixel data: {e}"))?;

    let rows = px.rows();
    let cols = px.columns();
    let spp = px.samples_per_pixel() as u32;
    if rows == 0 || cols == 0 {
        return Err("file has no rows/cols (no pixel data?)".into());
    }

    // Raw samples (Modality LUT applied per file attributes, VOI not applied).
    // f32 target accepts both signed (negative) and unsigned 16-bit values.
    let samples = px
        .to_vec_frame::<f32>(0)
        .map_err(|e| format!("extract frame 0: {e}"))?;

    let expected = (rows as usize) * (cols as usize) * spp as usize;
    if samples.len() != expected {
        return Err(format!(
            "sample count mismatch: got {} expected {}",
            samples.len(),
            expected
        ));
    }

    let photometric_invert =
        matches!(px.photometric_interpretation(), dicom::pixeldata::PhotometricInterpretation::Monochrome1);

    let transfer_syntax = obj.meta().transfer_syntax().to_string();

    Ok(DcmFrame {
        samples,
        rows,
        cols,
        spp,
        photometric_invert,
        transfer_syntax,
    })
}

/// Scan a directory for `.dcm`/`.dicom` files (case-insensitive extension)
/// and return sorted metadata entries. Pixel data is NOT decoded here.
pub fn scan_folder(dir: &str) -> (Vec<DcmEntry>, String) {
    let mut files: Vec<DcmEntry> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => return (files, format!("cannot open folder: {e}")),
    };

    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let is_dcm = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| {
                e.eq_ignore_ascii_case("dcm")
                    || e.eq_ignore_ascii_case("dicom")
            })
            .unwrap_or(false);
        if !is_dcm {
            continue;
        }
        let path_str = path.to_string_lossy().to_string();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path_str.clone());

        // Metadata only — we close the file right away; pixels are decoded
        // later in `open_dcm` when the user actually selects the file.
        match open_file(&path_str) {
            Ok(obj) => {
                let rows = obj.rows().unwrap_or(0);
                let cols = obj.cols().unwrap_or(0);
                let transfer_syntax = obj.transfer_syntax_uid().to_string();
                files.push(DcmEntry {
                    path: path_str,
                    name,
                    rows,
                    cols,
                    transfer_syntax,
                });
            }
            Err(e) => {
                errors.push(format!("{}: {e}", name));
            }
        }
    }

    files.sort_by(|a, b| a.name.cmp(&b.name));
    let error_msg = if errors.is_empty() {
        String::new()
    } else {
        format!("{} file(s) skipped: {}", errors.len(), errors.join("; "))
    };
    (files, error_msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Requires a real DICOM test folder at /home/russ/dcm_test.
    #[test]
    fn integration_decode_real_files() {
        let dir = "/home/russ/dcm_test";
        if !std::path::Path::new(dir).is_dir() {
            eprintln!("(skipped: no {dir} folder)");
            return;
        }
        let (entries, _err) = scan_folder(dir);
        assert!(!entries.is_empty(), "scan_folder found no .dcm files");
        // notes.txt must be excluded.
        assert!(
            entries.iter().all(|e| !e.name.contains("notes.txt")),
            "notes.txt leaked into the listing"
        );
        for e in entries.iter() {
            let f = open_dcm(&e.path).unwrap_or_else(|err| panic!("open_dcm({}) failed: {err}", e.path));
            let expected = (f.rows as usize) * (f.cols as usize) * f.spp as usize;
            assert_eq!(f.samples.len(), expected, "sample count mismatch for {}", e.path);
            assert!(f.rows > 0 && f.cols > 0);
            assert!(!f.samples.is_empty());
            // Signed int16 files can show negative values (rescaled).
            let has_neg = f.samples.iter().any(|&s| s < 0.0);
            if e.name.starts_with("chest_001") || e.name.starts_with("hand_001") {
                // these are signed (PixelRepresentation=1) — allow negatives
                let _ = has_neg;
            }
            println!(
                "  OK {} {}x{} spp={} samples={} mono1={}",
                e.name, f.cols, f.rows, f.spp, f.samples.len(), f.photometric_invert
            );
        }
    }
}
