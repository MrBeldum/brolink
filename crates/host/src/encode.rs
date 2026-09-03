//! Desktop capture + H.264 encode.
//!
//! FFmpeg `ddagrab` (Desktop Duplication) feeds a hardware encoder — AMF,
//! NVENC, QSV, or Media Foundation — with a libx264 `ultrafast` fallback, and
//! `gdigrab` behind that for machines where Desktop Duplication is unavailable.

use anyhow::{anyhow, Context, Result};
use brolink_core::codec::{AnnexBSplitter, EncodedFrame};
use brolink_core::config::{StreamQuality, MAX_BITRATE_KBPS, MIN_BITRATE_KBPS};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A probe that has not finished by now is not a viable low-latency encoder.
const PROBE_TIMEOUT: Duration = Duration::from_secs(12);
/// How much test output a probe must produce before we believe it.
const PROBE_SECONDS: &str = "0.3";

#[derive(Debug, Clone)]
pub struct EncoderInfo {
    pub name: String,
    pub ffmpeg: PathBuf,
    args: Vec<String>,
}

impl EncoderInfo {
    /// Rebuild the command line at a new CBR without re-probing the GPU.
    pub fn with_bitrate(&self, kbps: u32) -> Self {
        let kbps = kbps.clamp(MIN_BITRATE_KBPS, MAX_BITRATE_KBPS);
        let mut args = self.args.clone();
        set_flag(&mut args, "-b:v", format!("{kbps}k"));
        set_flag(&mut args, "-maxrate", format!("{kbps}k"));
        set_flag(&mut args, "-bufsize", format!("{}k", (kbps / 4).max(1000)));
        Self {
            name: self.name.clone(),
            ffmpeg: self.ffmpeg.clone(),
            args,
        }
    }
}

fn set_flag(args: &mut Vec<String>, flag: &str, val: String) {
    if let Some(i) = args.iter().position(|a| a == flag) {
        if i + 1 < args.len() {
            args[i + 1] = val;
            return;
        }
    }
    args.push(flag.into());
    args.push(val);
}

pub struct VideoPipeline {
    child: Option<Child>,
    reader: Option<JoinHandle<()>>,
    stderr_reader: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    pub info: EncoderInfo,
}

pub fn find_ffmpeg(configured: &str) -> Option<PathBuf> {
    // An explicitly configured path wins, and is worth complaining about when
    // it is wrong rather than silently falling back.
    let configured = configured.trim();
    if !configured.is_empty() {
        let p = PathBuf::from(configured);
        if p.exists() || runs(&p) {
            return Some(p);
        }
        tracing::warn!("configured ffmpeg_path '{configured}' is not usable; searching instead");
    }
    // Next to our own executable, so a bundled ffmpeg.exe is preferred over a
    // random one on PATH.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for name in ["ffmpeg.exe", "ffmpeg"] {
                let p = dir.join(name);
                if p.exists() {
                    return Some(p);
                }
            }
        }
    }
    for c in [
        "ffmpeg",
        "ffmpeg.exe",
        r"C:\ffmpeg\bin\ffmpeg.exe",
        r"C:\ProgramData\chocolatey\bin\ffmpeg.exe",
    ] {
        let p = PathBuf::from(c);
        if p.exists() || runs(&p) {
            return Some(p);
        }
    }
    None
}

