//! One streaming session: starts the moonlight-common-c connection on a
//! worker thread, routes its callbacks to the decoders, and reports what
//! happens through [`Event`]s. moonlight-common-c holds exactly one
//! connection at a time, so sessions are serialised through a global lock.

use crate::audio::Player;
use crate::ffi;
use crate::video::{self, Decoder, FrameSlot};
use parking_lot::{Condvar, Mutex};
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Instant;

static CONNECTION: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A connection stage is starting.
    Stage(String),
    Connected,
    /// Setup failed before the connection was established.
    Failed {
        stage: String,
        code: i32,
    },
    /// The connection ended after it was established. `code` 0 means the PC
    /// closed it on purpose.
    Terminated {
        code: i32,
        message: String,
    },
    /// The network is struggling (`true`) or fine again (`false`).
    Poor(bool),
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    /// Ask for HEVC when both sides can; otherwise H.264.
    pub hevc: bool,
    /// The PC is not on this LAN: smaller packets, remote pacing.
    pub remote: bool,
}

/// What `Session::start` needs from `/serverinfo` and `/launch`.
#[derive(Debug, Clone)]
pub struct Server {
    pub address: String,
    pub app_version: String,
    pub gfe_version: String,
    pub rtsp_url: String,
    pub codec_mode_support: i32,
}

#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub fps: f32,
    pub mbps: f32,
    pub rtt_ms: u32,
    pub rtt_var_ms: u32,
    pub decode_ms: f32,
    pub host_ms: f32,
    pub width: u32,
    pub height: u32,
    pub decoder: &'static str,
    /// Share of video packets in the last second that arrived too late or
    /// not at all and could not be rebuilt from FEC.
    pub loss_pct: f32,
    /// Packets FEC did rebuild in the last second.
    pub fec_recovered: u32,
}

#[derive(Default)]
struct Window {
    since: Option<Instant>,
    frames: u32,
    bytes: u64,
    decode_us: u64,
    host_tenths: u64,
    /// RTP totals at the start of the window, to difference against.
    rtp: Option<ffi::RtpVideoStats>,
}

struct Inner {
    events: Sender<Event>,
    wake: Box<dyn Fn() + Send + Sync>,
    frames: Arc<FrameSlot>,
    decoder: Mutex<Option<Box<dyn Decoder>>>,
    audio: Mutex<Option<Player>>,
    window: Mutex<Window>,
    stats: Mutex<Stats>,
    connected: AtomicBool,
    finished: AtomicBool,
    /// Set when the connection ended or a stop was asked for.
    done: Mutex<bool>,
    done_cv: Condvar,
}

impl Inner {
    fn emit(&self, e: Event) {
        let _ = self.events.send(e);
        (self.wake)();
    }

    fn finish(&self) {
        *self.done.lock() = true;
        self.done_cv.notify_all();
    }
}

pub struct Session {
    inner: Arc<Inner>,
}

/// The keyboard-and-mouse side of a session, cheap to clone and safe to
/// use from any thread: a worker that has to type on the PC after a
/// network round trip holds one of these instead of the session.
#[derive(Clone)]
pub struct Input {
    inner: Arc<Inner>,
}

/// One UTF-8 text event is kept this small: the control stream's packet
/// buffer is 128 bytes on hosts without the newer encryption.
const TEXT_CHUNK: usize = 32;

impl Input {
    pub fn connected(&self) -> bool {
        self.inner.connected.load(Ordering::Acquire)
    }

    pub fn mouse_move(&self, dx: i16, dy: i16) {
        if self.connected() {
            unsafe { ffi::LiSendMouseMoveEvent(dx, dy) };
        }
    }

    pub fn mouse_position(&self, x: i16, y: i16, width: i16, height: i16) {
        if self.connected() {
            unsafe { ffi::LiSendMousePositionEvent(x, y, width, height) };
        }
    }

    /// `button` is one of `ffi::BUTTON_*`.
    pub fn mouse_button(&self, button: c_int, down: bool) {
        if self.connected() {
            let action = if down {
                ffi::BUTTON_ACTION_PRESS
            } else {
                ffi::BUTTON_ACTION_RELEASE
            };
            unsafe { ffi::LiSendMouseButtonEvent(action, button) };
        }
    }

