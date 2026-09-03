//! Client connection: handshake, video/audio receive, input send.

use crate::audio::AudioPlayer;
use crate::decode::{H264Decoder, VideoSink};
use anyhow::{Context, Result};
use bytes::BytesMut;
use forgelink_core::codec::FrameAssembler;
use forgelink_core::config::ClientConfig;
use forgelink_core::crypto::{random_bytes, verify_handshake, EphKey, ReplayWindow, SessionKeys};
use forgelink_core::identity::Identity;
use forgelink_core::net::{bind_udp_ephemeral, RelayLink, Transport, RECV_BUF};
use forgelink_core::proto::*;
use forgelink_core::ticket::{parse_endpoint, Ticket};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// How long to wait for a host to answer `Hello` on any candidate address.
const HELLO_TIMEOUT: Duration = Duration::from_secs(8);
/// Retransmit `Hello` this often while waiting — the first datagram is also the
/// one punching the NAT hole, and it is the most likely to be dropped.
const HELLO_RETRY: Duration = Duration::from_millis(400);
/// The host probes encoders before it can answer, which takes a few seconds.
const READY_TIMEOUT: Duration = Duration::from_secs(45);
const PING_INTERVAL: Duration = Duration::from_millis(250);
/// Give up if the host goes completely quiet.
const HOST_TIMEOUT: Duration = Duration::from_secs(8);
const RELAY_KEEPALIVE: Duration = Duration::from_secs(10);
/// How often the network loop services the socket when it is otherwise idle.
const POLL_INTERVAL: Duration = Duration::from_millis(1);

pub enum ClientCmd {
    Connect(Box<ConnectRequest>),
    Pin(String),
    Input(Vec<InputEvent>),
    Control(ControlMsg),
    Disconnect,
}

pub struct ConnectRequest {
    pub target: String,
    pub cfg: ClientConfig,
    pub identity: Identity,
}

#[derive(Clone)]
pub enum ClientEvent {
    Log(String),
    NeedPin {
        host: String,
    },
    Ready(SessionReady),
    Stats {
        rtt_ms: f32,
        fps: f32,
        bitrate_kbps: f32,
        loss: f32,
        decoder_ms: f32,
    },
    Error(String),
    Disconnected,
}

pub fn spawn(
    cmd_rx: mpsc::UnboundedReceiver<ClientCmd>,
    ev_tx: mpsc::UnboundedSender<ClientEvent>,
    video: Arc<VideoSink>,
) {
    std::thread::Builder::new()
        .name("forgelink-client-net".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("client tokio runtime");
            rt.block_on(run(cmd_rx, ev_tx, video));
        })
        .expect("spawn client net thread");
}

async fn run(
    mut cmd_rx: mpsc::UnboundedReceiver<ClientCmd>,
    ev_tx: mpsc::UnboundedSender<ClientEvent>,
    video: Arc<VideoSink>,
) {
    let mut live: Option<Live> = None;
    // A fixed cadence rather than a fresh sleep each iteration: recreating the
    // timer inside `select!` lets a steady stream of input commands starve the
    // poll branch, which shows up as video freezing while the mouse moves.
    let mut ticker = tokio::time::interval(POLL_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    ClientCmd::Connect(req) => {
                        if let Some(mut old) = live.take() {
                            let _ = old.goodbye().await;
                        }
                        match connect(&req, &ev_tx, video.clone()).await {
                            Ok(l) => {
                                if !l.awaiting_pin {
                                    let _ = ev_tx.send(ClientEvent::Ready(l.ready.clone()));
                                }
                                live = Some(l);
                            }
                            Err(e) => {
                                let _ = ev_tx.send(ClientEvent::Error(format!("{e:#}")));
                                let _ = ev_tx.send(ClientEvent::Disconnected);
                            }
                        }
                    }
                    ClientCmd::Pin(pin) => {
                        if let Some(l) = live.as_mut() {
                            if let Err(e) = l.send_pin(&pin).await {
                                let _ = ev_tx.send(ClientEvent::Error(format!("{e:#}")));
                            }
                        }
                    }
                    ClientCmd::Input(events) => {
                        if let Some(l) = live.as_mut() {
                            if let Err(e) = l.send_input(events).await {
                                tracing::debug!("input send failed: {e:#}");
                            }
                        }
                    }
                    ClientCmd::Control(msg) => {
                        if let Some(l) = live.as_mut() {
                            if let Err(e) = l.send_control(msg).await {
                                tracing::debug!("control send failed: {e:#}");
                            }
                        }
                    }
                    ClientCmd::Disconnect => {
                        if let Some(mut l) = live.take() {
                            let _ = l.goodbye().await;
                        }
                        let _ = ev_tx.send(ClientEvent::Disconnected);
                    }
                }
            }
            _ = ticker.tick(), if live.is_some() => {
                let Some(l) = live.as_mut() else { continue };
                match l.poll(&ev_tx).await {
                    Ok(true) => {}
                    Ok(false) => {
                        live = None;
                        let _ = ev_tx.send(ClientEvent::Disconnected);
                    }
                    Err(e) => {
                        let _ = ev_tx.send(ClientEvent::Error(format!("{e:#}")));
                        live = None;
                        let _ = ev_tx.send(ClientEvent::Disconnected);
                    }
                }
            }
        }
    }
}