fn runs(path: &Path) -> bool {
    ffmpeg_cmd(path)
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn ffmpeg_cmd(path: &Path) -> Command {
    let mut c = Command::new(path);
    #[cfg(windows)]
    {
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

fn null_sink() -> &'static str {
    if cfg!(windows) {
        "NUL"
    } else {
        "/dev/null"
    }
}

/// Run a candidate briefly and see whether it produces a stream.
fn probe(ffmpeg: &Path, extra: &[String]) -> bool {
    let mut args = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-nostdin".into(),
    ];
    args.extend(extra.iter().cloned());
    args.extend([
        "-t".into(),
        PROBE_SECONDS.into(),
        "-an".into(),
        "-f".into(),
        "h264".into(),
        "-y".into(),
        null_sink().into(),
    ]);
    tracing::debug!(?args, "probing encoder");

    let mut child = match ffmpeg_cmd(ffmpeg)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!("probe spawn failed: {e}");
            return false;
        }
    };
    // A wedged encoder (a driver waiting on a device that never answers) would
    // otherwise hang the probe, and with it the whole connection attempt.
    let deadline = Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    if let Some(mut err) = child.stderr.take() {
                        let mut s = String::new();
                        let _ = err.read_to_string(&mut s);
                        let s = s.trim();
                        if !s.is_empty() {
                            tracing::debug!("probe failed: {s}");
                        }
                    }
                }
                return status.success();
            }
            Ok(None) => {
                if Instant::now() > deadline {
                    tracing::debug!("probe timed out after {PROBE_TIMEOUT:?}");
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                tracing::debug!("probe wait failed: {e}");
                let _ = child.kill();
                return false;
            }
        }
    }
}

fn capture_filter(monitor: u32, fps: u32) -> String {
    format!("ddagrab=output_idx={monitor}:framerate={fps}:draw_mouse=1,hwdownload,format=bgra")
}

/// Input options shared by every Desktop Duplication candidate.
fn ddagrab_prefix(monitor: u32, q: &StreamQuality) -> Vec<String> {
    vec![
        "-fflags".into(),
        "nobuffer+flush_packets+genpts".into(),
        "-flags".into(),
        "low_delay".into(),
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        capture_filter(monitor, q.fps),
        "-vf".into(),
        scale_filter(q),
        "-pix_fmt".into(),
        "nv12".into(),
    ]
}

fn gdigrab_prefix(q: &StreamQuality) -> Vec<String> {
    vec![
        "-f".into(),
        "gdigrab".into(),
        "-framerate".into(),
        q.fps.to_string(),
        "-i".into(),
        "desktop".into(),
        "-vf".into(),
        scale_filter(q),
        // gdigrab hands us BGRA. Without an explicit 4:2:0 format libx264 picks
        // yuv444p, which is High 4:4:4 Predictive — a profile the client's
        // OpenH264 decoder cannot decode at all.
        "-pix_fmt".into(),
        "nv12".into(),
    ]
}

/// Fit the desktop into the requested resolution without distorting it, and
/// keep dimensions even for 4:2:0.
fn scale_filter(q: &StreamQuality) -> String {
    format!(
        "scale={w}:{h}:force_original_aspect_ratio=decrease:flags=bilinear,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,format=nv12",
        w = q.width,
        h = q.height
    )
}

fn bitrate_args(kbps: u32, fps: u32) -> Vec<String> {
    // A one-second GOP bounds how long a lost frame can corrupt the picture.
    let g = fps.max(30);
    let buf = (kbps / 4).max(1000);
    vec![
        "-b:v".into(),
        format!("{kbps}k"),
        "-maxrate".into(),
        format!("{kbps}k"),
        "-bufsize".into(),
        format!("{buf}k"),
        // No B-frames: they reorder output and add a frame of latency.
        "-bf".into(),
        "0".into(),
        "-g".into(),
        g.to_string(),
        "-keyint_min".into(),
        (g / 2).max(15).to_string(),
    ]
}

