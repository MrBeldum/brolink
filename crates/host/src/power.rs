//! Remote power actions.

use anyhow::Result;
use brolink_core::api::PowerAction;

/// Carry out `action`. Sleep returns once the request is accepted (the OS
/// suspends a moment later); restart and shutdown schedule themselves a few
/// seconds out so the reply has left the machine.
pub fn perform(action: PowerAction) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows::Win32::Foundation::BOOLEAN;
        use windows::Win32::System::Power::SetSuspendState;
        match action {
            PowerAction::Sleep => {
                // Wake events stay enabled: that is what lets a magic packet
                // bring the machine back.
                let ok = unsafe { SetSuspendState(BOOLEAN(0), BOOLEAN(1), BOOLEAN(0)) };
                anyhow::ensure!(
                    ok.0 != 0,
                    "SetSuspendState refused: {}",
                    std::io::Error::last_os_error()
                );
                Ok(())
            }
            PowerAction::Restart | PowerAction::Shutdown => {
                let flag = if action == PowerAction::Restart {
                    "/r"
                } else {
                    "/s"
                };
                // /f: a remote user cannot answer a "save changes?" dialog, and
                // a PC left on with one open is the worst outcome.
                let status = std::process::Command::new("shutdown")
                    .args([flag, "/f", "/t", "3"])
                    .creation_flags(0x0800_0000)
                    .status()?;
                anyhow::ensure!(status.success(), "shutdown.exe exited with {status}");
                Ok(())
            }
        }
    }
    #[cfg(not(windows))]
    {
        anyhow::bail!("{} is only supported on Windows", action.label())
    }
}