struct Live {
    transport: Transport,
    peer: SocketAddr,
    keys: SessionKeys,
    seq: SeqCounter,
    session_id: [u8; 16],
    ready: SessionReady,
    assembler: FrameAssembler,
    decoder: H264Decoder,
    video: Arc<VideoSink>,
    audio: Option<AudioPlayer>,
    replay: ReplayWindow,
    scratch: Vec<u8>,
    buf: Vec<u8>,
    last_ping: Instant,
    ping_sent: Option<Instant>,
    last_host_packet: Instant,
    last_relay_ka: Instant,
    rtt_ms: f32,
    frames: u32,
    bytes: u64,
    window: Instant,
    last_decode_ms: f32,
    awaiting_pin: bool,
}

/// Work out where to send `Hello`, and which host identity to insist on.
#[derive(Debug)]
struct Destination {
    candidates: Vec<SocketAddr>,
    relay: Option<RelayLink>,
    expected_host: Option<[u8; 32]>,
    host_label: String,
}

fn resolve_target(target: &str) -> Result<Destination> {
    if let Ok(t) = Ticket::decode(target) {
        let relay = t.relay.map(|r| RelayLink {
            addr: SocketAddr::V4(r.addr),
            token: r.token,
        });
        let mut candidates = t.candidate_addrs();
        if let Some(r) = relay {
            // Last resort: direct paths are always faster when they work.
            candidates.push(r.addr);
        }
        if candidates.is_empty() {
            anyhow::bail!("this ticket contains no addresses to connect to");
        }
        return Ok(Destination {
            candidates,
            relay,
            expected_host: Some(t.host_id),
            host_label: t.name,
        });
    }
    let addr = parse_endpoint(target, DEFAULT_PORT)?;
    Ok(Destination {
        candidates: vec![addr],
        relay: None,
        // Connecting to a bare address means there is no identity to check
        // against; the PIN is what protects this path.
        expected_host: None,
        host_label: addr.to_string(),
    })
}

