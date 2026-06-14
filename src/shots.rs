// Screenshot discovery, decoding, and clipboard.
use std::{fs, path::Path, path::PathBuf, time::SystemTime};

use anyhow::Result;
use chrono::{Local, TimeZone};

pub struct Photo {
    pub handle: egui::TextureHandle,
}

pub enum PhotoAction {
    None,
    Copy,
    Delete,
    Dismiss,
    Translate,
    ToggleView,
    Share,
}

/// All *.png in `dir`, newest first, with modified times.
pub fn scan_all(dir: &str) -> Vec<(PathBuf, SystemTime)> {
    let mut v = Vec::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            let is_png = p.extension().and_then(|x| x.to_str()).is_some_and(|x| x.eq_ignore_ascii_case("png"));
            if !is_png {
                continue;
            }
            if let Ok(m) = e.metadata().and_then(|md| md.modified()) {
                v.push((p, m));
            }
        }
    }
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v
}

// Max edge (px) for the in-VR photo texture; the file on disk stays full quality.
const VIEW_MAX: u32 = 1600;

pub fn load(ctx: &egui::Context, path: &Path) -> Result<Photo> {
    let mut img = image::open(path)?;
    if img.width().max(img.height()) > VIEW_MAX {
        img = img.resize(VIEW_MAX, VIEW_MAX, image::imageops::FilterType::Triangle);
    }
    let img = img.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
    let handle = ctx.load_texture("screenshot", color, egui::TextureOptions::LINEAR);
    Ok(Photo { handle })
}

/// Decode a downscaled preview (fits within `max` px) as raw pixels — no egui
/// context, so it can run on a worker thread (the texture upload is cheap and
/// done on the main thread).
pub fn load_thumb_image(path: &Path, max: u32) -> Result<egui::ColorImage> {
    let img = image::open(path)?.thumbnail(max, max).to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    Ok(egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()))
}

/// Human-readable local timestamp, taken from the Unix time baked into Monado's
/// `monado_screenshot_<unixtime>_<n>.png` filename, falling back to file mtime.
pub fn shot_time(path: &Path) -> String {
    let secs = path
        .file_stem()
        .and_then(|s| s.to_str())
        .and_then(|stem| stem.split('_').filter_map(|p| p.parse::<i64>().ok()).find(|n| *n > 1_000_000_000))
        .or_else(|| {
            fs::metadata(path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
        });
    match secs.and_then(|s| Local.timestamp_opt(s, 0).single()) {
        Some(dt) => dt.format("%Y/%m/%d  %H:%M:%S").to_string(),
        None => String::new(),
    }
}

pub fn copy_to_clipboard(path: &str) {
    match fs::File::open(path) {
        Ok(file) => match std::process::Command::new("wl-copy")
            .arg("--type")
            .arg("image/png")
            .stdin(std::process::Stdio::from(file))
            .spawn()
        {
            Ok(_) => log::info!("copied {path} to clipboard"),
            Err(e) => log::warn!("wl-copy failed ({e}); is wl-clipboard installed?"),
        },
        Err(e) => log::warn!("copy: cannot open {path}: {e}"),
    }
}

pub fn copy_text_to_clipboard(text: &str) {
    use std::io::Write;
    match std::process::Command::new("wl-copy").stdin(std::process::Stdio::piped()).spawn() {
        Ok(mut child) => {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            log::info!("copied text to clipboard");
        }
        Err(e) => log::warn!("wl-copy (text) failed ({e}); is wl-clipboard installed?"),
    }
}

/// Delete screenshots older than `days` (no-op if days <= 0). Returns the count removed.
pub fn cleanup_old(dir: &str, days: i32) -> usize {
    if days <= 0 {
        return 0;
    }
    let cutoff = match SystemTime::now().checked_sub(std::time::Duration::from_secs(days as u64 * 86_400)) {
        Some(t) => t,
        None => return 0,
    };
    let mut removed = 0;
    for (path, mtime) in scan_all(dir) {
        if mtime < cutoff && fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

pub enum ShotOutcome {
    Qr(String),                 // a QR code was decoded (file maybe deleted)
    Photo(egui::ColorImage),    // a normal photo: downscaled thumbnail pixels
}

/// Process a freshly-captured screenshot, meant to run OFF the render thread (a
/// full-res PNG decode/encode is heavy). One decode: detect a QR (and optionally
/// delete the file) → else crop the configured margin (fast PNG re-encode, mtime
/// preserved so the watcher doesn't re-detect it) and return a thumbnail.
pub fn process_new_shot(path: &Path, qr_detect: bool, qr_autodelete: bool, crop_pct: i32, thumb_max: u32) -> Result<ShotOutcome> {
    let img = image::open(path)?;

    if qr_detect {
        let mut prep = rqrr::PreparedImage::prepare(img.to_luma8());
        for grid in prep.detect_grids() {
            if let Ok((_meta, content)) = grid.decode() {
                if !content.is_empty() {
                    if qr_autodelete {
                        let _ = fs::remove_file(path);
                    }
                    return Ok(ShotOutcome::Qr(content));
                }
            }
        }
    }

    let img = maybe_crop(img, path, crop_pct);
    let thumb = img.thumbnail(thumb_max, thumb_max).to_rgba8();
    let size = [thumb.width() as usize, thumb.height() as usize];
    Ok(ShotOutcome::Photo(egui::ColorImage::from_rgba_unmultiplied(size, thumb.as_raw())))
}

// Crop `pct` off each edge and overwrite the file (fast PNG, mtime preserved).
// Returns the (possibly cropped) image for downstream thumbnailing.
fn maybe_crop(img: image::DynamicImage, path: &Path, pct: i32) -> image::DynamicImage {
    if pct <= 0 {
        return img;
    }
    let m = pct.clamp(0, 45) as f32 / 100.0;
    let (w, h) = (img.width(), img.height());
    let (dx, dy) = ((w as f32 * m) as u32, (h as f32 * m) as u32);
    let (cw, ch) = (w.saturating_sub(dx * 2), h.saturating_sub(dy * 2));
    if cw == 0 || ch == 0 {
        return img;
    }
    let cropped = img.crop_imm(dx, dy, cw, ch);
    let mtime = fs::metadata(path).and_then(|md| md.modified()).ok();
    if let Err(e) = save_png_fast(&cropped, path) {
        log::warn!("crop save {path:?}: {e}");
    } else if let Some(t) = mtime {
        let _ = filetime::set_file_mtime(path, filetime::FileTime::from_system_time(t));
    }
    cropped
}

// Encode a PNG with fast compression — full-res re-encode is the slowest part of
// cropping, and default compression would spike the CPU hard.
fn save_png_fast(img: &image::DynamicImage, path: &Path) -> Result<()> {
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    use image::ImageEncoder;
    let file = std::io::BufWriter::new(fs::File::create(path)?);
    let enc = PngEncoder::new_with_quality(file, CompressionType::Fast, FilterType::Adaptive);
    enc.write_image(img.as_bytes(), img.width(), img.height(), img.color().into())?;
    Ok(())
}
