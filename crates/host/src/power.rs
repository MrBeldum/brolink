//! Remote power actions, and keeping the PC from idle-sleeping.

use anyhow::Result;
use brolink_core::api::PowerAction;

/// Prevent Windows from idle-sleeping while BroLink Host is running, so
/// Tailscale stays up. User-initiated Sleep (Start menu or the Mac) still
/// works. Call this from the background service, not the control panel:
/// the execution state is per-thread, and `refresh_loop` is the one caller.
pub fn keep_awake(on: bool) {
    #[cfg(windows)]
    {
        use windows::Win32::System::Power::{
            GetSystemPowerStatus, SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED,
            SYSTEM_POWER_STATUS,
        };
        // An execution-state request is reversible and belongs to this
        // thread; it never rewrites the owner's power plan (setup does that
        // once, on purpose), so turning the setting off truly turns it off.
        // The setting promises "while plugged in": a laptop on battery is
        // left to sleep. Desktops report AC power.
        unsafe {
            let mut power = SYSTEM_POWER_STATUS::default();
            let plugged_in = GetSystemPowerStatus(&mut power).is_ok() && power.ACLineStatus == 1;
            if on && plugged_in {
                SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED);
            } else {
                SetThreadExecutionState(ES_CONTINUOUS);
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = on;
    }
}

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
