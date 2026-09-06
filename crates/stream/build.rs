//! Compiles moonlight-common-c (with its ENet and Reed-Solomon dependencies)
//! and the small C shim that adapts its callback structs to plain function
//! pointers. Crypto is not compiled from C: `src/crypto.rs` provides the
//! `Plt*` functions the library expects.

use std::path::Path;

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../third_party/moonlight-common-c");
    println!("cargo:rerun-if-changed=csrc");
    println!("cargo:rerun-if-changed={}", root.display());
    println!("cargo:rerun-if-env-changed=BROLINK_SKIP_C");
    // `cargo check --target aarch64-apple-darwin` from a non-Mac has no C
    // toolchain for the target; the Rust side can still be type-checked.
    if std::env::var_os("BROLINK_SKIP_C").is_some() {
        return;
    }
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    let mut b = cc::Build::new();
    b.include(root.join("src"))
        .include(root.join("enet/include"))
        .include(root.join("nanors"))
        .include(root.join("nanors/deps"))
        .include(root.join("nanors/deps/obl"))
        .include("csrc")
        .define("NDEBUG", None)
        .define("HAS_SOCKLEN_T", "1")
        .warnings(false)
        .opt_level(2);
    for f in list(&root.join("src")) {
        b.file(f);
    }
    for f in list(&root.join("enet")) {
        b.file(f);
    }
    b.file(root.join("nanors/rs.c"))
        .file(root.join("nanors/deps/obl/oblas_common.c"))
        .file(root.join("nanors/deps/obl/oblas_lite.c"))
        .file("csrc/shim.c");
    if target_os == "windows" {
        b.define("_CRT_SECURE_NO_WARNINGS", None);
        println!("cargo:rustc-link-lib=ws2_32");
        println!("cargo:rustc-link-lib=winmm");
    } else {
        b.flag("-std=gnu11");
    }
    if target_os == "macos" {
        for fw in ["VideoToolbox", "CoreMedia", "CoreVideo", "CoreFoundation"] {
            println!("cargo:rustc-link-lib=framework={fw}");
        }
    }
    b.compile("moonlight-common-c");
}

fn list(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "c"))
        .collect();
    v.sort();
    v
}
