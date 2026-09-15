//! The engine's executables carry the name and icon they were built with,
//! and Task Manager, the volume mixer and a firewall prompt show exactly
//! that. Setup runs `brolink-host --brand-engine <dir>` elevated, with the
//! engine stopped, and this module rewrites two resources in each file in
//! place. The version block gets BroLink's description and product name,
//! and every other string it carried (copyright, licence, version numbers)
//! is read out first and written back unchanged. The first icon group is
//! replaced by BroLink's own, copied out of this executable. Nothing else
//! in the files changes, and the archive they came from is never touched.

use anyhow::Result;
use parking_lot::Mutex;
use std::path::Path;
use std::time::{Duration, Instant};

/// What the engine process is called once branded.
pub const DESCRIPTION: &str = "BroLink Streaming";
pub const SERVICE_DESCRIPTION: &str = "BroLink Streaming Service";

/// Files under the engine directory and the description each gets.
pub const TARGETS: [(&str, &str); 2] = [
    ("sunshine.exe", DESCRIPTION),
    (r"tools\sunshinesvc.exe", SERVICE_DESCRIPTION),
];

/// True when the engine in `dir` already presents itself as BroLink.
pub fn is_branded(dir: &Path) -> bool {
    description(&dir.join(TARGETS[0].0)).as_deref() == Some(DESCRIPTION)
}

/// [`is_branded`], re-read at most every ten seconds: the status is polled
/// often and the answer changes only when setup runs.
pub fn is_branded_cached(dir: &Path) -> bool {
    static CACHE: Mutex<Option<(Instant, bool)>> = Mutex::new(None);
    let mut c = CACHE.lock();
    if let Some((t, v)) = *c {
        if t.elapsed() < Duration::from_secs(10) {
            return v;
        }
    }
    let v = is_branded(dir);
    *c = Some((Instant::now(), v));
    v
}

/// Brand every file in [`TARGETS`] under `dir`. The engine must be stopped:
/// Windows will not rewrite a running executable.
pub fn brand(dir: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        win::brand(dir)
    }
    #[cfg(not(windows))]
    {
        let _ = dir;
        Err(anyhow::anyhow!("the engine can only be branded on Windows"))
    }
}

/// The FileDescription of `exe`'s version block, if it has one.
pub fn description(exe: &Path) -> Option<String> {
    #[cfg(windows)]
    {
        win::description(exe)
    }
    #[cfg(not(windows))]
    {
        let _ = exe;
        None
    }
}

/// The 12-byte entry headers and the image resource ids of an icon group
/// (GRPICONDIR: reserved, type, count, then 14-byte entries).
#[cfg_attr(not(windows), allow(dead_code))]
fn group_entries(group: &[u8]) -> Vec<([u8; 12], u16)> {
    if group.len() < 6 {
        return Vec::new();
    }
    let count = u16::from_le_bytes([group[4], group[5]]) as usize;
    (0..count)
        .filter_map(|i| {
            let at = 6 + i * 14;
            let e = group.get(at..at + 14)?;
            let mut head = [0u8; 12];
            head.copy_from_slice(&e[..12]);
            Some((head, u16::from_le_bytes([e[12], e[13]])))
        })
        .collect()
}

/// A GRPICONDIR naming these images.
#[cfg_attr(not(windows), allow(dead_code))]
fn group_bytes(entries: &[([u8; 12], u16)]) -> Vec<u8> {
    let mut v = vec![0, 0, 1, 0];
    v.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for (head, id) in entries {
        v.extend_from_slice(head);
        v.extend_from_slice(&id.to_le_bytes());
    }
    v
}

