//! VS_VERSIONINFO, the block Task Manager, the volume mixer and Explorer's
//! Details tab read a program's name from. Built by hand because Windows
//! offers no call that changes one string in an existing block: setup gives
//! the streaming engine's executables BroLink's description and icon, and
//! everything else they carried - copyright, licence, version numbers - is
//! read out first and written back unchanged.
//!
//! Layout (all little-endian, every node 32-bit aligned):
//! `wLength, wValueLength, wType, szKey, pad, Value, pad, Children`. The
//! root's value is VS_FIXEDFILEINFO; StringFileInfo holds one StringTable
//! keyed by language and code page; VarFileInfo holds the Translation pair.

/// Byte length of VS_FIXEDFILEINFO.
pub const FIXED_LEN: usize = 52;

/// US English, Unicode: the table every version tool looks in first.
pub const LANG: u16 = 0x0409;
pub const CODEPAGE: u16 = 0x04B0;

/// The string names Windows documents, in the order its tools list them.
pub const KEYS: [&str; 12] = [
    "Comments",
    "CompanyName",
    "FileDescription",
    "FileVersion",
    "InternalName",
    "LegalCopyright",
    "LegalTrademarks",
    "OriginalFilename",
    "PrivateBuild",
    "ProductName",
    "ProductVersion",
    "SpecialBuild",
];

/// A fixed block that says only "a Windows NT application": the signature,
/// the structure version, the OS and the file type. Versions stay zero.
pub fn fixed_default() -> [u8; FIXED_LEN] {
    let mut v = [0u8; FIXED_LEN];
    let put = |v: &mut [u8; FIXED_LEN], at: usize, x: u32| {
        v[at..at + 4].copy_from_slice(&x.to_le_bytes())
    };
    put(&mut v, 0, 0xFEEF_04BD); // dwSignature
    put(&mut v, 4, 0x0001_0000); // dwStrucVersion
    put(&mut v, 24, 0x3F); // dwFileFlagsMask
    put(&mut v, 32, 0x0004_0004); // dwFileOS: VOS_NT_WINDOWS32
    put(&mut v, 36, 1); // dwFileType: VFT_APP
    v
}

/// `existing` with `overrides` applied: a key in both takes the override, a
/// key only in `overrides` is added, and the result follows [`KEYS`] order
/// so the block is the same bytes however the input was ordered. Unknown
/// keys are kept after the documented ones, in their original order.
pub fn merged(existing: &[(String, String)], overrides: &[(&str, &str)]) -> Vec<(String, String)> {
    let value = |key: &str| -> Option<String> {
        overrides
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.to_string())
            .or_else(|| {
                existing
                    .iter()
                    .find(|(k, _)| k == key)
                    .map(|(_, v)| v.clone())
            })
    };
    let mut out: Vec<(String, String)> = KEYS
        .iter()
        .filter_map(|k| value(k).map(|v| (k.to_string(), v)))
        .collect();
    for (k, v) in existing {
        if !KEYS.contains(&k.as_str()) && !out.iter().any(|(o, _)| o == k) {
            out.push((k.clone(), v.clone()));
        }
    }
    out.retain(|(_, v)| !v.is_empty());
    out
}

/// The whole block for one language table.
pub fn build(
    fixed: &[u8; FIXED_LEN],
    strings: &[(String, String)],
    lang: u16,
    codepage: u16,
) -> Vec<u8> {
    let table: Vec<Vec<u8>> = strings.iter().map(|(k, v)| string_node(k, v)).collect();
    let string_table = node(&format!("{lang:04X}{codepage:04X}"), 1, 0, &[], &table);
    let string_file_info = node("StringFileInfo", 1, 0, &[], &[string_table]);
    let translation = ((codepage as u32) << 16 | lang as u32).to_le_bytes();
    let var = node("Translation", 0, 4, &translation, &[]);
    let var_file_info = node("VarFileInfo", 1, 0, &[], &[var]);
    node(
        "VS_VERSION_INFO",
        0,
        FIXED_LEN as u16,
        fixed,
        &[string_file_info, var_file_info],
    )
}

fn string_node(key: &str, value: &str) -> Vec<u8> {
    let wide: Vec<u8> = value
        .encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect();
    // wValueLength counts WCHARs including the terminator for strings.
    node(key, 1, (wide.len() / 2) as u16, &wide, &[])
}

/// One node: header, key, padding, value, then each child on a 32-bit
/// boundary. `wLength` covers the node itself; the padding before the next
/// sibling belongs to the parent, which is how Windows walks the tree.
fn node(key: &str, kind: u16, value_len: u16, value: &[u8], children: &[Vec<u8>]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&0u16.to_le_bytes());
    v.extend_from_slice(&value_len.to_le_bytes());
    v.extend_from_slice(&kind.to_le_bytes());
    for u in key.encode_utf16().chain(std::iter::once(0)) {
        v.extend_from_slice(&u.to_le_bytes());
    }
    pad4(&mut v);
    v.extend_from_slice(value);
    for c in children {
        pad4(&mut v);
        v.extend_from_slice(c);
    }
    let len = u16::try_from(v.len()).expect("a version block fits in 64 KiB");
    v[0..2].copy_from_slice(&len.to_le_bytes());
    v
}

