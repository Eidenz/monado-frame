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
}
