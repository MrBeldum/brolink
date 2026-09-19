//! The one BroLink window: a machine list to connect from, and sharing of
//! this machine so others can connect in.

use crate::app::HostApp;
use brolink_client::app::ClientApp;
use brolink_client::share::{LocalShare, Slot};
use eframe::egui;
use parking_lot::Mutex;
use std::sync::Arc;

pub struct NodeApp {
    client: ClientApp,
    host: HostApp,
    local: Slot,
}

impl NodeApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let local: Slot = Arc::new(Mutex::new(LocalShare::default()));
        let client = ClientApp::new(cc).with_local(local.clone());
        Self {
            client,
            host: HostApp::new(cc),
            local,
        }
    }
}

impl eframe::App for NodeApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        {
            let shared = self.host.shared();
            let mut g = self.local.lock();
            g.status = shared.status.clone();
            g.setup_running = shared.setup_running;
            g.setup_result = shared.setup_result.clone();
            let want = g.want_setup;
            g.want_setup = false;
            let want_power = g.want_power.take();
            let want_stay = g.want_stay_awake.take();
            let want_auto = g.want_autostart.take();
            drop(g);
            drop(shared);
            if want {
                self.host.request_setup();
            }
            self.host
                .apply_share_toggles(want_power, want_stay, want_auto);
            self.host.publish_share(&self.local);
        }
        self.client.update(ctx, frame);
    }
}