#[cfg(windows)]
mod win {
    use super::{group_bytes, group_entries, DESCRIPTION, TARGETS};
    use crate::verinfo::{self, FIXED_LEN};
    use anyhow::{Context, Result};
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{FreeLibrary, BOOL, FALSE, HANDLE, HMODULE, TRUE};
    use windows::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
    };
    use windows::Win32::System::LibraryLoader::{
        BeginUpdateResourceW, EndUpdateResourceW, EnumResourceLanguagesW, EnumResourceNamesW,
        FindResourceW, GetModuleHandleW, LoadLibraryExW, LoadResource, LockResource,
        SizeofResource, UpdateResourceW, LOAD_LIBRARY_AS_DATAFILE, LOAD_LIBRARY_AS_IMAGE_RESOURCE,
    };

    const RT_ICON: PCWSTR = PCWSTR(3 as *const u16);
    const RT_GROUP_ICON: PCWSTR = PCWSTR(14 as *const u16);
    const RT_VERSION: PCWSTR = PCWSTR(16 as *const u16);

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    fn wide_str(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// A resource name: a small integer or a string, as Windows has both.
    #[derive(Clone, Debug, PartialEq)]
    enum Name {
        Id(u16),
        Text(Vec<u16>),
    }

    impl Name {
        fn from_raw(p: PCWSTR) -> Self {
            if (p.0 as usize) >> 16 == 0 {
                Name::Id(p.0 as usize as u16)
            } else {
                let text = unsafe { p.as_wide() };
                Name::Text(text.iter().copied().chain(std::iter::once(0)).collect())
            }
        }

        fn as_pcwstr(&self) -> PCWSTR {
            match self {
                Name::Id(i) => PCWSTR(*i as usize as *const u16),
                Name::Text(t) => PCWSTR(t.as_ptr()),
            }
        }
    }

    unsafe extern "system" fn collect_names(
        _: HMODULE,
        _: PCWSTR,
        name: PCWSTR,
        lparam: isize,
    ) -> BOOL {
        let v = unsafe { &mut *(lparam as *mut Vec<Name>) };
        v.push(Name::from_raw(name));
        TRUE
    }

    unsafe extern "system" fn collect_langs(
        _: HMODULE,
        _: PCWSTR,
        _: PCWSTR,
        lang: u16,
        lparam: isize,
    ) -> BOOL {
        let v = unsafe { &mut *(lparam as *mut Vec<u16>) };
        v.push(lang);
        TRUE
    }

    /// Every resource of `kind` in `module`; none when it has none, which
    /// Windows reports as a failed call.
    fn names(module: HMODULE, kind: PCWSTR) -> Vec<Name> {
        let mut v: Vec<Name> = Vec::new();
        let _ = unsafe {
            EnumResourceNamesW(module, kind, Some(collect_names), &mut v as *mut _ as isize)
        };
        v
    }

    fn langs(module: HMODULE, kind: PCWSTR, name: &Name) -> Vec<u16> {
        let mut v: Vec<u16> = Vec::new();
        let _ = unsafe {
            EnumResourceLanguagesW(
                module,
                kind,
                name.as_pcwstr(),
                Some(collect_langs),
                &mut v as *mut _ as isize,
            )
        };
        v
    }

    fn bytes(module: HMODULE, kind: PCWSTR, name: &Name) -> Option<Vec<u8>> {
        unsafe {
            let h = FindResourceW(module, name.as_pcwstr(), kind);
            if h.0.is_null() {
                return None;
            }
            let g = LoadResource(module, h).ok()?;
            let p = LockResource(g);
            let n = SizeofResource(module, h);
            if p.is_null() || n == 0 {
                return None;
            }
            Some(std::slice::from_raw_parts(p as *const u8, n as usize).to_vec())
        }
    }

    /// BroLink's icon out of this executable: each group entry's header
    /// with its image.
    fn own_icon() -> Result<Vec<([u8; 12], Vec<u8>)>> {
        let me = unsafe { GetModuleHandleW(PCWSTR::null()) }.context("this executable's module")?;
        let group = names(me, RT_GROUP_ICON)
            .into_iter()
            .next()
            .context("this executable has no icon group")?;
        let dir = bytes(me, RT_GROUP_ICON, &group).context("read this executable's icon group")?;
        let mut out = Vec::new();
        for (head, id) in group_entries(&dir) {
            let img = bytes(me, RT_ICON, &Name::Id(id))
                .with_context(|| format!("icon image {id} is missing from this executable"))?;
            out.push((head, img));
        }
        anyhow::ensure!(!out.is_empty(), "this executable's icon group is empty");
        Ok(out)
    }

    /// One VerQueryValue lookup: the value's address and length (in
    /// characters for strings, bytes otherwise), or none.
    fn query(block: &[u8], sub: &str) -> Option<(*const c_void, u32)> {
        let s = wide_str(sub);
        let mut p: *mut c_void = std::ptr::null_mut();
        let mut len = 0u32;
        let ok = unsafe {
            VerQueryValueW(
                block.as_ptr() as *const c_void,
                PCWSTR(s.as_ptr()),
                &mut p,
                &mut len,
            )
        };
        (ok.as_bool() && !p.is_null() && len > 0).then_some((p as *const c_void, len))
    }

    /// The fixed info, the strings and the language of `exe`'s version
    /// block; `None` when it has no block at all.
    #[allow(clippy::type_complexity)]
    fn read_version(exe: &Path) -> Option<([u8; FIXED_LEN], Vec<(String, String)>, u16)> {
        let path = wide(exe.as_os_str());
        let size = unsafe { GetFileVersionInfoSizeW(PCWSTR(path.as_ptr()), None) };
        if size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        unsafe {
            GetFileVersionInfoW(
                PCWSTR(path.as_ptr()),
                0,
                size,
                buf.as_mut_ptr() as *mut c_void,
            )
        }
        .ok()?;
        let mut fixed = verinfo::fixed_default();
        if let Some((p, len)) = query(&buf, "\\") {
            if len as usize >= FIXED_LEN {
                fixed.copy_from_slice(unsafe {
                    std::slice::from_raw_parts(p as *const u8, FIXED_LEN)
                });
            }
        }
        let (lang, cp) = match query(&buf, "\\VarFileInfo\\Translation") {
            Some((p, len)) if len >= 4 => {
                let t = unsafe { std::slice::from_raw_parts(p as *const u16, 2) };
                (t[0], t[1])
            }
            _ => (verinfo::LANG, verinfo::CODEPAGE),
        };
        let mut strings = Vec::new();
        for key in verinfo::KEYS {
            // Some files declare one translation and write their table under
            // the usual one; look in both.
            for table in [format!("{lang:04X}{cp:04X}"), "040904B0".to_string()] {
                if let Some((p, len)) = query(&buf, &format!("\\StringFileInfo\\{table}\\{key}")) {
                    let u = unsafe { std::slice::from_raw_parts(p as *const u16, len as usize) };
                    let s = String::from_utf16_lossy(u)
                        .trim_end_matches('\0')
                        .to_string();
                    if !s.is_empty() {
                        strings.push((key.to_string(), s));
                    }
                    break;
                }
            }
        }
        Some((fixed, strings, lang))
    }

    pub(super) fn description(exe: &Path) -> Option<String> {
        read_version(exe)?
            .1
            .into_iter()
            .find(|(k, _)| k == "FileDescription")
            .map(|(_, v)| v)
    }

    pub(super) fn brand(dir: &Path) -> Result<()> {
        let icon = own_icon()?;
        for (file, description) in TARGETS {
            let exe = dir.join(file);
            brand_file(&exe, description, &icon)
                .with_context(|| format!("brand {}", exe.display()))?;
        }
        anyhow::ensure!(super::is_branded(dir), "the new version block did not take");
        Ok(())
    }

    /// Rewrite `exe`'s version block and first icon group in place.
    fn brand_file(exe: &Path, description: &str, icon: &[([u8; 12], Vec<u8>)]) -> Result<()> {
        anyhow::ensure!(exe.is_file(), "{} is missing", exe.display());
        let (fixed, theirs, lang) =
            read_version(exe).unwrap_or((verinfo::fixed_default(), Vec::new(), verinfo::LANG));
        let strings = verinfo::merged(
            &theirs,
            &[
                ("FileDescription", description),
                ("ProductName", DESCRIPTION),
            ],
        );
        let block = verinfo::build(&fixed, &strings, lang, verinfo::CODEPAGE);

        // What the file holds now, through a data-only mapping that is
        // closed again before the file is opened for writing.
        let path = wide(exe.as_os_str());
        let module = unsafe {
            LoadLibraryExW(
                PCWSTR(path.as_ptr()),
                HANDLE::default(),
                LOAD_LIBRARY_AS_DATAFILE | LOAD_LIBRARY_AS_IMAGE_RESOURCE,
            )
        }
        .with_context(|| format!("open {}", exe.display()))?;
        let versions: Vec<(Name, Vec<u16>)> = names(module, RT_VERSION)
            .into_iter()
            .map(|n| {
                let l = langs(module, RT_VERSION, &n);
                (n, l)
            })
            .collect();
        let first_group = names(module, RT_GROUP_ICON).into_iter().next();
        let old_images: Vec<(Name, Vec<u16>)> = first_group
            .as_ref()
            .and_then(|g| bytes(module, RT_GROUP_ICON, g))
            .map(|d| {
                group_entries(&d)
                    .into_iter()
                    .map(|(_, id)| {
                        let n = Name::Id(id);
                        let l = langs(module, RT_ICON, &n);
                        (n, l)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let group_langs = first_group
            .as_ref()
            .map(|g| langs(module, RT_GROUP_ICON, g))
            .unwrap_or_default();
        // Other icon groups (a tray icon, say) keep their images, so the new
        // ones take ids above every image the file has.
        let next_id = names(module, RT_ICON)
            .iter()
            .filter_map(|n| match n {
                Name::Id(i) => Some(*i),
                Name::Text(_) => None,
            })
            .max()
            .unwrap_or(0)
            + 1;
        let _ = unsafe { FreeLibrary(module) };

        let update = unsafe { BeginUpdateResourceW(PCWSTR(path.as_ptr()), FALSE) }
            .with_context(|| format!("open {} for writing", exe.display()))?;
        let write = || -> Result<()> {
            unsafe {
                for (name, ls) in &versions {
                    for l in ls {
                        UpdateResourceW(update, RT_VERSION, name.as_pcwstr(), *l, None, 0)
                            .context("remove the old version block")?;
                    }
                }
                UpdateResourceW(
                    update,
                    RT_VERSION,
                    Name::Id(1).as_pcwstr(),
                    lang,
                    Some(block.as_ptr() as *const c_void),
                    block.len() as u32,
                )
                .context("write the version block")?;
                for (name, ls) in &old_images {
                    for l in ls {
                        UpdateResourceW(update, RT_ICON, name.as_pcwstr(), *l, None, 0)
                            .context("remove an old icon image")?;
                    }
                }
                if let Some(g) = &first_group {
                    for l in &group_langs {
                        UpdateResourceW(update, RT_GROUP_ICON, g.as_pcwstr(), *l, None, 0)
                            .context("remove the old icon group")?;
                    }
                }
                let mut entries = Vec::new();
                for (i, (head, img)) in icon.iter().enumerate() {
                    let id = next_id + i as u16;
                    UpdateResourceW(
                        update,
                        RT_ICON,
                        PCWSTR(id as usize as *const u16),
                        lang,
                        Some(img.as_ptr() as *const c_void),
                        img.len() as u32,
                    )
                    .context("write an icon image")?;
                    entries.push((*head, id));
                }
                let dir = group_bytes(&entries);
                let group_name = first_group.clone().unwrap_or(Name::Id(1));
                UpdateResourceW(
                    update,
                    RT_GROUP_ICON,
                    group_name.as_pcwstr(),
                    lang,
                    Some(dir.as_ptr() as *const c_void),
                    dir.len() as u32,
                )
                .context("write the icon group")?;
            }
            Ok(())
        };
        match write() {
            Ok(()) => unsafe { EndUpdateResourceW(update, FALSE) }
                .with_context(|| format!("write {}", exe.display())),
            Err(e) => {
                let _ = unsafe { EndUpdateResourceW(update, TRUE) };
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_icon_group_round_trips_through_its_entries() {
        let entries = vec![
            ([16, 16, 0, 0, 1, 0, 32, 0, 0x28, 0x04, 0, 0], 7u16),
            ([0, 0, 0, 0, 1, 0, 32, 0, 0x28, 0x00, 0x04, 0], 8),
        ];
        let bytes = group_bytes(&entries);
        assert_eq!(&bytes[..6], &[0, 0, 1, 0, 2, 0]);
        assert_eq!(bytes.len(), 6 + 14 * 2);
        assert_eq!(group_entries(&bytes), entries);
        // A truncated or empty directory yields nothing rather than a panic.
        assert!(group_entries(&bytes[..6 + 14 + 3]).len() == 1);
        assert!(group_entries(&[]).is_empty());
    }

    #[test]
    fn off_windows_nothing_is_branded_and_branding_says_why() {
        let dir = std::env::temp_dir();
        if !cfg!(windows) {
            assert!(!is_branded(&dir));
            assert!(brand(&dir).unwrap_err().to_string().contains("Windows"));
        }
    }
}
