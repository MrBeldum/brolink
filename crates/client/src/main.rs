mod app;
mod audio;
mod decode;
mod input_map;
mod session;

use anyhow::Result;
use audio::AudioStats;
use clap::Parser;
use decode::VideoSink;
use forgelink_core::config::ClientConfig;
use forgelink_core::identity::Identity;
use session::{ClientCmd, ClientEvent, ConnectRequest};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing_subscriber::EnvFilter;

/// Frames the loopback test wants to see before it calls the pipeline healthy.
const HEADLESS_FRAMES: u64 = 30;
/// Connect, negotiate, probe the encoder and stream 30 frames inside this.
const HEADLESS_TIMEOUT: Duration = Duration::from_secs(60);
/// 100 ms of audio actually handed to the output device. Enough to clear the
/// 40 ms prebuffer and prove the stream keeps flowing, not just starts.
const HEADLESS_AUDIO_FRAMES: u64 = 4_800;
/// Any real signal clears this easily; a stream of silent buffers never will.
const HEADLESS_AUDIO_PEAK: u64 = 64;

#[derive(Parser, Debug)]
#[command(
    name = "forgelink-client",
    version,
    about = "ForgeLink client — play your Windows PC from a Mac"
)]
struct Args {
    /// Ticket, IP, or IP:port to connect to immediately.
    #[arg(long)]
    connect: Option<String>,
    /// No GUI; connect, verify frames arrive, then exit (for tests).
    #[arg(long)]
    headless: bool,
    /// Override the saved output volume (0.0–2.0).
    #[arg(long)]
    volume: Option<f32>,
    /// Headless: also require audio to reach the output device, not just video.
    /// Needs something playing on the host — WASAPI loopback has nothing to
    /// capture on a silent desktop.
    #[arg(long)]
    require_audio: bool,
}

