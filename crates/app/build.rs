// Embeds ezhuthu.manifest (per-monitor-v2 DPI awareness, common controls v6)
// and the icons (exe icon, tray on/off) with the MSVC linker, so no resource
// compiler or extra crate is needed.

#[path = "icons/res.rs"]
mod res;

use std::path::PathBuf;

/// Icon resource ids; tray.rs loads the tray ones by the same numbers.
const ICONS: &[(u16, &str)] = &[(1, "ezhuthu.ico"), (2, "tray-on.ico"), (3, "tray-off.ico")];

fn main() {
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    println!("cargo:rerun-if-changed=ezhuthu.manifest");
    println!("cargo:rerun-if-changed=icons");
    let manifest = dir.join("ezhuthu.manifest");
    println!("cargo:rustc-link-arg-bin=ezhuthu=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bin=ezhuthu=/MANIFESTINPUT:{}",
        manifest.display()
    );

    let icons: Vec<_> = ICONS
        .iter()
        .map(|&(id, name)| (id, dir.join("icons").join(name)))
        .collect();
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("icons.res");
    std::fs::write(&out, res::icons(&icons)).unwrap();
    println!("cargo:rustc-link-arg-bin=ezhuthu={}", out.display());
}
