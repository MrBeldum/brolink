//! This machine's sharing side, filled by the unified app from the local
//! control service. The lobby shows it so every BroLink install is both a
//! viewer and a host.

use brolink_core::api::Status;
use parking_lot::Mutex;
use std::sync::Arc;

#[derive(Clone, Default)]
pub struct LocalShare {
    pub status: Option<Status>,
    pub setup_running: bool,
    pub setup_result: Option<Result<(), String>>,
    /// The window asked to run setup; the unified app clears this after it
    /// starts the platform setup.
    pub want_setup: bool,
    pub power_allowed: bool,
    pub stay_awake: bool,
    pub autostart: bool,
    pub want_power: Option<bool>,
    pub want_stay_awake: Option<bool>,
    pub want_autostart: Option<bool>,
}

pub type Slot = Arc<Mutex<LocalShare>>;
