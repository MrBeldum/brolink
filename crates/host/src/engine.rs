//! Host engine: UDP server, pairing, capture/encode, input.

use crate::audio::{AudioCapture, AudioPacket};
use crate::clipboard::ClipboardBridge;
use crate::encode::{find_ffmpeg, select_encoder, EncoderInfo, VideoPipeline};
use crate::ffmpeg_setup;
use crate::input::InputInjector;
use anyhow::{anyhow, Result};
use brolink_core::abr::{AbrController, COOLDOWN_SECS};
use brolink_core::codec::{fragment_frame, keyframe_flag};
use brolink_core::config::{global_v6, primary_lan_v4, tailscale_v4, HostConfig, StreamQuality};
use brolink_core::crypto::{
    constant_eq, random_bytes, random_pin, sign_handshake, EphKey, ReplayWindow, SessionKeys,
};
use brolink_core::discovery::{announce, Beacon};
use brolink_core::identity::{AllowList, Identity};
use brolink_core::net::{bind_udp, RelayLink, Transport, RECV_BUF};
use brolink_core::proto::*;
use brolink_core::stun::discover_wan;
use brolink_core::ticket::{Candidate, CandidateKind, RelayHint, Ticket};
use brolink_core::upnp::{map_udp_port, PortMapping};
use bytes::BytesMut;
use parking_lot::Mutex;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A client that stops sending anything for this long is gone.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(8);
/// How long a client has to type the PIN before the host gives up on it.
const PAIRING_TIMEOUT: Duration = Duration::from_secs(90);
/// Re-check the public address this often while idle. Never while streaming:
/// an active session keeps the NAT mapping open on its own.
const STUN_REFRESH: Duration = Duration::from_secs(20);
const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(1);
const RELAY_KEEPALIVE: Duration = Duration::from_secs(10);
const UPNP_REFRESH: Duration = Duration::from_secs(15 * 60);
const CLIPBOARD_POLL: Duration = Duration::from_millis(400);
const HELLO_RATE: Duration = Duration::from_secs(2);
/// Cap the fragments pushed per pump so input stays responsive under a burst
/// of large keyframes.
const MAX_FRAGMENTS_PER_PUMP: usize = 256;
const LOG_LINES: usize = 200;

#[derive(Debug, Clone)]
pub struct HostStatus {
    pub running: bool,
    pub encoder: String,
    pub lan: Option<SocketAddr>,
    pub wan: Option<SocketAddr>,
    pub tailscale: Option<SocketAddr>,
    pub relay: Option<SocketAddr>,
    pub ticket: String,
    pub ticket_display: String,
    pub pending_pin: Option<String>,
    pub pending_name: Option<String>,
    pub client: Option<String>,
    pub streaming: bool,
    pub fps: f32,
    pub bitrate_kbps: f32,
    pub frames_sent: u64,
    pub last_error: Option<String>,
    pub log: Vec<String>,
    pub upnp: Option<String>,
    pub internet: String,
    pub ffmpeg_ok: bool,
    pub paired: Vec<(String, String)>,
    pub path: String,
}

impl Default for HostStatus {
    fn default() -> Self {
        Self {
            running: false,
            encoder: "—".into(),
            lan: None,
            wan: None,
            tailscale: None,
            relay: None,
            ticket: String::new(),
            ticket_display: String::new(),
            pending_pin: None,
            pending_name: None,
            client: None,
            streaming: false,
            fps: 0.0,
            bitrate_kbps: 0.0,
            frames_sent: 0,
            last_error: None,
            log: Vec::new(),
            upnp: None,
            internet: "checking…".into(),
            ffmpeg_ok: false,
            paired: Vec::new(),
            path: String::new(),
        }
    }
}

impl HostStatus {
    fn push_log(&mut self, line: impl Into<String>) {
        let line = line.into();
        tracing::info!("{line}");
        self.log.push(line);
        if self.log.len() > LOG_LINES {
            let extra = self.log.len() - LOG_LINES;
            self.log.drain(..extra);
        }
    }

    fn end_session(&mut self, reason: &str) {
        self.streaming = false;
        self.client = None;
        self.pending_pin = None;
        self.pending_name = None;
        self.fps = 0.0;
        self.bitrate_kbps = 0.0;
        self.push_log(reason.to_string());
    }
}

pub struct Engine {
    pub status: Arc<Mutex<HostStatus>>,
    stop: Arc<AtomicBool>,
    kick: Arc<AtomicBool>,
    cfg: Arc<Mutex<HostConfig>>,
    identity: Identity,
    /// Probing every encoder costs seconds; a reconnect with unchanged settings
    /// should not pay it again.
    encoder_cache: Arc<Mutex<Option<(EncoderKey, EncoderInfo)>>>,
    revoke: Arc<Mutex<Vec<String>>>,
}

/// Everything that would invalidate a cached encoder choice.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EncoderKey {
    width: u32,
    height: u32,
    fps: u32,
    bitrate_kbps: u32,
    monitor: u32,
    prefer: String,
}

