// The gesture config file the Monado detector reads (and hot-reloads).
use std::{env, fs, path::Path};

pub struct Settings {
    pub enabled: bool,
    pub hold_ms: i32,
    pub debug: bool,
    pub path: String,
    pub dirty: bool,
}

pub fn config_path() -> String {
    let base = env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{}/.config", env::var("HOME").unwrap_or_default()));
    format!("{base}/monado/gestures.json")
}

pub fn load() -> Settings {
    let path = config_path();
    let mut s = Settings { enabled: true, hold_ms: 2000, debug: false, path: path.clone(), dirty: false };
    if let Ok(txt) = fs::read_to_string(&path) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
            if let Some(b) = v.get("enabled").and_then(|x| x.as_bool()) {
                s.enabled = b;
            }
            if let Some(n) = v.get("hold_ms").and_then(|x| x.as_i64()) {
                s.hold_ms = n as i32;
            }
            if let Some(b) = v.get("debug").and_then(|x| x.as_bool()) {
                s.debug = b;
            }
        }
    }
    s
}

pub fn save(s: &Settings) {
    if let Some(dir) = Path::new(&s.path).parent() {
        let _ = fs::create_dir_all(dir);
    }
    let v = serde_json::json!({ "enabled": s.enabled, "hold_ms": s.hold_ms, "debug": s.debug });
    match serde_json::to_string_pretty(&v) {
        Ok(txt) => match fs::write(&s.path, txt) {
            Ok(()) => log::info!("wrote {} (enabled={} hold_ms={})", s.path, s.enabled, s.hold_ms),
            Err(e) => log::warn!("failed to write {}: {e}", s.path),
        },
        Err(e) => log::warn!("serialise config: {e}"),
    }
}

// monado-frame's own settings (NOT the Monado detector's gestures.json), e.g. QR
// handling. Stored at <XDG config>/monado-frame/config.json.
pub struct AppSettings {
    pub qr_detect: bool,
    pub qr_autodelete: bool,
    pub path: String,
}

pub fn app_config_path() -> String {
    let base = env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{}/.config", env::var("HOME").unwrap_or_default()));
    format!("{base}/monado-frame/config.json")
}

pub fn load_app() -> AppSettings {
    let path = app_config_path();
    let mut a = AppSettings { qr_detect: false, qr_autodelete: false, path: path.clone() };
    if let Ok(txt) = fs::read_to_string(&path) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
            if let Some(b) = v.get("qr_detect").and_then(|x| x.as_bool()) {
                a.qr_detect = b;
            }
            if let Some(b) = v.get("qr_autodelete").and_then(|x| x.as_bool()) {
                a.qr_autodelete = b;
            }
        }
    }
    a
}

pub fn save_app(a: &AppSettings) {
    if let Some(dir) = Path::new(&a.path).parent() {
        let _ = fs::create_dir_all(dir);
    }
    let v = serde_json::json!({ "qr_detect": a.qr_detect, "qr_autodelete": a.qr_autodelete });
    match serde_json::to_string_pretty(&v) {
        Ok(txt) => match fs::write(&a.path, txt) {
            Ok(()) => log::info!("wrote {} (qr_detect={} qr_autodelete={})", a.path, a.qr_detect, a.qr_autodelete),
            Err(e) => log::warn!("failed to write {}: {e}", a.path),
        },
        Err(e) => log::warn!("serialise app config: {e}"),
    }
}