    /// `vk` is a Windows virtual-key code; `modifiers` a mask of `ffi::MODIFIER_*`.
    pub fn key(&self, vk: i16, down: bool, modifiers: c_char) {
        if self.connected() {
            let action = if down {
                ffi::KEY_ACTION_DOWN
            } else {
                ffi::KEY_ACTION_UP
            };
            unsafe { ffi::LiSendKeyboardEvent(vk, action, modifiers) };
        }
    }

    /// Type `text` on the PC as it is, whatever the keyboard layouts.
    pub fn text(&self, text: &str) {
        if !self.connected() {
            return;
        }
        for chunk in text_chunks(text) {
            unsafe {
                ffi::LiSendUtf8TextEvent(chunk.as_ptr() as *const c_char, chunk.len() as u32)
            };
        }
    }

    /// Vertical and horizontal scroll in 1/120ths of a wheel click.
    pub fn scroll(&self, vertical: i16, horizontal: i16) {
        if self.connected() {
            if vertical != 0 {
                unsafe { ffi::LiSendHighResScrollEvent(vertical) };
            }
            if horizontal != 0 {
                unsafe { ffi::LiSendHighResHScrollEvent(horizontal) };
            }
        }
    }
}

/// `text` in pieces of at most [`TEXT_CHUNK`] bytes, cut between characters.
fn text_chunks(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + TEXT_CHUNK).min(text.len());
        while end > start && !text.is_char_boundary(end) {
            end -= 1;
        }
        if end == start {
            break;
        }
        out.push(&text[start..end]);
        start = end;
    }
    out
}