impl Engine {
    pub fn new(cfg: HostConfig, identity: Identity) -> Self {
        Self {
            status: Arc::new(Mutex::new(HostStatus::default())),
            stop: Arc::new(AtomicBool::new(false)),
            kick: Arc::new(AtomicBool::new(false)),
            cfg: Arc::new(Mutex::new(cfg)),
            identity,
            encoder_cache: Arc::new(Mutex::new(None)),
            revoke: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn update_config(&self, cfg: HostConfig) {
        *self.cfg.lock() = cfg;
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        self.kick.store(true, Ordering::Relaxed);
    }

    pub fn kick_client(&self) {
        self.kick.store(true, Ordering::Relaxed);
    }

    pub fn revoke_client(&self, hex: String) {
        self.revoke.lock().push(hex);
    }

    pub async fn run(&self) -> Result<()> {
        let cfg = self.cfg.lock().clone();
        let bind: SocketAddr = format!("{}:{}", cfg.bind, cfg.port)
            .parse()
            .map_err(|e| anyhow!("invalid bind address '{}:{}': {e}", cfg.bind, cfg.port))?;
        let sock = bind_udp(bind)?;
        let local = sock.local_addr()?;

        let relay = resolve_relay(&cfg.relay);
        if !cfg.relay.is_empty() && relay.is_none() {
            self.status.lock().push_log(format!(
                "Relay '{}' could not be resolved — ignoring",
                cfg.relay
            ));
        }
        let transport = Transport::new(sock, relay);
        {
            let mut st = self.status.lock();
            st.running = true;
            st.relay = relay.map(|r| r.addr);
            st.push_log(format!("Listening on {local}"));
            if let Some(r) = relay {
                st.push_log(format!("Relay: {}", r.addr));
            }
        }

        // Learn our public address before advertising a ticket.
        let mut wan = discover_wan(transport.socket()).await;
        self.publish_ticket(local.port(), wan, relay);

        if cfg.enable_upnp {
            if let Some(mapped) = map_udp_port(local.port(), 3600).await {
                wan = Some(merge_upnp_wan(wan, &mapped));
                {
                    let mut st = self.status.lock();
                    st.upnp = Some(format!("{} via {}", mapped.external, mapped.via.as_str()));
                    st.push_log(format!(
                        "Internet: mapped {} ({})",
                        mapped.external,
                        mapped.via.as_str()
                    ));
                }
                self.publish_ticket(local.port(), wan, relay);
            } else {
                self.status.lock().push_log(
                    "Router did not map the port (UPnP/NAT-PMP). Worldwide access needs Tailscale, a relay, or a manual forward.",
                );
            }
        }

        let ffmpeg = find_ffmpeg(&cfg.ffmpeg_path).or_else(ffmpeg_setup::bundled_ffmpeg);
        match ffmpeg.as_ref() {
            Some(ff) => {
                {
                    let mut st = self.status.lock();
                    st.ffmpeg_ok = true;
                    st.push_log(format!("FFmpeg: {}", ff.display()));
                }
                // Probe while idle so the first Mac is not stuck behind 12s
                // timeouts on every encoder that is not installed.
                let q = cfg.quality.clone();
                let monitor = cfg.monitor_index;
                let prefer = cfg.encoder.clone();
                let ffmpeg = ff.clone();
                let cache = self.encoder_cache.clone();
                let status = self.status.clone();
                tokio::task::spawn_blocking(move || {
                    match select_encoder(&ffmpeg, &q, monitor, &prefer) {
                        Ok(info) => {
                            let key = EncoderKey {
                                width: q.width,
                                height: q.height,
                                fps: q.fps,
                                bitrate_kbps: q.bitrate_kbps,
                                monitor,
                                prefer,
                            };
                            status
                                .lock()
                                .push_log(format!("Encoder ready: {}", info.name));
                            *cache.lock() = Some((key, info));
                        }
                        Err(e) => status.lock().push_log(format!("Encoder probe: {e:#}")),
                    }
                });
            }
            None => {
                self.status.lock().push_log(
                    "FFmpeg not found — downloading a local copy into %LOCALAPPDATA%\\BroLink…",
                );
                let status = self.status.clone();
                let cfg_slot = self.cfg.clone();
                tokio::spawn(async move {
                    let bytes = Arc::new(std::sync::atomic::AtomicU64::new(0));
                    let total = Arc::new(std::sync::atomic::AtomicU64::new(0));
                    match tokio::task::spawn_blocking(move || {
                        ffmpeg_setup::download_ffmpeg(&bytes, &total)
                    })
                    .await
                    {
                        Ok(Ok(p)) => {
                            let mut cfg_now = cfg_slot.lock().clone();
                            cfg_now.ffmpeg_path = p.display().to_string();
                            let _ = cfg_now.save();
                            *cfg_slot.lock() = cfg_now;
                            let mut st = status.lock();
                            st.ffmpeg_ok = true;
                            st.push_log(format!("FFmpeg ready: {}", p.display()));
                        }
                        Ok(Err(e)) => status.lock().push_log(format!(
                            "Could not download FFmpeg ({e:#}). Place ffmpeg.exe next to brolink-host.exe."
                        )),
                        Err(e) => status
                            .lock()
                            .push_log(format!("FFmpeg download task failed: {e}")),
                    }
                });
            }
        }
        self.refresh_internet_label(wan, relay);

        let allowlist_path = brolink_core::config::allowlist_path()?;
        let mut allow = AllowList::load(&allowlist_path).unwrap_or_default();
        let mut buf = vec![0u8; RECV_BUF];
        let mut session: Option<LiveSession> = None;
        let mut last_announce = Instant::now() - ANNOUNCE_INTERVAL;
        let mut last_stun = Instant::now();
        let mut last_upnp = Instant::now();
        let mut last_relay_ka = Instant::now() - RELAY_KEEPALIVE;
        let mut last_hello_from: Option<(SocketAddr, Instant)> = None;
        let mut seq_out = SeqCounter::new();

        while !self.stop.load(Ordering::Relaxed) {
            {
                let hexes: Vec<String> = self.revoke.lock().drain(..).collect();
                for hex in hexes {
                    if allow.remove_hex(&hex) {
                        let _ = allow.save(&allowlist_path);
                        self.status
                            .lock()
                            .push_log(format!("Revoked paired client {hex}"));
                    }
                }
                let mut st = self.status.lock();
                st.paired = allow
                    .clients
                    .iter()
                    .map(|c| (c.name.clone(), c.public.clone()))
                    .collect();
            }

            if last_announce.elapsed() >= ANNOUNCE_INTERVAL {
                last_announce = Instant::now();
                let (name, ticket, encoder) = {
                    let st = self.status.lock();
                    (
                        self.cfg.lock().name.clone(),
                        st.ticket.clone(),
                        st.encoder.clone(),
                    )
                };
                let beacon = Beacon {
                    name,
                    host_id: self.identity.public_hex(),
                    port: local.port(),
                    version: env!("CARGO_PKG_VERSION").into(),
                    encoder,
                    ticket,
                };
                let _ = announce(transport.socket(), &beacon).await;
            }

            // Hold the relay registration open even with nobody connected.
            if relay.is_some() && last_relay_ka.elapsed() >= RELAY_KEEPALIVE {
                last_relay_ka = Instant::now();
                let _ = transport.relay_keepalive().await;
            }

            // Refresh the public address only while idle. Running STUN on the
            // shared socket mid-session would swallow the client's input and
            // stall video for as long as the STUN exchange takes.
            if session.is_none() && last_stun.elapsed() >= STUN_REFRESH {
                last_stun = Instant::now();
                let fresh = discover_wan(transport.socket()).await;
                if fresh != self.status.lock().wan && self.status.lock().upnp.is_none() {
                    wan = fresh;
                    self.publish_ticket(local.port(), wan, relay);
                    self.refresh_internet_label(wan, relay);
                }
            }
            if session.is_none()
                && self.cfg.lock().enable_upnp
                && last_upnp.elapsed() >= UPNP_REFRESH
            {
                last_upnp = Instant::now();
                if let Some(mapped) = map_udp_port(local.port(), 3600).await {
                    wan = Some(merge_upnp_wan(wan, &mapped));
                    self.status.lock().upnp =
                        Some(format!("{} via {}", mapped.external, mapped.via.as_str()));
                    self.publish_ticket(local.port(), wan, relay);
                    self.refresh_internet_label(wan, relay);
                }
            }

            if self.kick.swap(false, Ordering::Relaxed) && session.take().is_some() {
                self.status
                    .lock()
                    .end_session("Disconnected from the host UI");
            }

            if let Some(sess) = session.as_mut() {
                if let Err(e) = sess.pump(&transport, &mut seq_out).await {
                    self.status
                        .lock()
                        .end_session(&format!("Stream ended: {e:#}"));
                    session = None;
                }
            }

            let recv =
                tokio::time::timeout(Duration::from_millis(2), transport.recv_from(&mut buf)).await;
            let Ok(Ok((n, from))) = recv else {
                continue;
            };
            let Ok(header) = parse_header(&buf[..n]) else {
                continue;
            };
            match header.typ {
                PacketType::Discovery => {}
                PacketType::Hello => {
                    if session.is_some() {
                        continue;
                    }
                    if last_hello_from.is_some_and(|(a, t)| a == from && t.elapsed() < HELLO_RATE) {
                        continue;
                    }
                    last_hello_from = Some((from, Instant::now()));
                    match self
                        .handle_hello(
                            &transport,
                            from,
                            &buf[..n],
                            &mut allow,
                            &mut seq_out,
                            &allowlist_path,
                        )
                        .await
                    {
                        Ok(s) => {
                            let mut st = self.status.lock();
                            st.client = Some(s.client_name.clone());
                            st.pending_pin = None;
                            st.pending_name = None;
                            st.streaming = true;
                            st.frames_sent = 0;
                            st.push_log(format!("Paired with {} ({from})", s.client_name));
                            drop(st);
                            session = Some(s);
                        }
                        Err(e) => {
                            let mut st = self.status.lock();
                            st.pending_pin = None;
                            st.pending_name = None;
                            st.push_log(format!("Handshake from {from} failed: {e:#}"));
                        }
                    }
                }
                _ => {
                    if let Some(sess) = session.as_mut() {
                        if !transport.accepts_from(sess.peer, from) {
                            continue;
                        }
                        match sess.on_packet(&transport, &buf[..n], &mut seq_out).await {
                            Ok(true) => {}
                            Ok(false) => {
                                self.status.lock().end_session("Client disconnected");
                                session = None;
                            }
                            // A packet that fails to decrypt is a stray or
                            // corrupt datagram, not a reason to drop a working
                            // session — over the internet those are routine.
                            Err(e) => tracing::debug!("ignored packet from {from}: {e:#}"),
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Rebuild and publish the ticket from whatever addresses we currently know.
    fn publish_ticket(&self, port: u16, wan: Option<SocketAddr>, relay: Option<RelayLink>) {
        let name = self.cfg.lock().name.clone();
        let mut candidates = Vec::new();
        if let Some(ip) = primary_lan_v4() {
            candidates.push(Candidate {
                kind: CandidateKind::Lan,
                addr: SocketAddr::V4(SocketAddrV4::new(ip, port)),
            });
        }
        if let Some(w) = wan {
            candidates.push(Candidate {
                kind: CandidateKind::Wan,
                addr: w,
            });
        }
        let ts = tailscale_v4().map(|ip| SocketAddr::V4(SocketAddrV4::new(ip, port)));
        if let Some(addr) = ts {
            candidates.push(Candidate {
                kind: CandidateKind::Tailscale,
                addr,
            });
        }
        for ip in global_v6() {
            candidates.push(Candidate {
                kind: CandidateKind::Wan,
                addr: SocketAddr::from((ip, port)),
            });
        }
        let relay_hint = relay.map(|r| RelayHint {
            addr: r.addr,
            token: r.token,
        });
        let ticket = Ticket::new(&self.identity, candidates, relay_hint, &name);

        let mut st = self.status.lock();
        let first_publish = st.ticket.is_empty();
        st.lan = ticket.lan().map(SocketAddr::V4);
        st.wan = wan;
        st.tailscale = ts;
        st.ticket = ticket.encode();
        st.ticket_display = ticket.display_code();
        if first_publish {
            match wan {
                Some(w) => st.push_log(format!("Public address: {w}")),
                None => st.push_log(
                    "No public address yet — WAN needs UPnP, Tailscale, a relay, or a port-forward",
                ),
            }
            if let Some(t) = ts {
                st.push_log(format!("Tailscale: {t}"));
            }
        } else {
            st.push_log("Public address changed — ticket refreshed");
        }
    }

    fn refresh_internet_label(&self, wan: Option<SocketAddr>, relay: Option<RelayLink>) {
        let mut st = self.status.lock();
        st.internet = if st.upnp.is_some() {
            "Reachable from the internet (router port mapping)".into()
        } else if st.tailscale.is_some() {
            "Reachable over Tailscale".into()
        } else if relay.is_some() {
            "Reachable through your relay".into()
        } else if wan.is_some() {
            "Public address known — may still need UPnP, Tailscale, or a relay on many home routers"
                .into()
        } else {
            "Local network only. Enable UPnP, add a relay, or install Tailscale for worldwide access.".into()
        };
    }

    /// Pick an encoder, reusing the previous choice when nothing relevant changed.
    async fn resolve_encoder(
        &self,
        ffmpeg: std::path::PathBuf,
        quality: &StreamQuality,
        monitor: u32,
        prefer: &str,
    ) -> Result<EncoderInfo> {
        let key = EncoderKey {
            width: quality.width,
            height: quality.height,
            fps: quality.fps,
            bitrate_kbps: quality.bitrate_kbps,
            monitor,
            prefer: prefer.to_string(),
        };
        if let Some((cached_key, info)) = self.encoder_cache.lock().as_ref() {
            if *cached_key == key {
                return Ok(info.clone());
            }
        }
        self.status.lock().push_log("Probing encoders…");
        let q = quality.clone();
        let prefer = prefer.to_string();
        // Probing spawns ffmpeg and waits: keep it off the async runtime.
        let info =
            tokio::task::spawn_blocking(move || select_encoder(&ffmpeg, &q, monitor, &prefer))
                .await
                .map_err(|e| anyhow!("encoder probe panicked: {e}"))??;
        *self.encoder_cache.lock() = Some((key, info.clone()));
        Ok(info)
    }

    async fn handle_hello(
        &self,
        transport: &Transport,
        from: SocketAddr,
        pkt: &[u8],
        allow: &mut AllowList,
        seq_out: &mut SeqCounter,
        allowlist_path: &std::path::Path,
    ) -> Result<LiveSession> {
        let hello: HelloMsg = json_from_slice(&pkt[HEADER_LEN..])?;
        let cfg = self.cfg.lock().clone();
        let eph = EphKey::generate();
        let server_nonce = random_bytes::<16>();
        let session_id = random_bytes::<16>();
        let known = allow.contains(&hello.client_id);
        if !known && !cfg.allow_unpaired_with_pin && !cfg.auto_trust {
            anyhow::bail!("client is not on the allow-list");
        }
        let needs_pin = !known && !cfg.auto_trust;
        if !known && cfg.auto_trust {
            allow.add(&hello.client_id, &hello.name);
            let _ = allow.save(allowlist_path);
        }
        let pin = needs_pin.then(random_pin);
        if let Some(p) = pin.as_ref() {
            let mut st = self.status.lock();
            st.pending_pin = Some(p.clone());
            st.pending_name = Some(hello.name.clone());
            st.push_log(format!("PIN for '{}': {p}", hello.name));
        }

        let sig = sign_handshake(
            &self.identity.signing,
            &hello.client_eph,
            &eph.public,
            &hello.nonce,
            &server_nonce,
        );
        let ack = HelloAck {
            server_id: self.identity.public,
            server_eph: eph.public,
            nonce: server_nonce,
            session_id,
            needs_pin,
            host_name: cfg.name.clone(),
            signature: sig,
        };
        let payload = json_payload(&ack)?;
        let ack_pkt = encode_plain(PacketType::HelloAck, seq_out.next_seq()?, &payload);
        transport.send_to(&ack_pkt, from).await?;
        if let Some(wan) = hello
            .client_wan
            .as_deref()
            .and_then(|s| s.parse::<SocketAddr>().ok())
        {
            if wan != from {
                let _ = transport.send_to(&ack_pkt, wan).await;
            }
        }

        let shared = eph.shared(&hello.client_eph);
        let keys = SessionKeys::derive(&shared, &hello.nonce, &server_nonce, true)?;

        if let Some(expected) = pin {
            self.await_pin(transport, from, &keys, &expected, session_id, seq_out)
                .await?;
            allow.add(&hello.client_id, &hello.name);
            if let Err(e) = allow.save(allowlist_path) {
                self.status
                    .lock()
                    .push_log(format!("Could not save the allow-list: {e:#}"));
            }
        }

        let res = PairResult {
            ok: true,
            message: "paired".into(),
        };
        send_sealed(
            transport,
            &keys,
            seq_out,
            PacketType::PairResult,
            0,
            &json_payload(&res)?,
            from,
        )
        .await?;

        // The client may ask for a different resolution/rate than our default;
        // honour it within limits it can actually decode.
        let mut quality = cfg.quality.clone();
        if let (Some(w), Some(h)) = (hello.width, hello.height) {
            quality.width = w;
            quality.height = h;
        }
        if let Some(f) = hello.fps {
            quality.fps = f;
        }
        if let Some(b) = hello.bitrate_kbps {
            quality.bitrate_kbps = b;
        }
        let quality = quality.sanitized();

        let ffmpeg = find_ffmpeg(&cfg.ffmpeg_path)
            .or_else(ffmpeg_setup::bundled_ffmpeg)
            .ok_or_else(|| {
                anyhow!("FFmpeg not found — the host is still installing it, or place ffmpeg.exe next to brolink-host.exe")
            })?;
        let enc = self
            .resolve_encoder(ffmpeg, &quality, cfg.monitor_index, &cfg.encoder)
            .await?;
        {
            let mut st = self.status.lock();
            st.encoder = enc.name.clone();
            st.push_log(format!(
                "Encoder: {} at {}x{} {} fps, {} kbps",
                enc.name, quality.width, quality.height, quality.fps, quality.bitrate_kbps
            ));
        }

        let ready = SessionReady {
            width: quality.width,
            height: quality.height,
            fps: quality.fps,
            bitrate_kbps: quality.bitrate_kbps,
            codec: "h264".into(),
            encoder: enc.name.clone(),
            audio: AudioFormat::PcmS16Le48kStereo,
            monitor_name: format!("Display {}", cfg.monitor_index),
        };
        send_sealed(
            transport,
            &keys,
            seq_out,
            PacketType::SessionReady,
            0,
            &json_payload(&ready)?,
            from,
        )
        .await?;

        LiveSession::start(
            from,
            hello.name,
            keys,
            enc,
            quality.bitrate_kbps,
            cfg.enable_audio,
            cfg.enable_gamepad,
            cfg.enable_clipboard,
            cfg.adaptive_bitrate,
            self.status.clone(),
        )
    }

    /// Block until the client sends the right PIN, or we give up on it.
    async fn await_pin(
        &self,
        transport: &Transport,
        from: SocketAddr,
        keys: &SessionKeys,
        expected: &str,
        session_id: [u8; 16],
        seq_out: &mut SeqCounter,
    ) -> Result<()> {
        let deadline = Instant::now() + PAIRING_TIMEOUT;
        let mut buf = vec![0u8; RECV_BUF];
        let mut scratch = Vec::new();
        let mut attempts = 0u32;
        while Instant::now() < deadline {
            if self.stop.load(Ordering::Relaxed) {
                anyhow::bail!("shutting down");
            }
            let n = match tokio::time::timeout(
                Duration::from_millis(200),
                transport.recv_from(&mut buf),
            )
            .await
            {
                Ok(Ok((n, src))) if transport.accepts_from(from, src) => n,
                _ => continue,
            };
            // Junk on the socket during pairing is expected (a retried Hello, a
            // late STUN reply); skip it rather than abandoning the pairing.
            let Ok(header) = parse_header(&buf[..n]) else {
                continue;
            };
            if header.typ != PacketType::PairPin {
                continue;
            }
            let Ok((_h, pt)) = open(keys, &buf[..n], &mut scratch) else {
                continue;
            };
            let Ok(pin_msg) = json_from_slice::<PairPin>(pt) else {
                continue;
            };
            if pin_msg.session_id != session_id {
                continue;
            }
            if constant_eq(&pin_msg.pin, expected) {
                return Ok(());
            }
            attempts += 1;
            let remaining = 3i32 - attempts as i32;
            let res = PairResult {
                ok: remaining > 0,
                message: if remaining > 0 {
                    format!("Incorrect PIN — {remaining} attempt(s) left")
                } else {
                    "Incorrect PIN".into()
                },
            };
            send_sealed(
                transport,
                keys,
                seq_out,
                PacketType::PairResult,
                0,
                &json_payload(&res)?,
                from,
            )
            .await?;
            self.status
                .lock()
                .push_log(format!("Incorrect PIN from {from} (attempt {attempts})"));
            if attempts >= 3 {
                anyhow::bail!("too many incorrect PINs");
            }
        }
        anyhow::bail!("pairing timed out")
    }
}

fn merge_upnp_wan(stun: Option<SocketAddr>, mapped: &PortMapping) -> SocketAddr {
    if !mapped.external.ip().is_unspecified() {
        return SocketAddr::V4(mapped.external);
    }
    match stun {
        Some(s) => SocketAddr::new(s.ip(), mapped.external.port()),
        None => SocketAddr::V4(mapped.external),
    }
}

/// Resolve the configured relay to an address plus a fresh pairing token.
fn resolve_relay(spec: &str) -> Option<RelayLink> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }
    let addr = spec
        .parse::<SocketAddr>()
        .ok()
        .or_else(|| {
            // Bare host or host:port — resolve it once at startup.
            let with_port = if spec.contains(':') {
                spec.to_string()
            } else {
                format!("{spec}:47851")
            };
            std::net::ToSocketAddrs::to_socket_addrs(&with_port)
                .ok()?
                .find(|a| a.is_ipv4())
        })
        .or_else(|| {
            spec.parse::<Ipv4Addr>()
                .ok()
                .map(|ip| SocketAddr::new(ip.into(), 47851))
        })?;
    Some(RelayLink {
        addr,
        token: random_bytes::<16>(),
    })
}

async fn send_sealed(
    transport: &Transport,
    keys: &SessionKeys,
    seq: &mut SeqCounter,
    typ: PacketType,
    flags: u8,
    payload: &[u8],
    to: SocketAddr,
) -> Result<()> {
    let mut out = BytesMut::with_capacity(MAX_DATAGRAM);
    seal(keys, typ, seq.next_seq()?, flags, payload, &mut out)?;
    transport.send_to(&out, to).await?;
    Ok(())
}

struct LiveSession {
    peer: SocketAddr,
    client_name: String,
    keys: SessionKeys,
    video_rx: crossbeam_channel::Receiver<brolink_core::codec::EncodedFrame>,
    audio_rx: Option<crossbeam_channel::Receiver<AudioPacket>>,
    _video: VideoPipeline,
    _audio: Option<AudioCapture>,
    enc: EncoderInfo,
    input: InputInjector,
    replay: ReplayWindow,
    status: Arc<Mutex<HostStatus>>,
    frame_id: u32,
    frames_sent: u64,
    bytes_window: u64,
    frames_window: u64,
    window_start: Instant,
    last_client: Instant,
    scratch: Vec<u8>,
    clipboard: Option<ClipboardBridge>,
    last_clip: Instant,
    abr: Option<AbrController>,
    last_abr: Instant,
}

impl LiveSession {
    #[allow(clippy::too_many_arguments)]
    fn start(
        peer: SocketAddr,
        client_name: String,
        keys: SessionKeys,
        enc: EncoderInfo,
        bitrate_kbps: u32,
        enable_audio: bool,
        enable_gamepad: bool,
        enable_clipboard: bool,
        adaptive: bool,
        status: Arc<Mutex<HostStatus>>,
    ) -> Result<Self> {
        let (vtx, vrx) = crossbeam_channel::bounded(8);
        let video = VideoPipeline::start(enc.clone(), vtx)?;
        let (audio, audio_rx) = if enable_audio {
            let (atx, arx) = crossbeam_channel::bounded(64);
            match AudioCapture::start(atx) {
                Ok(a) => (Some(a), Some(arx)),
                Err(e) => {
                    status
                        .lock()
                        .push_log(format!("System audio unavailable: {e:#}"));
                    (None, None)
                }
            }
        } else {
            (None, None)
        };
        {
            let mut st = status.lock();
            st.path = classify_peer(peer);
        }
        Ok(Self {
            peer,
            client_name,
            keys,
            video_rx: vrx,
            audio_rx,
            _video: video,
            _audio: audio,
            enc,
            input: InputInjector::new(enable_gamepad),
            replay: ReplayWindow::default(),
            status,
            frame_id: 0,
            frames_sent: 0,
            bytes_window: 0,
            frames_window: 0,
            window_start: Instant::now(),
            last_client: Instant::now(),
            scratch: Vec::new(),
            clipboard: enable_clipboard.then(ClipboardBridge::new),
            last_clip: Instant::now(),
            abr: adaptive.then(|| AbrController::new(bitrate_kbps, bitrate_kbps)),
            last_abr: Instant::now() - Duration::from_secs(COOLDOWN_SECS),
        })
    }

    async fn pump(&mut self, transport: &Transport, seq: &mut SeqCounter) -> Result<()> {
        if self.last_client.elapsed() > CLIENT_TIMEOUT {
            anyhow::bail!("client timed out");
        }
        let mut sent = 0usize;
        while let Ok(frame) = self.video_rx.try_recv() {
            self.frame_id = self.frame_id.wrapping_add(1);
            let flags = keyframe_flag(frame.keyframe);
            for frag in fragment_frame(self.frame_id, frame.timestamp_us, &frame.data) {
                send_sealed(
                    transport,
                    &self.keys,
                    seq,
                    PacketType::Video,
                    flags,
                    &frag,
                    self.peer,
                )
                .await?;
                self.bytes_window += frag.len() as u64;
                sent += 1;
            }
            self.frames_window += 1;
            self.frames_sent += 1;
            if sent >= MAX_FRAGMENTS_PER_PUMP {
                break;
            }
        }
        if let Some(rx) = self.audio_rx.as_ref() {
            while let Ok(pkt) = rx.try_recv() {
                let payload = write_audio_payload(pkt.timestamp_us, &pkt.pcm);
                send_sealed(
                    transport,
                    &self.keys,
                    seq,
                    PacketType::Audio,
                    0,
                    &payload,
                    self.peer,
                )
                .await?;
                self.bytes_window += payload.len() as u64;
            }
        }
        if self.window_start.elapsed() >= Duration::from_secs(1) {
            let dt = self.window_start.elapsed().as_secs_f32().max(0.001);
            let mut st = self.status.lock();
            st.fps = self.frames_window as f32 / dt;
            st.bitrate_kbps = (self.bytes_window as f32 * 8.0 / dt) / 1000.0;
            st.frames_sent = self.frames_sent;
            drop(st);
            self.frames_window = 0;
            self.bytes_window = 0;
            self.window_start = Instant::now();
        }
        if self.last_clip.elapsed() >= CLIPBOARD_POLL {
            self.last_clip = Instant::now();
            if let Some(clip) = self.clipboard.as_mut() {
                if let Some(msg) = clip.poll_outgoing() {
                    let _ = send_sealed(
                        transport,
                        &self.keys,
                        seq,
                        PacketType::Control,
                        0,
                        &json_payload(&msg)?,
                        self.peer,
                    )
                    .await;
                }
            }
        }
        Ok(())
    }

    /// Returns `Ok(false)` when the client said goodbye.
    async fn on_packet(
        &mut self,
        transport: &Transport,
        pkt: &[u8],
        seq: &mut SeqCounter,
    ) -> Result<bool> {
        let (header, payload) = open(&self.keys, pkt, &mut self.scratch)?;
        if !header.typ.is_handshake() && !self.replay.check_and_update(header.seq) {
            return Ok(true);
        }
        self.last_client = Instant::now();
        match header.typ {
            PacketType::Input => {
                let msg: InputMsg = json_from_slice(payload)?;
                self.input.apply(&msg.events);
            }
            PacketType::Control => {
                let msg: ControlMsg = json_from_slice(payload)?;
                self.on_control(transport, seq, msg).await?;
            }
            PacketType::Ping => {
                send_sealed(
                    transport,
                    &self.keys,
                    seq,
                    PacketType::Pong,
                    0,
                    payload,
                    self.peer,
                )
                .await?;
            }
            PacketType::Goodbye => return Ok(false),
            _ => {}
        }
        Ok(true)
    }

    async fn on_control(
        &mut self,
        _transport: &Transport,
        _seq: &mut SeqCounter,
        msg: ControlMsg,
    ) -> Result<()> {
        match msg {
            ControlMsg::MouseCaptured { relative, captured } => {
                self.input.set_relative(relative);
                if !captured {
                    self.input.release_all();
                }
            }
            ControlMsg::ReleaseAllInput => self.input.release_all(),
            ControlMsg::SetBitrate { kbps } => self.adapt_bitrate(kbps, "client request")?,
            ControlMsg::SetFps { fps } => {
                tracing::debug!("client asked for {fps} fps; applies on reconnect")
            }
            ControlMsg::RequestIdr => tracing::debug!("client requested IDR; next GOP will serve"),
            ControlMsg::Stats {
                loss_pct, rtt_ms, ..
            } => {
                if let Some(abr) = self.abr.as_mut() {
                    if let Some(kbps) = abr.observe(loss_pct, rtt_ms) {
                        if self.last_abr.elapsed() >= Duration::from_secs(COOLDOWN_SECS) {
                            self.adapt_bitrate(
                                kbps,
                                &format!("loss {loss_pct:.1}% rtt {rtt_ms:.0}ms"),
                            )?;
                        }
                    }
                }
            }
            ControlMsg::ClientQuality { bitrate_kbps, .. } => {
                if bitrate_kbps > 0 {
                    self.adapt_bitrate(bitrate_kbps, "quality preset")?;
                }
            }
            ControlMsg::Clipboard { text } => {
                if let Some(clip) = self.clipboard.as_mut() {
                    clip.apply_remote(&text);
                }
            }
        }
        Ok(())
    }

    fn adapt_bitrate(&mut self, kbps: u32, why: &str) -> Result<()> {
        self.last_abr = Instant::now();
        let next = self.enc.with_bitrate(kbps);
        let (vtx, vrx) = crossbeam_channel::bounded(8);
        let pipe = VideoPipeline::start(next.clone(), vtx)?;
        self._video = pipe;
        self.enc = next;
        self.video_rx = vrx;
        self.status
            .lock()
            .push_log(format!("Bitrate adapted to {kbps} kbps ({why})"));
        Ok(())
    }
}

fn classify_peer(peer: SocketAddr) -> String {
    match peer.ip() {
        std::net::IpAddr::V4(v4) if v4.is_loopback() || v4.is_private() => "LAN".into(),
        std::net::IpAddr::V4(v4) if brolink_core::config::is_cgnat_v4(v4) => "Tailscale".into(),
        std::net::IpAddr::V6(v6) if v6.is_loopback() => "LAN".into(),
        _ => "Internet".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_spec_parsing() {
        assert!(resolve_relay("").is_none());
        assert!(resolve_relay("   ").is_none());
        let r = resolve_relay("198.51.100.7:47851").expect("ip:port");
        assert_eq!(r.addr, "198.51.100.7:47851".parse::<SocketAddr>().unwrap());
        // A bare IP picks up the default relay port.
        let r = resolve_relay("198.51.100.7").expect("bare ip");
        assert_eq!(r.addr.port(), 47851);
        // Each host run gets a distinct rendezvous token.
        let a = resolve_relay("198.51.100.7:47851").unwrap();
        let b = resolve_relay("198.51.100.7:47851").unwrap();
        assert_ne!(a.token, b.token);
    }

    #[test]
    fn status_end_session_clears_live_fields() {
        let mut st = HostStatus {
            client: Some("Mac".into()),
            streaming: true,
            fps: 60.0,
            bitrate_kbps: 25_000.0,
            pending_pin: Some("123456".into()),
            ..Default::default()
        };
        st.end_session("Stream ended");
        assert!(st.client.is_none());
        assert!(!st.streaming);
        assert_eq!(st.fps, 0.0);
        assert_eq!(st.bitrate_kbps, 0.0);
        assert!(st.pending_pin.is_none());
        assert_eq!(st.log.last().unwrap(), "Stream ended");
    }

    #[test]
    fn status_log_is_bounded() {
        let mut st = HostStatus::default();
        for i in 0..(LOG_LINES * 3) {
            st.push_log(format!("line {i}"));
        }
        assert_eq!(st.log.len(), LOG_LINES);
        // Oldest lines are the ones dropped.
        assert_eq!(
            st.log.last().unwrap(),
            &format!("line {}", LOG_LINES * 3 - 1)
        );
    }

    #[test]
    fn upnp_mapping_prefers_the_routers_public_ip() {
        let stun: SocketAddr = "203.0.113.9:47850".parse().unwrap();
        let mapped = PortMapping {
            external: "198.51.100.7:40000".parse::<SocketAddrV4>().unwrap(),
            via: brolink_core::upnp::MappingVia::Upnp,
        };
        assert_eq!(
            merge_upnp_wan(Some(stun), &mapped),
            "198.51.100.7:40000".parse::<SocketAddr>().unwrap()
        );
    }

    #[test]
    fn nat_pmp_without_an_ip_keeps_stuns_address_and_the_mapped_port() {
        let stun: SocketAddr = "203.0.113.9:47850".parse().unwrap();
        let mapped = PortMapping {
            external: SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 40000),
            via: brolink_core::upnp::MappingVia::NatPmp,
        };
        assert_eq!(
            merge_upnp_wan(Some(stun), &mapped),
            "203.0.113.9:40000".parse::<SocketAddr>().unwrap()
        );
    }

    #[test]
    fn classify_peer_names_the_path() {
        assert_eq!(classify_peer("192.168.1.5:47850".parse().unwrap()), "LAN");
        assert_eq!(
            classify_peer("100.64.0.30:47850".parse().unwrap()),
            "Tailscale"
        );
        assert_eq!(
            classify_peer("203.0.113.9:47850".parse().unwrap()),
            "Internet"
        );
    }

    #[test]
    fn encoder_key_changes_with_every_relevant_setting() {
        let base = EncoderKey {
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_kbps: 25_000,
            monitor: 0,
            prefer: "auto".into(),
        };
        for changed in [
            EncoderKey {
                width: 2560,
                ..base.clone()
            },
            EncoderKey {
                height: 1440,
                ..base.clone()
            },
            EncoderKey {
                fps: 120,
                ..base.clone()
            },
            EncoderKey {
                bitrate_kbps: 40_000,
                ..base.clone()
            },
            EncoderKey {
                monitor: 1,
                ..base.clone()
            },
            EncoderKey {
                prefer: "libx264".into(),
                ..base.clone()
            },
        ] {
            assert_ne!(
                base, changed,
                "cache must not reuse an encoder across this change"
            );
        }
        assert_eq!(base, base.clone());
    }
}
