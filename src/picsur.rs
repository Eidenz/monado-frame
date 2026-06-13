// Upload an image to a Picsur instance and return the shareable link.
//
// The endpoint + API key are baked in at build time from `picsur.env` (see
// build.rs). Absent => `configured()` is false and the Share button is hidden.
use std::path::Path;
use std::time::Duration;

const BASE_URL: Option<&str> = option_env!("MF_PICSUR_BASE_URL");
const API_KEY: Option<&str> = option_env!("MF_PICSUR_API_KEY");

/// True if a Picsur instance was configured at build time.
pub fn configured() -> bool {
    BASE_URL.is_some() && API_KEY.is_some()
}

/// Blocking: upload `path` to Picsur, return the `/i/<id>.png` share URL.
/// Run off the render thread.
pub fn upload(path: &Path) -> Result<String, String> {
    let (Some(base), Some(key)) = (BASE_URL, API_KEY) else {
        return Err("picsur is not configured".into());
    };
    let base = base.trim_end_matches('/');
    let bytes = std::fs::read(path).map_err(|e| format!("read image: {e}"))?;
    let filename = path.file_name().and_then(|s| s.to_str()).unwrap_or("screenshot.png");

    // multipart/form-data with a single file field named "image" (Picsur grabs
    // the first file part regardless of name).
    let boundary = "----monadoframeKZ7sQ2x9bnd";
    let mut body = Vec::new();
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"image\"; filename=\"{filename}\"\r\nContent-Type: image/png\r\n\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(&bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let url = format!("{base}/api/image/upload");
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(60))).build().into();
    let resp = agent
        .post(&url)
        .header("Authorization", &format!("Api-Key {key}"))
        .header("Content-Type", &format!("multipart/form-data; boundary={boundary}"))
        .send(&body[..])
        .map_err(|e| format!("upload failed: {e}"))?;

    let v: serde_json::Value = resp.into_body().read_json().map_err(|e| format!("bad response: {e}"))?;
    if v["success"].as_bool() != Some(true) {
        return Err(v["data"]["message"].as_str().unwrap_or("upload rejected").to_string());
    }
    let id = v["data"]["id"].as_str().ok_or("no id in response")?;
    Ok(format!("{base}/i/{id}.png"))
}
