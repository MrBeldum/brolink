//! The background control service: what a Mac on the tailnet talks to.
//!
//! One TCP listener on every interface, one rule for who gets an answer:
//! loopback (the host's own control panel) or a tailnet address that
//! `tailscale whois` attributes to the same account this PC is signed in
//! as. Everyone else gets a 403 and nothing more.

use crate::config::HostConfig;
use crate::streamer::{self, Api, Install};
use crate::wake::{self, WakeInfo};
use anyhow::Result;
use brolink_core::api::{Ack, PinRequest, PowerRequest, Status, Streamer};
use brolink_core::http::{self, Request, Response};
use brolink_core::{tailscale, CONTROL_PORT};
use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::sync::Arc;
use std::time::{Duration, Instant};

const LOG_LINES: usize = 80;
/// How long a `whois` answer is trusted before asking again.
const AUTH_TTL: Duration = Duration::from_secs(60);

pub struct Service {
    cfg: Mutex<HostConfig>,
    tailscale: Mutex<Result<tailscale::Status, String>>,
    wake: Mutex<WakeInfo>,
    install: Mutex<Option<Install>>,
    streamer: Mutex<Streamer>,
    log: Mutex<VecDeque<String>>,
    auth: Mutex<HashMap<IpAddr, (Instant, Option<u64>)>>,
}

impl Service {
    pub fn new() -> Self {
        Self {
            cfg: Mutex::new(HostConfig::load()),
            tailscale: Mutex::new(Err("not checked yet".into())),
            wake: Mutex::new(WakeInfo::default()),
            install: Mutex::new(None),
            streamer: Mutex::new(Streamer::default()),
            log: Mutex::new(VecDeque::new()),
            auth: Mutex::new(HashMap::new()),
        }
    }

    pub fn log(&self, msg: impl Into<String>) {
        let msg = msg.into();
        tracing::info!("{msg}");
        let mut log = self.log.lock();
        if log.len() >= LOG_LINES {
            log.pop_front();
        }
        log.push_back(msg);
    }