impl Session {
    /// Connect on a worker thread. `wake` is called whenever there is a new
    /// event or frame, so a UI can repaint.
    pub fn start(
        server: Server,
        settings: Settings,
        ri_key: [u8; 16],
        ri_iv: [u8; 16],
        frames: Arc<FrameSlot>,
        events: Sender<Event>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Session {
        let inner = Arc::new(Inner {
            events,
            wake: Box::new(wake),
            frames,
            decoder: Mutex::new(None),
            audio: Mutex::new(None),
            window: Mutex::new(Window::default()),
            stats: Mutex::new(Stats::default()),
            connected: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            done: Mutex::new(false),
            done_cv: Condvar::new(),
        });
        let worker = inner.clone();
        std::thread::Builder::new()
            .name("stream".into())
            .spawn(move || run(worker, server, settings, ri_key, ri_iv))
            .expect("spawn stream thread");
        Session { inner }
    }

    pub fn connected(&self) -> bool {
        self.inner.connected.load(Ordering::Acquire)
    }

    /// The worker has torn everything down; a new session may start.
    pub fn finished(&self) -> bool {
        self.inner.finished.load(Ordering::Acquire)
    }

    /// Ask the connection to end. Returns at once; poll [`Session::finished`].
    pub fn stop(&self) {
        if *self.inner.done.lock() {
            return;
        }
        unsafe { ffi::bl_interrupt() };
        self.inner.finish();
    }

    pub fn stats(&self) -> Stats {
        self.inner.stats.lock().clone()
    }

    /// A handle for sending input from other threads.
    pub fn input(&self) -> Input {
        Input {
            inner: self.inner.clone(),
        }
    }

    pub fn mouse_move(&self, dx: i16, dy: i16) {
        self.input().mouse_move(dx, dy);
    }

    pub fn mouse_position(&self, x: i16, y: i16, width: i16, height: i16) {
        self.input().mouse_position(x, y, width, height);
    }

    /// `button` is one of `ffi::BUTTON_*`.
    pub fn mouse_button(&self, button: c_int, down: bool) {
        self.input().mouse_button(button, down);
    }

    /// `vk` is a Windows virtual-key code; `modifiers` a mask of `ffi::MODIFIER_*`.
    pub fn key(&self, vk: i16, down: bool, modifiers: c_char) {
        self.input().key(vk, down, modifiers);
    }

    pub fn text(&self, text: &str) {
        self.input().text(text);
    }

    /// Vertical and horizontal scroll in 1/120ths of a wheel click.
    pub fn scroll(&self, vertical: i16, horizontal: i16) {
        self.input().scroll(vertical, horizontal);
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run(inner: Arc<Inner>, server: Server, s: Settings, ri_key: [u8; 16], ri_iv: [u8; 16]) {
    let _one_at_a_time = CONNECTION.lock();
    if *inner.done.lock() {
        inner.finished.store(true, Ordering::Release);
        return;
    }
    let address = CString::new(server.address).unwrap_or_default();
    let app_version = CString::new(server.app_version).unwrap_or_default();
    let gfe_version = CString::new(server.gfe_version).unwrap_or_default();
    let rtsp_url = CString::new(server.rtsp_url).unwrap_or_default();
    let si = ffi::ServerInfo {
        address: address.as_ptr(),
        app_version: app_version.as_ptr(),
        gfe_version: gfe_version.as_ptr(),
        rtsp_url: rtsp_url.as_ptr(),
        codec_mode_support: server.codec_mode_support,
    };
    let mut formats = video::supported_formats();
    if !s.hevc {
        formats &= ffi::VIDEO_FORMAT_MASK_H264;
    }
    let cfg = ffi::StreamConfig {
        width: s.width as c_int,
        height: s.height as c_int,
        fps: s.fps as c_int,
        bitrate_kbps: s.bitrate_kbps as c_int,
        packet_size: if s.remote { 1024 } else { 1392 },
        remote: if s.remote {
            ffi::STREAM_CFG_REMOTE
        } else {
            ffi::STREAM_CFG_AUTO
        },
        video_formats: formats,
        color_space: ffi::COLORSPACE_REC_709,
        color_range: ffi::COLOR_RANGE_LIMITED,
        encryption_flags: ffi::ENCFLG_ALL,
        video_capabilities: video::capabilities(),
        audio_capabilities: ffi::CAPABILITY_DIRECT_SUBMIT
            | ffi::CAPABILITY_SUPPORTS_ARBITRARY_AUDIO_DURATION,
        ri_key: ri_key.as_ptr(),
        ri_iv: ri_iv.as_ptr(),
    };
    let cb = ffi::Callbacks {
        video_setup,
        video_cleanup,
        video_frame,
        audio_setup,
        audio_cleanup,
        audio_packet,
        stage,
        connected,
        terminated,
        status,
        log,
    };
    let ctx = Arc::as_ptr(&inner) as *mut c_void;
    let rc = unsafe { ffi::bl_start(&si, &cfg, &cb, ctx) };
    if rc == 0 {
        let mut done = inner.done.lock();
        while !*done {
            inner.done_cv.wait(&mut done);
        }
    }
    inner.connected.store(false, Ordering::Release);
    unsafe { ffi::bl_stop() };
    *inner.decoder.lock() = None;
    *inner.audio.lock() = None;
    inner.frames.clear();
    inner.finished.store(true, Ordering::Release);
    (inner.wake)();
}

unsafe fn ctx<'a>(p: *mut c_void) -> &'a Inner {
    &*(p as *const Inner)
}

unsafe extern "C" fn video_setup(
    p: *mut c_void,
    format: c_int,
    w: c_int,
    h: c_int,
    _fps: c_int,
) -> c_int {
    let inner = ctx(p);
    match video::new_decoder(format, w as u32, h as u32) {
        Ok(d) => {
            let mut st = inner.stats.lock();
            st.width = w as u32;
            st.height = h as u32;
            st.decoder = d.name();
            drop(st);
            *inner.decoder.lock() = Some(d);
            0
        }
        Err(e) => {
            tracing::error!("video decoder: {e:#}");
            -1
        }
    }
}

unsafe extern "C" fn video_cleanup(p: *mut c_void) {
    *ctx(p).decoder.lock() = None;
}

unsafe extern "C" fn video_frame(
    p: *mut c_void,
    data: *const u8,
    len: c_int,
    frame_type: c_int,
    _frame_number: c_int,
    host_latency: u16,
    _receive_us: u64,
    _enqueue_us: u64,
) -> c_int {
    let inner = ctx(p);
    if data.is_null() || len <= 0 {
        return ffi::DR_OK;
    }
    let bytes = std::slice::from_raw_parts(data, len as usize);
    let mut guard = inner.decoder.lock();
    let Some(dec) = guard.as_mut() else {
        return ffi::DR_OK;
    };
    let t = Instant::now();
    let result = dec.decode(bytes, frame_type == ffi::FRAME_TYPE_IDR);
    let decode_us = t.elapsed().as_micros() as u64;
    drop(guard);
    match result {
        Ok(Some(frame)) => {
            inner.frames.publish(frame);
            account(inner, len as u64, decode_us, host_latency);
            (inner.wake)();
            ffi::DR_OK
        }
        Ok(None) => ffi::DR_OK,
        Err(e) => {
            tracing::warn!("decode: {e:#}");
            ffi::DR_NEED_IDR
        }
    }
}

/// Roll per-second averages into `stats`.
fn account(inner: &Inner, bytes: u64, decode_us: u64, host_tenths: u16) {
    let mut w = inner.window.lock();
    let since = *w.since.get_or_insert_with(Instant::now);
    w.frames += 1;
    w.bytes += bytes;
    w.decode_us += decode_us;
    w.host_tenths += host_tenths as u64;
    let connected = inner.connected.load(Ordering::Relaxed);
    if w.rtp.is_none() && connected {
        w.rtp = rtp_stats();
    }
    let elapsed = since.elapsed().as_secs_f32();
    if elapsed >= 1.0 {
        let mut st = inner.stats.lock();
        st.fps = w.frames as f32 / elapsed;
        st.mbps = w.bytes as f32 * 8.0 / elapsed / 1_000_000.0;
        st.decode_ms = w.decode_us as f32 / w.frames.max(1) as f32 / 1000.0;
        st.host_ms = w.host_tenths as f32 / w.frames.max(1) as f32 / 10.0;
        let mut rtt = 0u32;
        let mut var = 0u32;
        if connected && unsafe { ffi::LiGetEstimatedRttInfo(&mut rtt, &mut var) } {
            st.rtt_ms = rtt;
            st.rtt_var_ms = var;
        }
        let now = if connected { rtp_stats() } else { None };
        if let (Some(before), Some(after)) = (w.rtp, now) {
            let (loss, recovered) = loss_in_window(&before, &after);
            st.loss_pct = loss;
            st.fec_recovered = recovered;
        }
        *w = Window {
            since: Some(Instant::now()),
            rtp: now,
            ..Default::default()
        };
    }
}

fn rtp_stats() -> Option<ffi::RtpVideoStats> {
    let p = unsafe { ffi::LiGetRTPVideoStats() };
    if p.is_null() {
        None
    } else {
        Some(unsafe { *p })
    }
}

/// Loss as a percentage of the video packets seen between two readings of
/// the RTP counters, and how many packets FEC saved in that time.
fn loss_in_window(before: &ffi::RtpVideoStats, after: &ffi::RtpVideoStats) -> (f32, u32) {
    let video = after
        .packet_count_video
        .saturating_sub(before.packet_count_video);
    let failed = after
        .packet_count_fec_failed
        .saturating_sub(before.packet_count_fec_failed);
    let recovered = after
        .packet_count_fec_recovered
        .saturating_sub(before.packet_count_fec_recovered);
    let seen = video + failed;
    let loss = if seen == 0 {
        0.0
    } else {
        failed as f32 * 100.0 / seen as f32
    };
    (loss, recovered)
}

unsafe extern "C" fn audio_setup(
    p: *mut c_void,
    sample_rate: c_int,
    channels: c_int,
    streams: c_int,
    coupled: c_int,
    samples_per_frame: c_int,
    mapping: *const u8,
) -> c_int {
    let inner = ctx(p);
    let mapping = std::slice::from_raw_parts(mapping, channels.max(0) as usize);
    match Player::new(
        sample_rate as u32,
        channels as usize,
        streams,
        coupled,
        samples_per_frame as usize,
        mapping,
    ) {
        Ok(pl) => {
            *inner.audio.lock() = Some(pl);
            0
        }
        Err(e) => {
            // Streaming without sound beats not streaming.
            tracing::warn!("audio: {e:#}");
            0
        }
    }
}

unsafe extern "C" fn audio_cleanup(p: *mut c_void) {
    *ctx(p).audio.lock() = None;
}

unsafe extern "C" fn audio_packet(p: *mut c_void, data: *const u8, len: c_int) {
    let inner = ctx(p);
    let packet: &[u8] = if data.is_null() || len <= 0 {
        &[]
    } else {
        std::slice::from_raw_parts(data, len as usize)
    };
    if let Some(a) = inner.audio.lock().as_mut() {
        a.push(packet);
    }
}

unsafe extern "C" fn stage(p: *mut c_void, stage: c_int, state: c_int, error: c_int) {
    let inner = ctx(p);
    let name = ffi::stage_name(stage);
    match state {
        0 => inner.emit(Event::Stage(name)),
        2 => {
            inner.emit(Event::Failed {
                stage: name,
                code: error,
            });
            inner.finish();
        }
        _ => {}
    }
}

unsafe extern "C" fn connected(p: *mut c_void) {
    let inner = ctx(p);
    inner.connected.store(true, Ordering::Release);
    inner.emit(Event::Connected);
}

unsafe extern "C" fn terminated(p: *mut c_void, code: c_int) {
    let inner = ctx(p);
    inner.connected.store(false, Ordering::Release);
    inner.emit(Event::Terminated {
        code,
        message: termination_message(code),
    });
    inner.finish();
}

unsafe extern "C" fn status(p: *mut c_void, status: c_int) {
    ctx(p).emit(Event::Poor(status == ffi::CONN_STATUS_POOR));
}

unsafe extern "C" fn log(_p: *mut c_void, line: *const c_char) {
    if !line.is_null() {
        tracing::debug!("moonlight: {}", CStr::from_ptr(line).to_string_lossy());
    }
}

pub fn termination_message(code: i32) -> String {
    match code {
        ffi::ML_ERROR_GRACEFUL_TERMINATION => "The PC ended the session.".into(),
        ffi::ML_ERROR_NO_VIDEO_TRAFFIC => {
            "No video arrived. UDP port 47998 to the PC is blocked, or its firewall is.".into()
        }
        ffi::ML_ERROR_NO_VIDEO_FRAME => {
            "Video could not be assembled. The connection is too poor for this bitrate.".into()
        }
        ffi::ML_ERROR_UNEXPECTED_EARLY_TERMINATION => {
            "The PC ended the session as soon as it started.".into()
        }
        ffi::ML_ERROR_PROTECTED_CONTENT => "Protected content is on the PC's screen.".into(),
        ffi::ML_ERROR_FRAME_CONVERSION => {
            "The PC could not convert frames (HDR or resolution mismatch).".into()
        }
        n => format!("Connection lost (code {n})."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_typed_in_small_pieces_between_characters() {
        let short = "powershell";
        assert_eq!(text_chunks(short), vec![short]);
        let long = "é".repeat(40); // 80 bytes
        let chunks = text_chunks(&long);
        assert!(chunks.iter().all(|c| c.len() <= TEXT_CHUNK), "{chunks:?}");
        assert!(chunks.iter().all(|c| c.chars().all(|ch| ch == 'é')));
        assert_eq!(chunks.concat(), long);
        assert!(text_chunks("").is_empty());
    }

    #[test]
    fn loss_is_the_share_of_packets_fec_could_not_save() {
        let before = ffi::RtpVideoStats {
            packet_count_video: 1000,
            packet_count_fec_failed: 10,
            packet_count_fec_recovered: 5,
            ..Default::default()
        };
        let after = ffi::RtpVideoStats {
            packet_count_video: 1900,
            packet_count_fec_failed: 110,
            packet_count_fec_recovered: 25,
            ..Default::default()
        };
        let (loss, recovered) = loss_in_window(&before, &after);
        assert!((loss - 10.0).abs() < 0.01, "{loss}");
        assert_eq!(recovered, 20);
        assert_eq!(loss_in_window(&after, &after), (0.0, 0));
        // Counters reset (a new session): never negative, never a panic.
        assert_eq!(loss_in_window(&after, &before), (0.0, 0));
    }

    #[test]
    fn stage_names_and_messages_are_readable() {
        assert!(!ffi::stage_name(4).is_empty());
        assert!(termination_message(0).contains("ended"));
        assert!(termination_message(-100).contains("47998"));
        assert!(termination_message(-7).contains("-7"));
        assert!(!ffi::launch_query().is_empty());
    }
}

/// Stream from the Sunshine on this machine for a few seconds:
/// `cargo test -p brolink-stream stream_real -- --ignored --nocapture`
/// (run `pair_real` first).
#[cfg(test)]
mod real {
    use super::*;
    use crate::nvhttp::Client;
    use std::time::Duration;

    #[test]
    #[ignore = "needs a paired Sunshine on this machine"]
    fn stream_real() {
        let dir = std::env::temp_dir().join("brolink-pair-test");
        let identity = crate::Identity::load_or_create(&dir).unwrap();
        let ip: std::net::IpAddr = "127.0.0.1".parse().unwrap();
        let cert = std::fs::read(dir.join("server.der")).ok();
        let mut client = Client::new(&identity, ip, cert).unwrap();
        let mut info = client.server_info().unwrap();
        if !info.paired {
            let der = client
                .pair("4321", "brolink-test", || {
                    let _ = brolink_core::http::post_json::<_, brolink_core::api::Ack>(
                        ("127.0.0.1", brolink_core::CONTROL_PORT),
                        "/v1/pin",
                        &brolink_core::api::PinRequest {
                            pin: "4321".into(),
                            name: "brolink-test".into(),
                        },
                        Duration::from_secs(10),
                    );
                })
                .unwrap();
            std::fs::write(dir.join("server.der"), &der).unwrap();
            info = client.server_info().unwrap();
        }
        eprintln!("{info:?}");
        let apps = client.app_list().unwrap();
        let desktop = apps.iter().find(|a| a.title == "Desktop").unwrap();
        let ri_key: [u8; 16] = rand::random();
        let ri_id: u32 = rand::random();
        let mut ri_iv = [0u8; 16];
        ri_iv[..4].copy_from_slice(&ri_id.to_be_bytes());
        let resume = info.current_game != 0;
        let rtsp = client
            .launch(desktop.id, 1280, 720, 60, &ri_key, ri_id, resume)
            .unwrap();
        eprintln!("rtsp: {rtsp} (resume={resume})");
        let frames = Arc::new(FrameSlot::default());
        let (tx, rx) = std::sync::mpsc::channel();
        let session = Session::start(
            Server {
                address: ip.to_string(),
                app_version: info.app_version.clone(),
                gfe_version: info.gfe_version.clone(),
                rtsp_url: rtsp,
                codec_mode_support: info.codec_mode_support,
            },
            Settings {
                width: 1280,
                height: 720,
                fps: 60,
                bitrate_kbps: 10_000,
                hevc: false,
                remote: false,
            },
            ri_key,
            ri_iv,
            frames.clone(),
            tx,
            || {},
        );
        let start = Instant::now();
        let mut connected = false;
        while start.elapsed() < Duration::from_secs(12) {
            while let Ok(ev) = rx.try_recv() {
                eprintln!("{:>6.2}s {ev:?}", start.elapsed().as_secs_f32());
                if ev == Event::Connected {
                    connected = true;
                }
                if matches!(ev, Event::Failed { .. } | Event::Terminated { .. }) {
                    panic!("session ended early: {ev:?}");
                }
            }
            if let Some(f) = frames.take() {
                if frames.seq() % 60 == 1 {
                    eprintln!(
                        "frame {}x{} stride {} seq {}",
                        f.width,
                        f.height,
                        f.y_stride,
                        frames.seq()
                    );
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let stats = session.stats();
        eprintln!("stats: {stats:?}, frames published: {}", frames.seq());
        assert!(connected, "never connected");
        assert!(frames.seq() > 60, "too few frames: {}", frames.seq());
        session.stop();
        let t = Instant::now();
        while !session.finished() && t.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(session.finished(), "session did not finish");
        eprintln!("stopped in {:?}", t.elapsed());
    }
}
