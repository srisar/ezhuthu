// Embeds the files ezhuthu-setup installs: the release ezhuthu.exe and the
// model assets listed in tools/assets.sha256.
//
// Without them (e.g. a plain `cargo build --workspace`) the setup still
// compiles but refuses to run, so workspace builds and tests keep working.
// tools/build-installer.sh sets EZHUTHU_REQUIRE_PAYLOAD=1 to make a missing
// file a build error instead.
//
// Also links the app's icon in as the setup's own (resource id 1).

#[path = "../app/icons/res.rs"]
mod res;

use std::fmt::Write;
use std::path::{Path, PathBuf};

fn main() {
    let root = Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../..");
    let exe = std::env::var_os("EZHUTHU_EXE")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target/release/ezhuthu.exe"));
    let sums = root.join("tools/assets.sha256");
    let manifest =
        Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("ezhuthu-setup.manifest");

    println!("cargo:rerun-if-env-changed=EZHUTHU_EXE");
    println!("cargo:rerun-if-env-changed=EZHUTHU_REQUIRE_PAYLOAD");
    println!("cargo:rerun-if-changed={}", sums.display());
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rustc-link-arg-bin=ezhuthu-setup=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bin=ezhuthu-setup=/MANIFESTINPUT:{}",
        manifest.display()
    );

    let icon = root.join("crates/app/icons/ezhuthu.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    let icon_res = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("icons.res");
    std::fs::write(&icon_res, res::icons(&[(1, icon)])).unwrap();
    println!(
        "cargo:rustc-link-arg-bin=ezhuthu-setup={}",
        icon_res.display()
    );

    // (path inside the install folder, source file)
    let mut files = vec![("ezhuthu.exe".to_string(), exe)];
    for line in std::fs::read_to_string(&sums).unwrap_or_default().lines() {
        if let Some(name) = line.split_whitespace().nth(1) {
            let name = name.trim_start_matches('*');
            files.push((format!("assets/{name}"), root.join("assets").join(name)));
        }
    }

    let missing: Vec<String> = files
        .iter()
        .filter(|(_, src)| !src.is_file())
        .map(|(_, src)| src.display().to_string())
        .collect();
    let mut code = String::from("pub const FILES: &[(&str, &[u8])] = &[\n");
    if missing.is_empty() {
        for (name, src) in &files {
            println!("cargo:rerun-if-changed={}", src.display());
            let src = src.canonicalize().unwrap();
            writeln!(
                code,
                "    ({name:?}, include_bytes!({:?})),",
                src.display().to_string()
            )
            .unwrap();
        }
    } else if std::env::var_os("EZHUTHU_REQUIRE_PAYLOAD").is_some() {
        panic!("setup payload missing: {}", missing.join(", "));
    } else {
        for m in &missing {
            println!("cargo:rerun-if-changed={m}");
        }
        println!(
            "cargo:warning=ezhuthu-setup built without its payload (missing {}); use tools/build-installer.sh",
            missing.join(", ")
        );
    }
    code.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("payload.rs");
    std::fs::write(out, code).unwrap();
}