async fn connect(
    req: &ConnectRequest,
    ev: &mpsc::UnboundedSender<ClientEvent>,
    video: Arc<VideoSink>,
) -> Result<Live> {
    let ConnectRequest {
        target,
        cfg,
        identity,
    } = req;
    let dest = resolve_target(target)?;
    let _ = ev.send(ClientEvent::Log(format!(
        "Connecting to {}…",
        dest.host_label
    )));
    if dest.expected_host.is_none() {
        let _ = ev.send(ClientEvent::Log(
            "No ticket — the host's identity cannot be verified on this connection.".into(),
        ));
    }

    let transport = Transport::new(bind_udp_ephemeral()?, dest.relay);
    let eph = EphKey::generate();
    let nonce = random_bytes::<16>();
    let quality = cfg.quality.sanitized();
    let hello = HelloMsg {
        client_id: identity.public,
        client_eph: eph.public,
        nonce,
        name: cfg.name.clone(),
        app_version: env!("CARGO_PKG_VERSION").into(),
        width: Some(quality.width),
        height: Some(quality.height),
        fps: Some(quality.fps),
        bitrate_kbps: Some(quality.bitrate_kbps),
    };
    let pkt = encode_plain(PacketType::Hello, 1, &json_payload(&hello)?);

    let mut buf = vec![0u8; RECV_BUF];
    let deadline = Instant::now() + HELLO_TIMEOUT;
    let mut last_send: Option<Instant> = None;
    let (from, ack) = loop {
        let now = Instant::now();
        if now >= deadline {
            anyhow::bail!(
                "no reply from the host on {} (check the ticket, that the host is running, \
                 and that UDP is allowed through its firewall)",
                describe(&dest.candidates)
            );
        }
        if last_send.is_none_or(|t| t.elapsed() >= HELLO_RETRY) {
            last_send = Some(now);
            for c in &dest.candidates {
                if transport.send_to(&pkt, *c).await.is_ok() && last_send == Some(now) {
                    tracing::debug!("Hello -> {c}");
                }
            }
        }
        let wait = HELLO_RETRY.min(deadline.saturating_duration_since(now));
        let Ok(Ok((n, from))) = tokio::time::timeout(wait, transport.recv_from(&mut buf)).await
        else {
            continue;
        };
        let Ok(h) = parse_header(&buf[..n]) else {
            continue;
        };
        if h.typ != PacketType::HelloAck {
            continue;
        }
        match json_from_slice::<HelloAck>(&buf[HEADER_LEN..n]) {
            Ok(ack) => break (from, ack),
            Err(e) => {
                tracing::debug!("malformed HelloAck from {from}: {e}");
                continue;
            }
        }
    };

    // Confirm we are talking to the machine the ticket names, *before* trusting
    // its signature. Verifying the signature against the identity the responder
    // supplied only proves it holds some key, so without this check any machine
    // that can reach us could answer in the host's place.
    if let Some(expected) = dest.expected_host {
        if ack.server_id != expected {
            anyhow::bail!(
                "the machine at {from} is not the host this ticket is for \
                 (expected {}…, got {}…) — refusing to connect",
                short_id(&expected),
                short_id(&ack.server_id)
            );
        }
    }
    verify_handshake(
        &ack.server_id,
        &ack.signature,
        &eph.public,
        &ack.server_eph,
        &nonce,
        &ack.nonce,
    )
    .context("the host's handshake signature did not verify")?;

    let keys = SessionKeys::derive(&eph.shared(&ack.server_eph), &nonce, &ack.nonce, false)?;
    let via = if dest.relay.is_some_and(|r| r.addr == from) {
        format!("{from} (relay)")
    } else {
        from.to_string()
    };
    let _ = ev.send(ClientEvent::Log(format!(
        "Connected to {} via {via}",
        ack.host_name
    )));

    let audio = match AudioPlayer::start(cfg.volume) {
        Ok(a) => Some(a),
        Err(e) => {
            let _ = ev.send(ClientEvent::Log(format!("Audio output unavailable: {e:#}")));
            None
        }
    };

    let mut live = Live {
        transport,
        peer: from,
        keys,
        seq: SeqCounter::new(),
        session_id: ack.session_id,
        ready: SessionReady {
            width: quality.width,
            height: quality.height,
            fps: quality.fps,
            bitrate_kbps: 0,
            codec: "h264".into(),
            encoder: String::new(),
            audio: AudioFormat::PcmS16Le48kStereo,
            monitor_name: String::new(),
        },
        assembler: FrameAssembler::default(),
        decoder: H264Decoder::new()?,
        video,
        audio,
        replay: ReplayWindow::default(),
        scratch: Vec::new(),
        buf: vec![0u8; RECV_BUF],
        last_ping: Instant::now(),
        ping_sent: None,
        last_host_packet: Instant::now(),
        last_relay_ka: Instant::now(),
        rtt_ms: 0.0,
        frames: 0,
        bytes: 0,
        window: Instant::now(),
        last_decode_ms: 0.0,
        awaiting_pin: ack.needs_pin,
    };

    if ack.needs_pin {
        let _ = ev.send(ClientEvent::NeedPin {
            host: ack.host_name.clone(),
        });
        return Ok(live);
    }
    live.wait_ready().await?;
    Ok(live)
}