    /// Bind, then serve forever. Fails only when the port is taken, which
    /// means another copy is already running.
    pub fn run(self: Arc<Self>) -> Result<()> {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, CONTROL_PORT)))?;
        self.log(format!(
            "BroLink Host {} listening on TCP {CONTROL_PORT}",
            env!("CARGO_PKG_VERSION")
        ));
        let refresher = self.clone();
        std::thread::spawn(move || refresher.refresh_loop());
        let handler = self.clone();
        http::serve(listener, move |peer, req| handler.handle(peer, req));
        Ok(())
    }

    fn refresh_loop(&self) {
        let mut tick: u64 = 0;
        loop {
            self.refresh(tick);
            tick += 1;
            std::thread::sleep(Duration::from_secs(5));
        }
    }

    /// Cheap checks every tick; the slower probes on a longer cadence.
    fn refresh(&self, tick: u64) {
        *self.cfg.lock() = HostConfig::load();

        let ts = tailscale::status().map_err(|e| e.to_string());
        {
            let mut cur = self.tailscale.lock();
            match (&*cur, &ts) {
                (Ok(_), Err(e)) => self.log(format!("Tailscale: {e}")),
                (Err(_), Ok(s)) => self.log(format!(
                    "Tailscale up as {} ({})",
                    s.self_login().unwrap_or("?"),
                    s.self_node
                        .ipv4()
                        .map(|ip| ip.to_string())
                        .unwrap_or_default()
                )),
                _ => {}
            }
            *cur = ts;
        }

        let install = streamer::find();
        let running = install.is_some() && streamer::running();
        let cfg = self.cfg.lock().clone();
        let api_ok = if running
            && cfg.has_creds()
            && (tick.is_multiple_of(6) || !self.streamer.lock().api_ok)
        {
            Api {
                user: &cfg.sunshine_user,
                pass: &cfg.sunshine_pass,
            }
            .ok()
        } else {
            running && self.streamer.lock().api_ok
        };
        let st = Streamer {
            kind: install
                .as_ref()
                .map(|i| i.kind.to_string())
                .unwrap_or_default(),
            installed: install.is_some(),
            running,
            api_ok,
        };
        {
            let mut cur = self.streamer.lock();
            if *cur != st {
                self.log(match (&st.installed, &st.running, &st.api_ok) {
                    (false, _, _) => "Sunshine is not installed".to_string(),
                    (true, false, _) => format!("{} is installed but not running", st.kind),
                    (true, true, false) => {
                        format!("{} is running; BroLink cannot log in to it yet", st.kind)
                    }
                    (true, true, true) => {
                        format!("{} is running and BroLink is logged in", st.kind)
                    }
                });
                *cur = st;
            }
        }
        *self.install.lock() = install;

        if tick.is_multiple_of(12) {
            let w = wake::probe();
            let mut cur = self.wake.lock();
            if *cur != w {
                match (&w.mac, w.magic_packet) {
                    (Some(mac), Some(true)) => self.log(format!("Wake-on-LAN ready on {} ({mac})", w.adapter)),
                    (Some(_), Some(false)) => self.log(format!("Wake-on-LAN is off on {}; run setup", w.adapter)),
                    (Some(_), None) => self.log(format!("Wake-on-LAN state on {} is unknown", w.adapter)),
                    (None, _) => self.log("no wired adapter with a MAC found; the Mac will not be able to wake this PC"),
                }
                *cur = w;
            }
        }
    }

    pub fn status(&self, with_log: bool) -> Status {
        let cfg = self.cfg.lock().clone();
        let ts = self.tailscale.lock();
        let wake = self.wake.lock().clone();
        let streamer = self.streamer.lock().clone();
        let mut setup = Vec::new();
        if let Err(e) = &*ts {
            setup.push(format!("Tailscale: {e}. Install it and sign in."));
        }
        if !streamer.installed {
            setup.push("Sunshine is not installed.".into());
        } else if !streamer.running {
            setup.push(format!("{} is installed but not running.", streamer.kind));
        } else if !streamer.api_ok {
            setup.push(format!(
                "BroLink has no working login for {}.",
                streamer.kind
            ));
        }
        if wake.magic_packet == Some(false) {
            setup.push(format!("Wake-on-LAN is off on {}.", wake.adapter));
        }
        Status {
            app: "brolink".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            name: brolink_core::config::machine_name(),
            tailscale_ip: ts
                .as_ref()
                .ok()
                .and_then(|s| s.self_node.ipv4())
                .map(|ip| ip.to_string()),
            tailscale_login: ts
                .as_ref()
                .ok()
                .and_then(|s| s.self_login().map(str::to_string)),
            lan_ip: wake.lan_ip.map(|ip| ip.to_string()),
            mac: wake.mac.clone(),
            wake_ready: wake.magic_packet,
            wake_adapter: wake.adapter.clone(),
            wake_adapter_description: wake.description.clone(),
            streamer,
            power_allowed: cfg.power_allowed,
            setup,
            log: if with_log {
                self.log.lock().iter().cloned().collect()
            } else {
                Vec::new()
            },
        }
    }

    /// Loopback is the control panel; a tailnet peer must belong to the
    /// account this PC is signed in as.
    fn authorized(&self, ip: IpAddr) -> bool {
        if ip.is_loopback() {
            return true;
        }
        let IpAddr::V4(v4) = ip else { return false };
        if !is_tailnet(v4) {
            return false;
        }
        let me = match &*self.tailscale.lock() {
            Ok(s) => s.self_node.user_id,
            Err(_) => return false,
        };
        let now = Instant::now();
        let cached = self
            .auth
            .lock()
            .get(&ip)
            .filter(|(at, _)| now - *at < AUTH_TTL)
            .map(|(_, u)| *u);
        let user = match cached {
            Some(u) => u,
            None => {
                let u = match tailscale::whois(ip) {
                    Ok(w) => {
                        self.log(format!(
                            "{} ({}) asked{}",
                            w.node.computed_name,
                            w.user_profile.login_name,
                            if w.user_profile.id == me {
                                ""
                            } else {
                                ": not this account, refused"
                            }
                        ));
                        Some(w.user_profile.id)
                    }
                    Err(e) => {
                        self.log(format!("whois {ip}: {e}"));
                        None
                    }
                };
                self.auth.lock().insert(ip, (now, u));
                u
            }
        };
        user == Some(me)
    }

    fn handle(&self, peer: SocketAddr, req: &Request) -> Response {
        if !self.authorized(peer.ip()) {
            return Response::json(
                403,
                &Ack::err("not a machine on this PC's Tailscale account"),
            );
        }
        let local = peer.ip().is_loopback();
        match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/v1/status") => Response::json(200, &self.status(local)),
            ("POST", "/v1/pin") => self.pin(req),
            ("POST", "/v1/power") => self.power(req),
            ("POST", "/v1/quit") if local => {
                self.log("control panel asked the service to stop");
                std::thread::spawn(|| {
                    std::thread::sleep(Duration::from_millis(300));
                    std::process::exit(0);
                });
                Response::json(200, &Ack::ok())
            }
            _ => Response::json(404, &Ack::err("no such route")),
        }
    }

    fn pin(&self, req: &Request) -> Response {
        let Ok(p) = serde_json::from_str::<PinRequest>(&req.body) else {
            return Response::json(400, &Ack::err("expected {\"pin\",\"name\"}"));
        };
        if p.pin.len() != 4 || !p.pin.chars().all(|c| c.is_ascii_digit()) {
            return Response::json(400, &Ack::err("the PIN is four digits"));
        }
        let cfg = self.cfg.lock().clone();
        if !cfg.has_creds() || !self.streamer.lock().api_ok {
            return Response::json(
                502,
                &Ack::err("BroLink cannot log in to Sunshine on this PC; run setup there"),
            );
        }
        let api = Api {
            user: &cfg.sunshine_user,
            pass: &cfg.sunshine_pass,
        };
        match api.submit_pin(&p.pin, &p.name) {
            Ok(()) => {
                self.log(format!("paired \"{}\"", p.name));
                Response::json(200, &Ack::ok())
            }
            Err(e) => Response::json(502, &Ack::err(e.to_string())),
        }
    }

    fn power(&self, req: &Request) -> Response {
        let Ok(p) = serde_json::from_str::<PowerRequest>(&req.body) else {
            return Response::json(
                400,
                &Ack::err("expected {\"action\": sleep|restart|shutdown}"),
            );
        };
        let cfg = self.cfg.lock().clone();
        if !cfg.power_allowed {
            return Response::json(
                403,
                &Ack::err("the owner of this PC has turned remote power actions off"),
            );
        }
        if cfg.has_creds() {
            Api {
                user: &cfg.sunshine_user,
                pass: &cfg.sunshine_pass,
            }
            .close_app();
        }
        self.log(format!(
            "{} requested from the Mac",
            p.action.label().to_lowercase()
        ));
        // Reply first: sleep can suspend the machine before the bytes leave.
        let action = p.action;
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            if let Err(e) = crate::power::perform(action) {
                tracing::error!("{}: {e:#}", action.label());
            }
        });
        Response::json(200, &Ack::ok())
    }
}