fn pad4(v: &mut Vec<u8>) {
    while !v.len().is_multiple_of(4) {
        v.push(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader written from the layout in the module doc, not from
    /// `build`, so a builder bug does not pass its own test.
    struct Node<'a> {
        key: String,
        kind: u16,
        value: &'a [u8],
        children: Vec<Node<'a>>,
    }

    fn read(b: &[u8]) -> Node<'_> {
        let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
        let len = u16_at(0) as usize;
        let value_len = u16_at(2) as usize;
        let kind = u16_at(4);
        let mut i = 6;
        let mut key = Vec::new();
        while u16_at(i) != 0 {
            key.push(u16_at(i));
            i += 2;
        }
        i += 2;
        i = (i + 3) & !3;
        let value_bytes = if kind == 1 { value_len * 2 } else { value_len };
        let value = &b[i..i + value_bytes];
        i += value_bytes;
        let mut children = Vec::new();
        while i < len {
            i = (i + 3) & !3;
            if i >= len {
                break;
            }
            let child = read(&b[i..len]);
            let child_len = u16_at(i) as usize;
            i += child_len;
            children.push(child);
        }
        Node {
            key: String::from_utf16(&key).unwrap(),
            kind,
            value,
            children,
        }
    }

    fn strings(root: &Node<'_>) -> Vec<(String, String)> {
        let sfi = root
            .children
            .iter()
            .find(|c| c.key == "StringFileInfo")
            .unwrap();
        let table = &sfi.children[0];
        table
            .children
            .iter()
            .map(|s| {
                let u: Vec<u16> = s
                    .value
                    .chunks(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                (
                    s.key.clone(),
                    String::from_utf16(&u)
                        .unwrap()
                        .trim_end_matches('\0')
                        .to_string(),
                )
            })
            .collect()
    }

    #[test]
    fn the_block_reads_back_and_keeps_what_it_was_given() {
        let mut fixed = fixed_default();
        fixed[8..12].copy_from_slice(&0x0000_07E2u32.to_le_bytes()); // some file version
        let given = vec![
            ("CompanyName".to_string(), "LizardByte".to_string()),
            (
                "FileDescription".to_string(),
                "BroLink Streaming".to_string(),
            ),
            (
                "LegalCopyright".to_string(),
                "https://example/LICENSE".to_string(),
            ),
            ("ProductName".to_string(), "BroLink Streaming".to_string()),
            ("ProductVersion".to_string(), "2026.906.222525".to_string()),
        ];
        let b = build(&fixed, &given, LANG, CODEPAGE);
        let root = read(&b);
        assert_eq!(root.key, "VS_VERSION_INFO");
        assert_eq!(root.kind, 0);
        assert_eq!(root.value, &fixed[..]);
        assert_eq!(u16::from_le_bytes([b[0], b[1]]) as usize, b.len());
        assert_eq!(strings(&root), given);
        let vfi = root
            .children
            .iter()
            .find(|c| c.key == "VarFileInfo")
            .unwrap();
        assert_eq!(vfi.children[0].key, "Translation");
        assert_eq!(vfi.children[0].value, &[0x09, 0x04, 0xB0, 0x04]);
        let sfi = root
            .children
            .iter()
            .find(|c| c.key == "StringFileInfo")
            .unwrap();
        assert_eq!(sfi.children[0].key, "040904B0");
        assert_eq!(b.len() % 4, 0);
    }

    #[test]
    fn the_fixed_block_is_a_windows_application() {
        let f = fixed_default();
        assert_eq!(&f[0..4], &0xFEEF_04BDu32.to_le_bytes());
        assert_eq!(&f[32..36], &0x0004_0004u32.to_le_bytes());
        assert_eq!(&f[36..40], &1u32.to_le_bytes());
        // Windows reads the signature at byte 40 of the block: 6 header
        // bytes, "VS_VERSION_INFO" plus NUL (32 bytes), padding to 40.
        let b = build(&f, &[], LANG, CODEPAGE);
        assert_eq!(&b[40..44], &0xFEEF_04BDu32.to_le_bytes());
    }

    #[test]
    fn merging_overrides_ours_and_keeps_theirs_in_documented_order() {
        let theirs = vec![
            ("ProductName".to_string(), "Sunshine".to_string()),
            ("LegalCopyright".to_string(), "their licence".to_string()),
            ("CompanyName".to_string(), "LizardByte".to_string()),
            ("FileDescription".to_string(), "Sunshine".to_string()),
            ("Custom".to_string(), "kept".to_string()),
            ("Comments".to_string(), String::new()),
        ];
        let m = merged(
            &theirs,
            &[
                ("FileDescription", "BroLink Streaming"),
                ("ProductName", "BroLink Streaming"),
            ],
        );
        assert_eq!(
            m,
            vec![
                ("CompanyName".to_string(), "LizardByte".to_string()),
                (
                    "FileDescription".to_string(),
                    "BroLink Streaming".to_string()
                ),
                ("LegalCopyright".to_string(), "their licence".to_string()),
                ("ProductName".to_string(), "BroLink Streaming".to_string()),
                ("Custom".to_string(), "kept".to_string()),
            ]
        );
        // A file with no block at all gets only what BroLink says.
        assert_eq!(
            merged(&[], &[("FileDescription", "BroLink Streaming Service")]),
            vec![(
                "FileDescription".to_string(),
                "BroLink Streaming Service".to_string()
            )]
        );
    }
}
