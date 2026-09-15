//! Windows only: give the exe an icon and a version block. Explorer, the
//! taskbar and Defender's heuristics all read them; a bare Rust binary has
//! neither. The icon is the same logo the windows draw.

#[cfg(windows)]
#[path = "../core/src/icon.rs"]
#[allow(dead_code)]
mod icon;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../core/src/icon.rs");
    println!("cargo:rerun-if-changed=../core/assets/logo-1024.png");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("brolink.ico");
        std::fs::write(&out, ico(&[16, 24, 32, 48, 64, 256])).expect("write icon");
        let mut res = winresource::WindowsResource::new();
        res.set_icon(out.to_str().unwrap())
            .set("ProductName", "BroLink")
            .set("FileDescription", "BroLink Host")
            .set("CompanyName", "BroLink Contributors")
            .set(
                "LegalCopyright",
                "Copyright (c) 2026 BroLink Contributors, MIT license",
            )
            .set("OriginalFilename", "brolink-host.exe");
        if let Err(e) = res.compile() {
            println!("cargo:warning=no Windows resources embedded: {e}");
        }
    }
}

/// An .ico holding uncompressed 32-bit images at each size.
#[cfg(windows)]
fn ico(sizes: &[u32]) -> Vec<u8> {
    let images: Vec<(u32, Vec<u8>)> = sizes.iter().map(|&s| (s, dib(s))).collect();
    let mut out = Vec::new();
    let put16 = |v: &mut Vec<u8>, x: u16| v.extend_from_slice(&x.to_le_bytes());
    let put32 = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&x.to_le_bytes());
    put16(&mut out, 0);
    put16(&mut out, 1);
    put16(&mut out, images.len() as u16);
    let mut offset = 6 + 16 * images.len() as u32;
    for (s, img) in &images {
        let dim = if *s >= 256 { 0 } else { *s as u8 };
        out.push(dim);
        out.push(dim);
        out.push(0);
        out.push(0);
        put16(&mut out, 1);
        put16(&mut out, 32);
        put32(&mut out, img.len() as u32);
        put32(&mut out, offset);
        offset += img.len() as u32;
    }
    for (_, img) in &images {
        out.extend_from_slice(img);
    }
    out
}

/// BITMAPINFOHEADER + bottom-up BGRA pixels + an empty AND mask.
#[cfg(windows)]
fn dib(size: u32) -> Vec<u8> {
    let rgba = icon::render(size);
    let mut v = Vec::with_capacity(40 + (size * size * 4) as usize);
    let mask_row = (size as usize).div_ceil(32) * 4;
    v.extend_from_slice(&40u32.to_le_bytes());
    v.extend_from_slice(&(size as i32).to_le_bytes());
    v.extend_from_slice(&((size * 2) as i32).to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&32u16.to_le_bytes());
    v.extend_from_slice(&0u32.to_le_bytes());
    v.extend_from_slice(&(size * size * 4 + mask_row as u32 * size).to_le_bytes());
    v.extend_from_slice(&[0u8; 16]);
    for y in (0..size).rev() {
        for x in 0..size {
            let i = ((y * size + x) * 4) as usize;
            v.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
        }
    }
    v.extend(std::iter::repeat_n(0u8, mask_row * size as usize));
    v
}
