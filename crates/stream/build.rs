//! Compiles moonlight-common-c (with its ENet and Reed-Solomon dependencies)
//! and the small C shim that adapts its callback structs to plain function
//! pointers. Crypto is not compiled from C: `src/crypto.rs` provides the
//! `Plt*` functions the library expects. What is vendored, and why, is in
//! `third_party/moonlight-common-c/VERSION`.

use std::path::Path;

/// Every C file the library needs, relative to the vendored root. Listed
/// rather than globbed, so a re-vendor that brings new files in has to
/// decide about each one.
const SOURCES: &[&str] = &[
    "src/AudioStream.c",
    "src/ByteBuffer.c",
    "src/Connection.c",
    "src/ControlStream.c",
    "src/FakeCallbacks.c",
    "src/InputStream.c",
    "src/LinkedBlockingQueue.c",
    "src/Misc.c",
    "src/Platform.c",
    "src/PlatformSockets.c",
    "src/RtpAudioQueue.c",
    "src/RtpVideoQueue.c",
    "src/RtspConnection.c",
    "src/RtspParser.c",
    "src/SdpGenerator.c",
    "src/VideoDepacketizer.c",
    "src/VideoStream.c",
    "enet/callbacks.c",
    "enet/host.c",
    "enet/list.c",
    "enet/packet.c",
    "enet/peer.c",
    "enet/protocol.c",
    // Each is empty on the other platform.
    "enet/unix.c",
    "enet/win32.c",
    "nanors/rs.c",
    "nanors/deps/obl/oblas_common.c",
    "nanors/deps/obl/oblas_lite.c",
];

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
        .opt_level(2)
        .files(SOURCES.iter().map(|f| root.join(f)))
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
