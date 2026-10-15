//! Embeds the application manifest (common controls 6, per-monitor DPI awareness) into the
//! executable. The MSVC linker merges it with the one it generates, so no resource compiler
//! is needed.

use std::path::Path;

fn main() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("kestrel-shellhost.manifest");
    println!("cargo:rerun-if-changed={}", manifest.display());
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", manifest.display());
    }
}
