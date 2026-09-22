//! A log file that does not grow without end.
//!
//! `init_logging` opened `panel.log` and `service.log` in append mode and
//! nothing ever pruned them. Both processes run for days — the panel for as
//! long as the window is open, the service until the machine restarts — so
//! the files only grew: Hermes reached a 32 MB `panel.log`, on the system
//! drive, for a program whose whole install is 14 MB.
//!
//! Writes roll over at `MAX_BYTES`. The previous contents move to
//! `<name>.1`, replacing whatever was there, so a log costs at most twice
//! `MAX_BYTES` on disk and the most recent lines always survive.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The size a log may reach before it rolls over. Large enough to hold a
/// long session's worth of `info` lines, small enough that two of them are
/// never worth noticing.
pub const MAX_BYTES: u64 = 4 * 1024 * 1024;

/// Where the previous generation of `path` is kept: `panel.log.1`. An
/// extension rather than a sibling name, so a directory listing keeps the
/// two together and a `*.log*` clean-up catches both.
pub fn rotated_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".1");
    path.with_file_name(name)
}

/// An append-only log file that rolls over once it passes `MAX_BYTES`.
pub struct RotatingLog {
    path: PathBuf,
    file: File,
    written: u64,
    max_bytes: u64,
}

impl RotatingLog {
    /// Open `path` for appending, continuing to count from its current size
    /// so a log that is already oversized rolls over on the next write
    /// rather than growing until the process happens to restart.
    pub fn open(path: PathBuf) -> io::Result<Self> {
        Self::with_max(path, MAX_BYTES)
    }

    fn with_max(path: PathBuf, max_bytes: u64) -> io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let written = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            path,
            file,
            written,
            max_bytes,
        })
    }

    /// Move what is on disk to `<name>.1` and start the live file again at
    /// zero. Copy and truncate rather than rename: Windows will not rename
    /// a file that is open, and this keeps the handle — and so the tracing
    /// subscriber holding it — valid throughout.
    fn rotate(&mut self) -> io::Result<()> {
        self.file.flush()?;
        std::fs::copy(&self.path, rotated_path(&self.path))?;
        self.file.set_len(0)?;
        self.written = 0;
        Ok(())
    }
}

impl Write for RotatingLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.written + buf.len() as u64 > self.max_bytes {
            // A failed rollover must not lose the line or kill logging: an
            // unwritable `.1` (a permission or disk problem) leaves the
            // live file as it was and the write goes on as an append.
            let _ = self.rotate();
        }
        let n = self.file.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("brolink-logfile-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn rolls_over_once_past_the_limit_and_keeps_one_previous_file() {
        let dir = tmpdir("roll");
        let path = dir.join("panel.log");
        let mut log = RotatingLog::with_max(path.clone(), 64).unwrap();

        // Stay under the limit: one file, nothing rotated.
        log.write_all(&[b'a'; 40]).unwrap();
        log.flush().unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 40);
        assert!(!rotated_path(&path).exists());

        // Cross it: the 40 bytes move aside and the live file holds only
        // what was written after the rollover.
        log.write_all(&[b'b'; 40]).unwrap();
        log.flush().unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 40);
        assert_eq!(std::fs::read(rotated_path(&path)).unwrap(), vec![b'a'; 40]);
        assert_eq!(std::fs::read(&path).unwrap(), vec![b'b'; 40]);

        // A second rollover replaces the previous generation; two files is
        // the whole cost, however long the process runs.
        log.write_all(&[b'c'; 40]).unwrap();
        log.flush().unwrap();
        assert_eq!(std::fs::read(rotated_path(&path)).unwrap(), vec![b'b'; 40]);
        assert_eq!(std::fs::read(&path).unwrap(), vec![b'c'; 40]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_already_oversized_log_rolls_over_on_the_next_write() {
        // The case on Hermes: 32 MB written by a build that never rotated.
        let dir = tmpdir("oversized");
        let path = dir.join("service.log");
        std::fs::write(&path, vec![b'x'; 500]).unwrap();

        let mut log = RotatingLog::with_max(path.clone(), 64).unwrap();
        log.write_all(b"new").unwrap();
        log.flush().unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert_eq!(std::fs::metadata(rotated_path(&path)).unwrap().len(), 500);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_previous_generation_sits_beside_the_log() {
        assert_eq!(
            rotated_path(Path::new("/var/brolink/panel.log")),
            PathBuf::from("/var/brolink/panel.log.1")
        );
    }
}
