// The Khronos OpenXR loader ships here as `libopenxr_loader.so.1` without the
// unversioned `libopenxr_loader.so` dev symlink, so the `-lopenxr_loader` that
// the `openxr` crate's `linked` feature emits can't resolve at link time
// (runtime is fine — it's registered with ldconfig). Drop a symlink into
// OUT_DIR and add it to the link search path.
use std::path::Path;

fn main() {
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");
    let link = Path::new(&out_dir).join("libopenxr_loader.so");

    let candidates = [
        "/usr/lib64/libopenxr_loader.so.1",
        "/lib64/libopenxr_loader.so.1",
        "/usr/lib/x86_64-linux-gnu/libopenxr_loader.so.1",
        "/usr/lib/libopenxr_loader.so.1",
    ];

    if !link.exists() {
        for c in candidates {
            if Path::new(c).exists() {
                let _ = std::os::unix::fs::symlink(c, &link);
                break;
            }
        }
    }

    println!("cargo:rustc-link-search=native={out_dir}");

    // Optional feature config baked in at build time from project-root .env
    // files (KEY=value), so endpoints/keys are never typed in VR. Absent file =>
    // feature disabled.
    bake_env("translate.env", &[("base_url", "MF_TRANSLATE_BASE_URL"), ("model", "MF_TRANSLATE_MODEL"), ("api_key", "MF_TRANSLATE_API_KEY")]);
    bake_env("picsur.env", &[("base_url", "MF_PICSUR_BASE_URL"), ("api_key", "MF_PICSUR_API_KEY")]);
}

fn bake_env(file: &str, keys: &[(&str, &str)]) {
    println!("cargo:rerun-if-changed={file}");
    let Ok(txt) = std::fs::read_to_string(file) else { return };
    for line in txt.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            if let Some((_, env)) = keys.iter().find(|(name, _)| *name == k.trim()) {
                println!("cargo:rustc-env={env}={}", v.trim());
            }
        }
    }
}
