//! Put a real playback device back as Windows' default after the engine
//! starts. The engine's own init disables Steam Streaming Speakers when
//! they are the default, which leaves a PC with no other active device
//! with no default endpoint and no sound on the stream.

#[cfg(windows)]
mod win {
    use anyhow::{bail, Context, Result};
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;
    use std::process::Command;

    const TASK: &str = "BroLinkEngineUser";
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    fn helper_dir() -> Result<PathBuf> {
        let dir = brolink_core::config::data_dir()?;
        std::fs::create_dir_all(&dir).with_context(|| dir.display().to_string())?;
        Ok(dir)
    }

    pub(crate) fn install_helpers() -> Result<PathBuf> {
        let dir = helper_dir()?;
        std::fs::write(
            dir.join("take-over-engine.ps1"),
            include_str!("../windows/take-over-engine.ps1"),
        )?;
        std::fs::write(
            dir.join("take-over-engine.cmd"),
            include_str!("../windows/take-over-engine.cmd"),
        )?;
        let cmd = dir.join("take-over-engine.cmd");
        let tr = cmd.display().to_string();
        let mut ok = create_logon_task(&tr, true);
        if !ok {
            ok = create_logon_task(&tr, false);
        }
        if !ok {
            tracing::info!("could not register {TASK}; the host will start the engine itself");
        }
        Ok(cmd)
    }

    fn create_logon_task(tr: &str, highest: bool) -> bool {
        let mut args = vec![
            "/Create", "/TN", TASK, "/TR", tr, "/SC", "ONLOGON", "/IT", "/F",
        ];
        if highest {
            args.extend(["/RL", "HIGHEST"]);
        }
        Command::new("schtasks")
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn session_id() -> u32 {
        let mut id = 0u32;
        let _ = unsafe {
            windows::Win32::System::Threading::ProcessIdToSessionId(
                windows::Win32::System::Threading::GetCurrentProcessId(),
                &mut id,
            )
        };
        id
    }

    /// Start the engine as the logged-on user and restore a default playback
    /// device. Safe to call often: a running user-session engine is left
    /// alone aside from putting the default back.
    pub fn take_over_engine() -> Result<()> {
        let cmd = install_helpers()?;
        if session_id() == 0 {
            let status = Command::new("schtasks")
                .args(["/Run", "/TN", TASK])
                .creation_flags(CREATE_NO_WINDOW)
                .status()
                .context("run logon engine task")?;
            if !status.success() {
                bail!("could not run {TASK}");
            }
            return Ok(());
        }
        let status = Command::new("powershell")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(cmd.with_extension("ps1"))
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .context("take over engine")?;
        if !status.success() {
            bail!("engine take-over failed");
        }
        Ok(())
    }
}

#[cfg(windows)]
pub use win::{install_helpers, take_over_engine};

#[cfg(not(windows))]
pub fn take_over_engine() -> anyhow::Result<()> {
    Ok(())
}
