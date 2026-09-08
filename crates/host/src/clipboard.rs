//! The PC's clipboard as text, for `/v1/clipboard`: what a Mac reads after
//! a ⌘C on the PC, and what it writes before a ⌘V.
//!
//! The service runs in the signed-in user's session (it starts from the Run
//! key), so the clipboard it sees is the one on the desktop. Only text is
//! handled; an image or a file on the clipboard reads as empty text.

use anyhow::Result;
use brolink_core::api::Clipboard;

/// Windows ends lines with CRLF; the Mac with LF. Both directions convert,
/// so a paste never carries stray carriage returns.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn to_windows(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', "\r\n")
}

#[cfg_attr(not(windows), allow(dead_code))]
pub fn from_windows(text: &str) -> String {
    text.replace("\r\n", "\n")
}

#[cfg(windows)]
pub fn read() -> Result<Clipboard> {
    use windows::Win32::Foundation::HGLOBAL;
    use windows::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, GetClipboardSequenceNumber, IsClipboardFormatAvailable,
    };
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    use windows::Win32::System::Ole::CF_UNICODETEXT;

    let format = u32::from(CF_UNICODETEXT.0);
    unsafe {
        let seq = u64::from(GetClipboardSequenceNumber());
        if IsClipboardFormatAvailable(format).is_err() {
            return Ok(Clipboard::fit("", seq));
        }
        open()?;
        let read = || -> Result<String> {
            let handle = GetClipboardData(format)?;
            let mem = HGLOBAL(handle.0);
            let p = GlobalLock(mem) as *const u16;
            anyhow::ensure!(!p.is_null(), "the clipboard memory could not be locked");
            let units = GlobalSize(mem) / 2;
            let all = std::slice::from_raw_parts(p, units);
            let n = all.iter().position(|&c| c == 0).unwrap_or(units);
            let text = String::from_utf16_lossy(&all[..n]);
            // GlobalUnlock reports "unlocked" as an error; nothing to do.
            let _ = GlobalUnlock(mem);
            Ok(text)
        };
        let result = read();
        let _ = CloseClipboard();
        Ok(Clipboard::fit(&from_windows(&result?), seq))
    }
}

#[cfg(windows)]
pub fn write(text: &str) -> Result<()> {
    use windows::Win32::Foundation::{GlobalFree, HANDLE};
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows::Win32::System::Ole::CF_UNICODETEXT;

    let mut wide: Vec<u16> = to_windows(text).encode_utf16().collect();
    wide.push(0);
    unsafe {
        let mem = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2)?;
        let p = GlobalLock(mem) as *mut u16;
        if p.is_null() {
            let _ = GlobalFree(mem);
            anyhow::bail!("could not lock memory for the clipboard");
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
        let _ = GlobalUnlock(mem);
        if let Err(e) = open() {
            let _ = GlobalFree(mem);
            return Err(e);
        }
        let placed = EmptyClipboard()
            .and_then(|()| SetClipboardData(u32::from(CF_UNICODETEXT.0), HANDLE(mem.0)));
        let _ = CloseClipboard();
        match placed {
            // The system owns the memory from here on.
            Ok(_) => Ok(()),
            Err(e) => {
                let _ = GlobalFree(mem);
                Err(e.into())
            }
        }
    }
}

/// Another program may hold the clipboard for a moment; try a few times.
#[cfg(windows)]
unsafe fn open() -> Result<()> {
    use windows::Win32::System::DataExchange::OpenClipboard;
    let mut last = None;
    for _ in 0..8 {
        match OpenClipboard(None) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        }
    }
    anyhow::bail!(
        "another program is holding the clipboard ({})",
        last.map(|e| e.to_string()).unwrap_or_default()
    )
}

#[cfg(not(windows))]
pub fn read() -> Result<Clipboard> {
    anyhow::bail!("the clipboard is only served on Windows")
}

#[cfg(not(windows))]
pub fn write(_text: &str) -> Result<()> {
    anyhow::bail!("the clipboard is only served on Windows")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_endings_convert_both_ways_without_doubling() {
        assert_eq!(to_windows("a\nb"), "a\r\nb");
        assert_eq!(to_windows("a\r\nb"), "a\r\nb");
        assert_eq!(from_windows("a\r\nb\r\n"), "a\nb\n");
        assert_eq!(from_windows(&to_windows("x\ny\n")), "x\ny\n");
        assert_eq!(to_windows(""), "");
    }
}