fn candidates(q: &StreamQuality, monitor: u32, prefer: &str) -> Vec<(String, Vec<String>)> {
    let br = bitrate_args(q.bitrate_kbps, q.fps);
    let dda = || ddagrab_prefix(monitor, q);

    let amf_args = |mut a: Vec<String>| {
        a.extend([
            "-c:v".into(),
            "h264_amf".into(),
            "-usage".into(),
            "ultralowlatency".into(),
            "-quality".into(),
            "speed".into(),
            "-rc".into(),
            "cbr".into(),
        ]);
        a.extend(br.clone());
        a
    };
    // The stream splitter emits an access unit as soon as it sees a slice, which
    // is what keeps latency at one frame. That requires one slice per picture,
    // so every candidate asks for exactly that.
    let x264_args = |mut a: Vec<String>| {
        a.extend([
            "-c:v".into(),
            "libx264".into(),
            "-preset".into(),
            "ultrafast".into(),
            "-tune".into(),
            "zerolatency".into(),
            "-x264-params".into(),
            "sliced-threads=0:slices=1".into(),
        ]);
        a.extend(br.clone());
        a
    };

    let mut ordered: Vec<(String, Vec<String>)> = vec![
        ("h264_amf".into(), amf_args(dda())),
        ("h264_nvenc".into(), {
            let mut a = dda();
            a.extend([
                "-c:v".into(),
                "h264_nvenc".into(),
                "-preset".into(),
                "p1".into(),
                "-tune".into(),
                "ull".into(),
                "-rc".into(),
                "cbr".into(),
                "-delay".into(),
                "0".into(),
                "-zerolatency".into(),
                "1".into(),
                "-rc-lookahead".into(),
                "0".into(),
                "-slices".into(),
                "1".into(),
            ]);
            a.extend(br.clone());
            a
        }),
        ("h264_qsv".into(), {
            let mut a = dda();
            a.extend([
                "-c:v".into(),
                "h264_qsv".into(),
                "-preset".into(),
                "veryfast".into(),
                "-look_ahead".into(),
                "0".into(),
                "-async_depth".into(),
                "1".into(),
            ]);
            a.extend(br.clone());
            a
        }),
        ("h264_mf".into(), {
            let mut a = dda();
            a.extend([
                "-c:v".into(),
                "h264_mf".into(),
                "-rate_control".into(),
                "cbr".into(),
            ]);
            a.extend(br.clone());
            a
        }),
        ("libx264".into(), x264_args(dda())),
        ("h264_amf+gdigrab".into(), amf_args(gdigrab_prefix(q))),
        ("libx264+gdigrab".into(), x264_args(gdigrab_prefix(q))),
    ];

    let prefer = prefer.trim().to_ascii_lowercase();
    if !prefer.is_empty() && prefer != "auto" {
        // Stable sort: the preferred family moves to the front, everything else
        // keeps its fallback order.
        ordered.sort_by_key(|(name, _)| !name.contains(&prefer));
    }
    ordered
}

pub fn select_encoder(
    ffmpeg: &Path,
    q: &StreamQuality,
    monitor: u32,
    prefer: &str,
) -> Result<EncoderInfo> {
    let mut tried = Vec::new();
    for (name, args) in candidates(q, monitor, prefer) {
        tracing::info!("probing encoder {name}");
        if probe(ffmpeg, &args) {
            tracing::info!("selected encoder {name}");
            return Ok(EncoderInfo {
                name,
                ffmpeg: ffmpeg.to_path_buf(),
                args,
            });
        }
        tried.push(name);
    }
    Err(anyhow!(
        "no working H.264 encoder found (tried {}). Install an FFmpeg build with AMF, NVENC, QSV, \
         or libx264, and make sure the Windows session is unlocked so Desktop Duplication can run.",
        tried.join(", ")
    ))
}

