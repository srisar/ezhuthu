// Embeds ezhuthu.manifest (per-monitor-v2 DPI awareness, common controls v6)
// with the MSVC linker, so no resource compiler or extra crate is needed.
fn main() {
    println!("cargo:rerun-if-changed=ezhuthu.manifest");
    let manifest = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("ezhuthu.manifest");
    println!("cargo:rustc-link-arg-bin=ezhuthu=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bin=ezhuthu=/MANIFESTINPUT:{}",
        manifest.display()
    );
}
