//! Put a real playback device back as Windows' default after the engine
//! starts. The engine's own init disables Steam Streaming Speakers when
//! they are the default, which leaves a PC with no other active device
//! with no default endpoint and no sound on the stream.

#[cfg(windows)]
mod win {
    use anyhow::{bail, Context, Result};
    use std::os::windows::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, Ordering};

    const TASK: &str = "LatchEngineUser";
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    /// The helper scripts, written into the data directory when they are
    /// not there as this build has them, and the logon task that runs
    /// them, registered once per process. The service calls this every 30
    /// seconds while the engine has no sound, and it used to rewrite all
    /// three files and re-register the task each time.
    pub fn install_helpers() -> Result<PathBuf> {
        static TASK_REGISTERED: AtomicBool = AtomicBool::new(false);
        let dir = latch_core::config::data_dir()?;
        let script = dir.join("take-over-engine.ps1");
        write_if_changed(&script, include_str!("../windows/take-over-engine.ps1"))?;
        let launcher = dir.join("take-over-engine.vbs");
        write_if_changed(&launcher, include_str!("../windows/take-over-engine.vbs"))?;
        // Earlier versions pointed the task at a .cmd, which kept a terminal
        // on the desktop for as long as the helper ran. A task an
        // administrator registered cannot be re-pointed from here, so the
        // .cmd stays, reduced to handing off to the windowless launcher.
        write_if_changed(
            &dir.join("take-over-engine.cmd"),
            include_str!("../windows/take-over-engine.cmd"),
        )?;
        if !TASK_REGISTERED.load(Ordering::Relaxed) {
            let tr = format!("wscript.exe //B //Nologo \"{}\"", launcher.display());
            if create_logon_task(&tr) {
                TASK_REGISTERED.store(true, Ordering::Relaxed);
                // An earlier version's task runs its own copy of the helper
                // from the old folder, and two would race to start the
                // engine. One an administrator registered survives this;
                // setup removes it.
                delete_logon_task(crate::legacy::WINDOWS_ENGINE_TASK);
            } else {
                tracing::info!("could not register {TASK}; the host will start the engine itself");
            }
        }
        Ok(script)
    }

    fn write_if_changed(path: &Path, text: &str) -> Result<()> {
        if std::fs::read(path).is_ok_and(|have| have == text.as_bytes()) {
            return Ok(());
        }
        std::fs::write(path, text).with_context(|| path.display().to_string())
    }

    fn delete_logon_task(name: &str) {
        let _ = Command::new("schtasks")
            .args(["/Delete", "/TN", name, "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }

    fn create_logon_task(tr: &str) -> bool {
        // These helpers live in the user's profile and must never gain an
        // elevated token through a scheduled task, including on upgrades.
        let args = [
            "/Create", "/TN", TASK, "/TR", tr, "/SC", "ONLOGON", "/IT", "/F", "/RL", "LIMITED",
        ];
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
            windows::Win32::System::RemoteDesktop::ProcessIdToSessionId(std::process::id(), &mut id)
        };
        id
    }

    /// Start the engine as the logged-on user and restore a default playback
    /// device. Safe to call often: a running user-session engine is left
    /// alone aside from putting the default back.
    pub fn take_over_engine() -> Result<()> {
        let script = install_helpers()?;
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
            .arg(&script)
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