impl VideoPipeline {
    pub fn start(info: EncoderInfo, tx: crossbeam_channel::Sender<EncodedFrame>) -> Result<Self> {
        let mut args = info.args.clone();
        args.extend([
            "-an".into(),
            "-f".into(),
            "h264".into(),
            "-flush_packets".into(),
            "1".into(),
            "pipe:1".into(),
        ]);
        tracing::info!(encoder = %info.name, "starting ffmpeg {:?}", args);

        let mut child = ffmpeg_cmd(&info.ffmpeg)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("spawn {}", info.ffmpeg.display()))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("ffmpeg stdout"))?;
        let stderr_reader = child.stderr.take().map(|mut stderr| {
            std::thread::Builder::new()
                .name("ffmpeg-stderr".into())
                .spawn(move || {
                    // Stream it line by line: buffering to EOF would hide the
                    // error that explains why a stream just died.
                    let mut buf = String::new();
                    let mut chunk = [0u8; 1024];
                    while let Ok(n) = stderr.read(&mut chunk) {
                        if n == 0 {
                            break;
                        }
                        buf.push_str(&String::from_utf8_lossy(&chunk[..n]));
                        while let Some(nl) = buf.find('\n') {
                            let line: String = buf.drain(..=nl).collect();
                            let line = line.trim();
                            if !line.is_empty() {
                                tracing::warn!("ffmpeg: {line}");
                            }
                        }
                    }
                    let rest = buf.trim();
                    if !rest.is_empty() {
                        tracing::warn!("ffmpeg: {rest}");
                    }
                })
                .ok()
        });

        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let reader = std::thread::Builder::new()
            .name("ffmpeg-stdout".into())
            .spawn(move || read_loop(stdout, tx, stop2))?;

        Ok(Self {
            child: Some(child),
            reader: Some(reader),
            stderr_reader: stderr_reader.flatten(),
            stop,
            info,
        })
    }

    pub fn stop(&mut self) {
        if self.child.is_some() {
            tracing::info!("stopping the {} pipeline", self.info.name);
        }
        self.stop.store(true, Ordering::Relaxed);
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        // Killing ffmpeg closes both pipes, so these threads finish on their own.
        if let Some(h) = self.reader.take() {
            let _ = h.join();
        }
        if let Some(h) = self.stderr_reader.take() {
            let _ = h.join();
        }
    }
}

impl Drop for VideoPipeline {
    fn drop(&mut self) {
        self.stop();
    }
}