impl Default for Service {
    fn default() -> Self {
        Self::new()
    }
}

/// Tailscale hands out addresses from 100.64.0.0/10.
fn is_tailnet(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 100 && (64..128).contains(&o[1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tailnet_range_is_recognised() {
        assert!(is_tailnet("100.64.0.1".parse().unwrap()));
        assert!(is_tailnet("100.127.255.254".parse().unwrap()));
        assert!(is_tailnet("100.64.0.10".parse().unwrap()));
        assert!(!is_tailnet("100.128.0.1".parse().unwrap()));
        assert!(!is_tailnet("192.168.1.2".parse().unwrap()));
    }

    #[test]
    fn strangers_get_403_and_loopback_gets_status() {
        let svc = Service::new();
        let req = Request {
            method: "GET".into(),
            path: "/v1/status".into(),
            body: String::new(),
        };
        let r = svc.handle("192.168.1.11:5".parse().unwrap(), &req);
        assert_eq!(r.status, 403);
        // A tailnet address with Tailscale down is refused too: nobody can vouch for it.
        let r = svc.handle("100.64.0.30:5".parse().unwrap(), &req);
        assert_eq!(r.status, 403);
        let r = svc.handle("127.0.0.1:5".parse().unwrap(), &req);
        assert_eq!(r.status, 200);
        let st: Status = r.parse().unwrap();
        assert_eq!(st.app, "brolink");
        assert!(!st.name.is_empty());
    }

    #[test]
    fn pin_and_power_validate_their_bodies() {
        let svc = Service::new();
        let local: SocketAddr = "127.0.0.1:5".parse().unwrap();
        let post = |path: &str, body: &str| Request {
            method: "POST".into(),
            path: path.into(),
            body: body.into(),
        };
        assert_eq!(svc.handle(local, &post("/v1/pin", "{}")).status, 400);
        assert_eq!(
            svc.handle(local, &post("/v1/pin", r#"{"pin":"12","name":"m"}"#))
                .status,
            400
        );
        // Valid shape, but no Sunshine login on this box: a 502, not a crash.
        assert_eq!(
            svc.handle(local, &post("/v1/pin", r#"{"pin":"1234","name":"m"}"#))
                .status,
            502
        );
        assert_eq!(
            svc.handle(local, &post("/v1/power", r#"{"action":"nap"}"#))
                .status,
            400
        );
        assert_eq!(svc.handle(local, &post("/v1/nope", "")).status, 404);
    }

    #[test]
    fn power_is_refused_when_the_owner_says_so() {
        let svc = Service::new();
        svc.cfg.lock().power_allowed = false;
        let r = svc.handle(
            "127.0.0.1:5".parse().unwrap(),
            &Request {
                method: "POST".into(),
                path: "/v1/power".into(),
                body: r#"{"action":"sleep"}"#.into(),
            },
        );
        assert_eq!(r.status, 403);
    }
}
