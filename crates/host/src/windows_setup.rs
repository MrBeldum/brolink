//! Current-user autostart for the host.

use anyhow::Result;
use std::path::Path;

const VALUE: &str = "BroLinkHost";
const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

pub fn set_start_with_windows(enable: bool, exe: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        if enable {
            let quoted = run_value(exe);
            let status = std::process::Command::new("reg")
                .args(["add", KEY, "/v", VALUE, "/t", "REG_SZ", "/d", &quoted, "/f"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()?;
            if !status.success() {
                anyhow::bail!("could not write the Start-with-Windows registry value");
            }
        } else {
            let _ = std::process::Command::new("reg")
                .args(["delete", KEY, "/v", VALUE, "/f"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (enable, exe);
        Ok(())
    }
}

pub fn run_value(exe: &Path) -> String {
    format!("\"{}\"", exe.display())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn run_value_quotes_the_path() {
        let p = PathBuf::from(r"C:\Users\Ada\AppData\Local\BroLink\brolink-host.exe");
        assert_eq!(
            run_value(&p),
            r#""C:\Users\Ada\AppData\Local\BroLink\brolink-host.exe""#
        );
    }
}