fn read_loop<R: Read>(
    mut stdout: R,
    tx: crossbeam_channel::Sender<EncodedFrame>,
    stop: Arc<AtomicBool>,
) {
    let mut splitter = AnnexBSplitter::default();
    let mut buf = vec![0u8; 64 * 1024];
    let mut frames = Vec::new();
    let start = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        match stdout.read(&mut buf) {
            Ok(0) => {
                frames.clear();
                splitter.flush(&mut frames, start.elapsed().as_micros() as u64);
                for f in frames.drain(..) {
                    let _ = tx.send(f);
                }
                break;
            }
            Ok(n) => {
                let ts = start.elapsed().as_micros() as u64;
                frames.clear();
                splitter.push(&buf[..n], &mut frames, ts);
                for f in frames.drain(..) {
                    // A full channel means the network side is behind. Dropping
                    // the oldest frame is the right trade for a live stream:
                    // stale frames are worse than a skipped one.
                    if let Err(crossbeam_channel::TrySendError::Full(f)) = tx.try_send(f) {
                        if tx.send(f).is_err() {
                            return;
                        }
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                tracing::warn!("ffmpeg stdout: {e}");
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brolink_core::config::QualityPreset;

    fn quality() -> StreamQuality {
        StreamQuality::balanced()
    }

    fn arg_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .map(|s| s.as_str())
    }

    #[test]
    fn every_candidate_encodes_to_a_decodable_pixel_format() {
        for (name, args) in candidates(&quality(), 0, "auto") {
            let joined = args.join(" ");
            assert!(
                joined.contains("nv12") || joined.contains("yuv420p"),
                "{name} must produce 4:2:0; OpenH264 cannot decode 4:4:4 ({joined})"
            );
        }
    }

    #[test]
    fn libx264_candidates_ask_for_a_single_slice() {
        // Multi-slice pictures break the zero-latency splitter, and `-tune
        // zerolatency` turns sliced threads on unless we say otherwise.
        for (name, args) in candidates(&quality(), 0, "auto") {
            if !name.contains("libx264") {
                continue;
            }
            let params = arg_after(&args, "-x264-params").unwrap_or("");
            assert!(
                params.contains("sliced-threads=0"),
                "{name} would emit multi-slice frames ({params})"
            );
            assert!(params.contains("slices=1"), "{name} params: {params}");
        }
    }

    #[test]
    fn every_candidate_disables_b_frames_and_uses_a_short_gop() {
        let q = quality();
        for (name, args) in candidates(&q, 0, "auto") {
            assert_eq!(
                arg_after(&args, "-bf"),
                Some("0"),
                "{name} must not use B-frames"
            );
            let g: u32 = arg_after(&args, "-g").unwrap().parse().unwrap();
            assert!(
                g <= q.fps.max(30),
                "{name} GOP {g} is longer than one second"
            );
        }
    }

    #[test]
    fn bitrate_args_are_capped_and_buffered() {
        let a = bitrate_args(25_000, 60);
        assert_eq!(arg_after(&a, "-b:v"), Some("25000k"));
        assert_eq!(arg_after(&a, "-maxrate"), Some("25000k"));
        assert_eq!(arg_after(&a, "-bufsize"), Some("6250k"));
        assert_eq!(arg_after(&a, "-g"), Some("60"));
        // A tiny bitrate still gets a usable buffer rather than 0k.
        let a = bitrate_args(2_000, 30);
        assert_eq!(arg_after(&a, "-bufsize"), Some("1000k"));
    }

    #[test]
    fn preference_reorders_without_dropping_fallbacks() {
        let all = candidates(&quality(), 0, "auto");
        let preferred = candidates(&quality(), 0, "libx264");
        assert_eq!(
            all.len(),
            preferred.len(),
            "preference must not drop candidates"
        );
        assert!(preferred[0].0.contains("libx264"));
        // Everything still present, just reordered.
        let mut a: Vec<&String> = all.iter().map(|(n, _)| n).collect();
        let mut b: Vec<&String> = preferred.iter().map(|(n, _)| n).collect();
        a.sort();
        b.sort();
        assert_eq!(a, b);
        // Unknown preferences fall back to the default order.
        let unknown = candidates(&quality(), 0, "h264_magic");
        assert_eq!(unknown[0].0, all[0].0);
    }

    #[test]
    fn with_bitrate_rewrites_the_rate_flags_and_keeps_the_encoder() {
        let info = EncoderInfo {
            name: "h264_nvenc".into(),
            ffmpeg: PathBuf::from("ffmpeg"),
            args: bitrate_args(25_000, 60),
        };
        let next = info.with_bitrate(10_000);
        assert_eq!(next.name, "h264_nvenc");
        assert_eq!(arg_after(&next.args, "-b:v"), Some("10000k"));
        assert_eq!(arg_after(&next.args, "-maxrate"), Some("10000k"));
        assert_eq!(arg_after(&next.args, "-bufsize"), Some("2500k"));
        assert_eq!(
            arg_after(&info.args, "-b:v"),
            Some("25000k"),
            "original unchanged"
        );
    }

    #[test]
    fn monitor_index_reaches_the_capture_filter() {
        let args = candidates(&quality(), 2, "h264_amf")[0].1.join(" ");
        assert!(args.contains("output_idx=2"), "{args}");
    }

    #[test]
    fn requested_resolution_reaches_the_scaler() {
        let q = StreamQuality::from_preset(QualityPreset::Quality);
        for (name, args) in candidates(&q, 0, "auto") {
            let vf = arg_after(&args, "-vf").unwrap_or("");
            assert!(
                vf.contains("2560:1440"),
                "{name} ignores the requested resolution ({vf})"
            );
            assert!(
                vf.contains("force_original_aspect_ratio=decrease"),
                "{name} would stretch the desktop ({vf})"
            );
        }
    }

    #[test]
    fn find_ffmpeg_rejects_a_path_that_does_not_exist() {
        // A bogus configured path must not be returned as if it worked.
        let found = find_ffmpeg("Z:\\definitely\\not\\here\\ffmpeg.exe");
        assert_ne!(
            found.as_deref(),
            Some(Path::new("Z:\\definitely\\not\\here\\ffmpeg.exe"))
        );
    }
}
