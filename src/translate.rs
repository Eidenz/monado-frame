// Image translation via an Ollama (OpenAI-compatible) vision model. One call
// does OCR + translation: send the screenshot, ask for English back.
//
// The endpoint is baked in at build time from `translate.env` (see build.rs).
// If it's absent, `configured()` is false and the Translate button is hidden.
use std::path::Path;
use std::time::Duration;

use base64::Engine;

const BASE_URL: Option<&str> = option_env!("MF_TRANSLATE_BASE_URL");
const MODEL: Option<&str> = option_env!("MF_TRANSLATE_MODEL");
const API_KEY: Option<&str> = option_env!("MF_TRANSLATE_API_KEY");

const PROMPT: &str = "Read all text in this image and translate it into English. \
Output only the translation — no preamble, notes, or commentary. \
If there is no readable text, reply exactly: (no text found)";

/// True if a translation endpoint was configured at build time.
pub fn configured() -> bool {
    BASE_URL.is_some() && MODEL.is_some()
}

/// Blocking: send `path` to the vision model and return the English translation.
/// Run this off the render thread — a large model can take many seconds.
pub fn translate_image(path: &Path) -> Result<String, String> {
    let (Some(base), Some(model)) = (BASE_URL, MODEL) else {
        return Err("translation is not configured".into());
    };
    let key = API_KEY.unwrap_or("ollama");

    let bytes = std::fs::read(path).map_err(|e| format!("read image: {e}"))?;
    let data_url = format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(&bytes));

    let body = serde_json::json!({
        "model": model,
        "stream": false,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": PROMPT },
                { "type": "image_url", "image_url": { "url": data_url } }
            ]
        }]
    });

    let url = format!("{}/chat/completions", base.trim_end_matches('/'));
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(180))).build().into();
    let resp = agent
        .post(&url)
        .header("Authorization", &format!("Bearer {key}"))
        .send_json(&body)
        .map_err(|e| format!("request failed: {e}"))?;

    let v: serde_json::Value = resp.into_body().read_json().map_err(|e| format!("bad response: {e}"))?;
    let text = v["choices"][0]["message"]["content"].as_str().ok_or("no content in response")?.trim().to_string();
    if text.is_empty() {
        return Err("empty translation".into());
    }
    Ok(text)
}