fn describe(addrs: &[SocketAddr]) -> String {
    addrs
        .iter()
        .map(|a| a.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn short_id(id: &[u8; 32]) -> String {
    data_encoding_hex(&id[..4])
}

fn data_encoding_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl Live {
    async fn send(&mut self, typ: PacketType, payload: &[u8]) -> Result<()> {
        let mut out = BytesMut::with_capacity(MAX_DATAGRAM);
        seal(&self.keys, typ, self.seq.next_seq()?, 0, payload, &mut out)?;
        self.transport.send_to(&out, self.peer).await?;
        Ok(())
    }

    async fn send_pin(&mut self, pin: &str) -> Result<()> {
        let msg = PairPin {
            session_id: self.session_id,
            pin: pin.trim().to_string(),
        };
        self.send(PacketType::PairPin, &json_payload(&msg)?).await
    }

    async fn send_input(&mut self, events: Vec<InputEvent>) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        // A burst of events can exceed the MTU; split it rather than letting
        // the whole batch fail to seal and vanish.
        for batch in chunk_input_events(events) {
            let msg = InputMsg { events: batch };
            self.send(PacketType::Input, &json_payload(&msg)?).await?;
        }
        Ok(())
    }

    async fn send_control(&mut self, msg: ControlMsg) -> Result<()> {
        self.send(PacketType::Control, &json_payload(&msg)?).await
    }

    async fn goodbye(&mut self) -> Result<()> {
        // Ask the host to let go of anything we were holding down before we go.
        let _ = self.send_control(ControlMsg::ReleaseAllInput).await;
        self.send(PacketType::Goodbye, b"{}").await
    }

    async fn wait_ready(&mut self) -> Result<()> {
        let deadline = Instant::now() + READY_TIMEOUT;
        let mut buf = vec![0u8; RECV_BUF];
        while Instant::now() < deadline {
            let Ok(Ok((n, from))) = tokio::time::timeout(
                Duration::from_millis(500),
                self.transport.recv_from(&mut buf),
            )
            .await
            else {
                continue;
            };
            if !self.transport.accepts_from(self.peer, from) {
                continue;
            }
            let Ok((h, pt)) = open(&self.keys, &buf[..n], &mut self.scratch) else {
                continue;
            };
            match h.typ {
                PacketType::PairResult => {
                    let r: PairResult = json_from_slice(pt)?;
                    if !r.ok {
                        anyhow::bail!("{}", r.message);
                    }
                    self.awaiting_pin = false;
                }
                PacketType::SessionReady => {
                    self.ready = json_from_slice(pt)?;
                    self.last_host_packet = Instant::now();
                    return Ok(());
                }
                _ => {}
            }
        }
        anyhow::bail!("timed out waiting for the host to start streaming (the encoder probe can take a few seconds)")
    }

    /// Returns `Ok(false)` when the session has ended.
    async fn poll(&mut self, ev: &mpsc::UnboundedSender<ClientEvent>) -> Result<bool> {
        if self.awaiting_pin {
            return self.poll_pairing(ev).await;
        }

        if self.last_host_packet.elapsed() > HOST_TIMEOUT {
            anyhow::bail!("the host stopped responding");
        }
        if self.last_ping.elapsed() >= PING_INTERVAL {
            self.last_ping = Instant::now();
            self.ping_sent = Some(Instant::now());
            let t = now_us();
            let _ = self.send(PacketType::Ping, &t.to_le_bytes()).await;
        }
        if self.transport.relay().is_some() && self.last_relay_ka.elapsed() >= RELAY_KEEPALIVE {
            self.last_relay_ka = Instant::now();
            let _ = self.transport.relay_keepalive().await;
        }

        // Drain everything the socket has for us this tick.
        loop {
            let mut buf = std::mem::take(&mut self.buf);
            let got =
                tokio::time::timeout(Duration::ZERO, self.transport.recv_from(&mut buf)).await;
            let result = match got {
                Ok(Ok((n, from))) if self.transport.accepts_from(self.peer, from) => {
                    self.handle_pkt(&buf[..n], ev)
                }
                Ok(Ok(_)) => Ok(true),
                _ => {
                    self.buf = buf;
                    break;
                }
            };
            self.buf = buf;
            match result {
                Ok(true) => {}
                Ok(false) => return Ok(false),
                // Stray, duplicated, and corrupt datagrams are ordinary on the
                // open internet. Dropping a working session over one would be a
                // far worse outcome than ignoring it.
                Err(e) => tracing::debug!("ignored packet: {e:#}"),
            }
        }

        if self.window.elapsed() >= Duration::from_secs(1) {
            let dt = self.window.elapsed().as_secs_f32().max(0.001);
            let _ = ev.send(ClientEvent::Stats {
                rtt_ms: self.rtt_ms,
                fps: self.frames as f32 / dt,
                bitrate_kbps: (self.bytes as f32 * 8.0 / dt) / 1000.0,
                loss: self.assembler.loss_pct(),
                decoder_ms: self.last_decode_ms,
            });
            self.frames = 0;
            self.bytes = 0;
            self.window = Instant::now();
        }
        Ok(true)
    }

    async fn poll_pairing(&mut self, ev: &mpsc::UnboundedSender<ClientEvent>) -> Result<bool> {
        let mut buf = std::mem::take(&mut self.buf);
        let got =
            tokio::time::timeout(Duration::from_millis(5), self.transport.recv_from(&mut buf))
                .await;
        let outcome = match got {
            Ok(Ok((n, from))) if self.transport.accepts_from(self.peer, from) => {
                match open(&self.keys, &buf[..n], &mut self.scratch) {
                    Ok((h, pt)) if h.typ == PacketType::PairResult => {
                        json_from_slice::<PairResult>(pt).ok()
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        self.buf = buf;
        let Some(result) = outcome else {
            return Ok(true);
        };
        if !result.ok {
            anyhow::bail!("{}", result.message);
        }
        self.awaiting_pin = false;
        let _ = ev.send(ClientEvent::Log("Paired. Starting the stream…".into()));
        self.wait_ready().await?;
        let _ = ev.send(ClientEvent::Ready(self.ready.clone()));
        Ok(true)
    }

    /// Returns `Ok(false)` when the host said goodbye.
    fn handle_pkt(&mut self, pkt: &[u8], ev: &mpsc::UnboundedSender<ClientEvent>) -> Result<bool> {
        // The plaintext buffer lives outside `self` for the duration of the
        // call so the decoder and assembler can take `&mut self` while holding
        // a slice of it. It is put back either way, so decrypting never costs
        // an allocation in the steady state.
        let mut scratch = std::mem::take(&mut self.scratch);
        let result = self.handle_decrypted(pkt, &mut scratch, ev);
        self.scratch = scratch;
        result
    }

    fn handle_decrypted(
        &mut self,
        pkt: &[u8],
        scratch: &mut Vec<u8>,
        ev: &mpsc::UnboundedSender<ClientEvent>,
    ) -> Result<bool> {
        let (h, payload) = open(&self.keys, pkt, scratch)?;
        if !h.typ.is_handshake() && !self.replay.check_and_update(h.seq) {
            return Ok(true);
        }
        self.last_host_packet = Instant::now();
        self.bytes += pkt.len() as u64;
        let mut result = Ok(true);
        match h.typ {
            PacketType::Video => {
                if let Some(frame) = self.assembler.push(payload) {
                    // Reclaim whatever the UI finished with, so a steady stream
                    // decodes into the same buffer instead of allocating 8 MB
                    // per frame.
                    if let Some(buf) = self.video.take_spare() {
                        self.decoder.reuse(buf);
                    }
                    match self.decoder.decode(&frame) {
                        Ok(Some(pic)) => {
                            self.frames += 1;
                            self.last_decode_ms = pic.decode_ms;
                            if let Some(old) = self.video.put(pic) {
                                self.decoder.reuse(old);
                            }
                        }
                        Ok(None) => {}
                        Err(e) => tracing::debug!("decode: {e:#}"),
                    }
                }
            }
            PacketType::Audio => {
                if let Ok((_ts, data)) = parse_audio_payload(payload) {
                    if let Some(a) = self.audio.as_ref() {
                        a.push_s16_48k_stereo(data);
                    }
                }
            }
            PacketType::Pong => {
                if let Some(sent) = self.ping_sent.take() {
                    // Smooth a little so a single delayed reply does not make
                    // the readout jump.
                    let sample = sent.elapsed().as_secs_f32() * 1000.0;
                    self.rtt_ms = if self.rtt_ms == 0.0 {
                        sample
                    } else {
                        self.rtt_ms * 0.8 + sample * 0.2
                    };
                }
            }
            PacketType::Goodbye => {
                let _ = ev.send(ClientEvent::Log("The host ended the session.".into()));
                result = Ok(false);
            }
            PacketType::SessionReady => {
                if let Ok(ready) = json_from_slice::<SessionReady>(payload) {
                    self.ready = ready.clone();
                    let _ = ev.send(ClientEvent::Ready(ready));
                }
            }
            _ => {}
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forgelink_core::ticket::{Candidate, CandidateKind, RelayHint};
    use std::net::{Ipv4Addr, SocketAddrV4};

    fn v4(a: u8, b: u8, c: u8, d: u8, port: u16) -> SocketAddrV4 {
        SocketAddrV4::new(Ipv4Addr::new(a, b, c, d), port)
    }

    #[test]
    fn a_ticket_pins_the_expected_host_identity() {
        let id = Identity::generate();
        let t = Ticket::new(
            &id,
            vec![Candidate {
                kind: CandidateKind::Lan,
                addr: v4(192, 168, 1, 5, 47850),
            }],
            None,
            "PC",
        );
        let dest = resolve_target(&t.display_code()).unwrap();
        assert_eq!(
            dest.expected_host,
            Some(id.public),
            "the ticket's identity must be carried into the handshake check"
        );
        assert_eq!(
            dest.candidates,
            vec![SocketAddr::V4(v4(192, 168, 1, 5, 47850))]
        );
        assert!(dest.relay.is_none());
        assert_eq!(dest.host_label, "PC");
    }

    #[test]
    fn a_bare_address_has_no_identity_to_pin() {
        let dest = resolve_target("192.168.1.5").unwrap();
        assert_eq!(
            dest.candidates,
            vec![SocketAddr::V4(v4(192, 168, 1, 5, DEFAULT_PORT))]
        );
        assert!(
            dest.expected_host.is_none(),
            "there is no identity to check when the user typed an address"
        );

        let dest = resolve_target("192.168.1.5:9000").unwrap();
        assert_eq!(
            dest.candidates,
            vec![SocketAddr::V4(v4(192, 168, 1, 5, 9000))]
        );
    }

    #[test]
    fn every_ticket_address_is_tried_with_the_relay_last() {
        let id = Identity::generate();
        let t = Ticket::new(
            &id,
            vec![
                Candidate {
                    kind: CandidateKind::Wan,
                    addr: v4(203, 0, 113, 9, 47850),
                },
                Candidate {
                    kind: CandidateKind::Lan,
                    addr: v4(192, 168, 1, 5, 47850),
                },
                Candidate {
                    kind: CandidateKind::Tailscale,
                    addr: v4(100, 90, 1, 2, 47850),
                },
            ],
            Some(RelayHint {
                addr: v4(198, 51, 100, 7, 47851),
                token: [3u8; 16],
            }),
            "PC",
        );
        let dest = resolve_target(&t.encode()).unwrap();
        assert_eq!(dest.candidates.len(), 4);
        // LAN first (fastest when it works), relay last (slowest).
        assert_eq!(
            dest.candidates[0],
            SocketAddr::V4(v4(192, 168, 1, 5, 47850))
        );
        assert_eq!(
            *dest.candidates.last().unwrap(),
            SocketAddr::V4(v4(198, 51, 100, 7, 47851))
        );
        let relay = dest.relay.expect("relay carried through");
        assert_eq!(relay.token, [3u8; 16]);
    }

    #[test]
    fn unusable_targets_are_rejected_with_a_message() {
        assert!(resolve_target("").is_err());
        assert!(resolve_target("not an address").is_err());
        // A well-formed ticket with nothing reachable in it.
        let id = Identity::generate();
        let t = Ticket::new(&id, Vec::new(), None, "PC");
        let err = resolve_target(&t.encode()).unwrap_err().to_string();
        assert!(err.contains("no addresses"), "{err}");
    }

    #[test]
    fn short_id_is_readable_hex() {
        let id = [
            0xAB, 0xCD, 0xEF, 0x01, 0xFF, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0,
        ];
        assert_eq!(short_id(&id), "abcdef01");
    }

    #[test]
    fn describe_lists_every_candidate() {
        let addrs = vec![
            SocketAddr::V4(v4(192, 168, 1, 5, 47850)),
            SocketAddr::V4(v4(203, 0, 113, 9, 47850)),
        ];
        let s = describe(&addrs);
        assert!(s.contains("192.168.1.5:47850"));
        assert!(s.contains("203.0.113.9:47850"));
    }
}