fn main() -> Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    let args = Args::parse();
    let mut cfg = ClientConfig::load().unwrap_or_default();
    if let Some(v) = args.volume {
        cfg.volume = v.clamp(0.0, 2.0);
    }
    let auto = args.connect.clone();
    if let Some(c) = args.connect {
        cfg.last_ticket = c;
    }
    cfg.quality = cfg.quality.sanitized();
    let identity = Identity::load_or_create(&forgelink_core::config::client_identity_path()?)?;
    tracing::info!("client id {}", identity.short_id());

    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let (ev_tx, ev_rx) = tokio::sync::mpsc::unbounded_channel();
    // Decoded video is handed over through a shared latest-frame slot rather
    // than the event channel, so a slow UI cannot build a backlog of frames.
    let video = Arc::new(VideoSink::default());
    let audio_stats = Arc::new(AudioStats::default());
    session::spawn(cmd_rx, ev_tx, video.clone(), audio_stats.clone());

    if args.headless {
        return run_headless(
            Headless {
                cmd_tx,
                ev_rx,
                video,
                audio: audio_stats,
                require_audio: args.require_audio,
            },
            cfg,
            identity,
            auto,
        );
    }

    let native = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([640.0, 400.0])
            .with_title("ForgeLink"),
        vsync: false,
        ..Default::default()
    };
    eframe::run_native(
        "ForgeLink",
        native,
        Box::new(move |cc| {
            let mut app = app::ClientApp::new(cc, cfg, identity, cmd_tx, ev_rx, video);
            if auto.is_some() {
                app.auto_connect_if_target();
            }
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

/// The channels and sinks the headless run reads its verdict from.
struct Headless {
    cmd_tx: tokio::sync::mpsc::UnboundedSender<ClientCmd>,
    ev_rx: tokio::sync::mpsc::UnboundedReceiver<ClientEvent>,
    video: Arc<VideoSink>,
    audio: Arc<AudioStats>,
    require_audio: bool,
}

/// Connect and confirm decoded frames actually arrive, then exit 0.
fn run_headless(
    h: Headless,
    cfg: ClientConfig,
    identity: Identity,
    auto: Option<String>,
) -> Result<()> {
    let Headless {
        cmd_tx,
        mut ev_rx,
        video,
        audio,
        require_audio,
    } = h;
    let target = auto.unwrap_or_else(|| cfg.last_ticket.clone());
    if target.trim().is_empty() {
        anyhow::bail!("--headless requires --connect or a saved ticket");
    }
    cmd_tx.send(ClientCmd::Connect(Box::new(ConnectRequest {
        target,
        cfg,
        identity,
    })))?;

    let deadline = Instant::now() + HEADLESS_TIMEOUT;
    let mut announced_first = false;
    loop {
        // The sink is the source of truth for "did a frame decode": it counts
        // pictures whether or not anything ever displays them.
        let frames = video.frame_count();
        if !announced_first && frames > 0 {
            announced_first = true;
            tracing::info!("first video frame received");
        }
        let audio_ok = !require_audio
            || (audio.frames_out() >= HEADLESS_AUDIO_FRAMES && audio.peak() >= HEADLESS_AUDIO_PEAK);
        if frames >= HEADLESS_FRAMES && audio_ok {
            tracing::info!("received {frames} video frames — pipeline OK");
            if require_audio {
                tracing::info!(
                    "played {} audio frames from {} packets, peak {} — audio OK",
                    audio.frames_out(),
                    audio.packets(),
                    audio.peak()
                );
            }
            let _ = cmd_tx.send(ClientCmd::Disconnect);
            std::thread::sleep(Duration::from_millis(200));
            return Ok(());
        }
        if Instant::now() > deadline {
            if frames >= HEADLESS_FRAMES {
                anyhow::bail!("headless: video OK but {}", audio_diagnosis(&audio));
            }
            anyhow::bail!("headless: timed out after {HEADLESS_TIMEOUT:?} (frames={frames})");
        }
        // Drop whatever the UI would have shown so buffers get recycled.
        if let Some(pic) = video.take() {
            video.recycle(pic.rgba);
        }
        match ev_rx.try_recv() {
            Ok(ClientEvent::Log(s)) => tracing::info!("{s}"),
            Ok(ClientEvent::Error(e)) => anyhow::bail!(e),
            Ok(ClientEvent::NeedPin { host }) => {
                anyhow::bail!(
                    "'{host}' wants a pairing PIN; run the host with --no-pin for automated tests"
                )
            }
            Ok(ClientEvent::Ready(r)) => tracing::info!(
                "session ready {}x{} @ {} fps via {}",
                r.width,
                r.height,
                r.fps,
                r.encoder
            ),
            Ok(ClientEvent::Stats {
                fps,
                bitrate_kbps,
                rtt_ms,
                loss,
                ..
            }) => tracing::info!(
                "stats fps={fps:.1} bitrate={bitrate_kbps:.0}kbps rtt={rtt_ms:.1}ms \
                 loss={loss:.1}% frames={frames}"
            ),
            Ok(ClientEvent::Disconnected) => anyhow::bail!("host disconnected (frames={frames})"),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                anyhow::bail!("session thread ended")
            }
        }
    }
}

/// Say *where* the audio path stopped, not just that it did. The three stages
/// fail for entirely different reasons and the fix differs each time.
fn audio_diagnosis(a: &AudioStats) -> String {
    let (packets, frames_in, frames_out) = (a.packets(), a.frames_in(), a.frames_out());
    if packets == 0 {
        return "no audio packets arrived; is the host running with audio enabled, and is anything actually playing on it?"
            .into();
    }
    if frames_in == 0 {
        return format!(
            "{packets} audio packets arrived but none were buffered; no output device opened on this machine"
        );
    }
    if frames_out == 0 {
        return format!(
            "{frames_in} audio frames were buffered from {packets} packets but none reached the device; the output stream is not running"
        );
    }
    if frames_out < HEADLESS_AUDIO_FRAMES {
        return format!(
            "only {frames_out} of {HEADLESS_AUDIO_FRAMES} audio frames played ({packets} packets, {frames_in} buffered); the stream is starving"
        );
    }
    format!(
        "{frames_out} audio frames played but every sample was silence (peak {}, need {HEADLESS_AUDIO_PEAK}); the pipeline runs but nothing audible was captured. Is anything playing on the host?",
        a.peak()
    )
}
