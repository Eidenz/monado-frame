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

pub fn load(ctx: &egui::Context, path: &Path) -> Result<Photo> {
    let img = image::open(path)?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
    let handle = ctx.load_texture("screenshot", color, egui::TextureOptions::LINEAR);
    Ok(Photo { handle })
}

/// Load a downscaled preview (fits within `max` px) as a texture in `ctx`.
pub fn load_thumb(ctx: &egui::Context, path: &Path, max: u32) -> Result<egui::TextureHandle> {
    let img = image::open(path)?.thumbnail(max, max).to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("thumb").to_string();
    Ok(ctx.load_texture(format!("thumb:{name}"), color, egui::TextureOptions::LINEAR))
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

/// Decode the first QR code found in the image, if any.
pub fn decode_qr(path: &Path) -> Option<String> {
    let img = image::open(path).ok()?.into_luma8();
    let mut prep = rqrr::PreparedImage::prepare(img);
    for grid in prep.detect_grids() {
        if let Ok((_meta, content)) = grid.decode() {
            if !content.is_empty() {
                return Some(content);
            }
        }
    }
    None
}
