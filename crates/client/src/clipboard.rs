//! The clipboard both ways while a stream runs, through BroLink Host's
//! `/v1/clipboard` on the PC.
//!
//! The Moonlight protocol carries keys and pictures, not clipboards, so ⌘V
//! on the PC used to paste whatever the PC last copied, and text copied on
//! the PC never reached the Mac. Now a worker polls the PC's clipboard
//! every second or so and hands new text to the window, which puts it in
//! the Mac's clipboard; and a paste first sends the Mac's text to the PC,
//! then presses Ctrl+V there. Hosts before 3.1 have no such route; the
//! worker notices the 404 and says so once.

use brolink_core::api::{Ack, Clipboard, CLIPBOARD_PATH};
use brolink_core::{http, CONTROL_PORT};
use parking_lot::Mutex;
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(1500);
/// After a ⌘C on the PC, Windows needs a moment before the clipboard has
/// the new text.
const POKE_DELAY: Duration = Duration::from_millis(400);
const TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Shared {
    /// Text the PC copied that the window has not taken yet.
    incoming: Option<String>,
    /// The last text either side saw, so nothing echoes back and forth.
    last: String,
    last_seq: Option<u64>,
    /// The host answered 404: older than 3.1.
    unsupported: bool,
    /// Something the window can show about the last exchange.
    note: Option<(String, Instant)>,
    pokes: u32,
}

pub struct Sync {
    ip: Ipv4Addr,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    /// Pastes whose chord has been sent, so the window can put a modifier
    /// the user is still holding back down afterwards.
    done: Arc<AtomicU32>,
}

impl Sync {
    /// Start polling the PC at `ip`; `ctx` is repainted when text arrives.
    pub fn spawn(ip: Ipv4Addr, ctx: egui::Context) -> Self {
        let shared: Arc<Mutex<Shared>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        {
            let shared = shared.clone();
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("clipboard".into())
                .spawn(move || poll_loop(ip, shared, stop, ctx))
                .expect("spawn clipboard thread");
        }
        Self {
            ip,
            shared,
            stop,
            done: Arc::default(),
        }
    }

    /// How many pastes have had their chord sent. The chord releases the
    /// modifier it pressed; if ⌘ is still physically down when this moves,
    /// the window presses the modifier again.
    pub fn pastes_done(&self) -> u32 {
        self.done.load(Ordering::Acquire)
    }

    /// Text copied on the PC since the last call, once.
    pub fn take_incoming(&self) -> Option<String> {
        self.shared.lock().incoming.take()
    }

    pub fn unsupported(&self) -> bool {
        self.shared.lock().unsupported
    }

    /// A short line for the toolbar, while it is fresh.
    pub fn note(&self) -> Option<String> {
        let sh = self.shared.lock();
        sh.note
            .as_ref()
            .filter(|(_, at)| at.elapsed() < Duration::from_secs(4))
            .map(|(t, _)| t.clone())
    }

    /// The PC's clipboard probably just changed (a ⌘C was sent): read it
    /// sooner than the next poll.
    pub fn poke(&self) {
        self.shared.lock().pokes += 1;
    }

    /// Put `text` in the PC's clipboard, then run `then` (the paste chord)
    /// on the worker thread. If the PC already has this text, `then` runs
    /// at once; if the host cannot take it, `then` still runs, so the PC
    /// pastes whatever it has rather than nothing.
    pub fn push(&self, text: String, then: impl FnOnce() + Send + 'static) {
        let (same, unsupported) = {
            let sh = self.shared.lock();
            (sh.last == text, sh.unsupported)
        };
        let done = self.done.clone();
        if same || unsupported || text.is_empty() {
            then();
            done.fetch_add(1, Ordering::AcqRel);
            return;
        }
        let shared = self.shared.clone();
        let ip = self.ip;
        std::thread::spawn(move || {
            match send(ip, &text) {
                Ok(()) => {
                    let mut sh = shared.lock();
                    sh.last = text;
                    sh.note = Some(("Clipboard sent to the PC".into(), Instant::now()));
                }
                Err(Fail::Unsupported) => shared.lock().unsupported = true,
                Err(Fail::Other(e)) => {
                    tracing::warn!("clipboard to PC: {e}");
                    shared.lock().note = Some((format!("Clipboard not sent: {e}"), Instant::now()));
                }
            }
            then();
            done.fetch_add(1, Ordering::AcqRel);
        });
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

enum Fail {
    Unsupported,
    Other(String),
}

fn send(ip: Ipv4Addr, text: &str) -> Result<(), Fail> {
    let body =
        serde_json::to_string(&Clipboard::fit(text, 0)).map_err(|e| Fail::Other(e.to_string()))?;
    let r = http::request(
        (ip, CONTROL_PORT),
        "POST",
        CLIPBOARD_PATH,
        Some(&body),
        TIMEOUT,
    )
    .map_err(|e| Fail::Other(e.to_string()))?;
    match r.status {
        200 => Ok(()),
        404 => Err(Fail::Unsupported),
        _ => Err(Fail::Other(
            r.parse::<Ack>()
                .ok()
                .and_then(|a| a.error)
                .unwrap_or_else(|| format!("HTTP {}", r.status)),
        )),
    }
}

fn fetch(ip: Ipv4Addr) -> Result<Clipboard, Fail> {
    let r = http::request((ip, CONTROL_PORT), "GET", CLIPBOARD_PATH, None, TIMEOUT)
        .map_err(|e| Fail::Other(e.to_string()))?;
    match r.status {
        200 => r
            .parse::<Clipboard>()
            .map_err(|e| Fail::Other(e.to_string())),
        404 => Err(Fail::Unsupported),
        _ => Err(Fail::Other(format!("HTTP {}", r.status))),
    }
}

fn poll_loop(ip: Ipv4Addr, shared: Arc<Mutex<Shared>>, stop: Arc<AtomicBool>, ctx: egui::Context) {
    let mut failures = 0u32;
    let mut pokes_seen = 0u32;
    while !stop.load(Ordering::Relaxed) {
        // Sleep in small steps so a poke or a stop is noticed quickly.
        let mut waited = Duration::ZERO;
        while waited < POLL && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(100));
            waited += Duration::from_millis(100);
            if shared.lock().pokes != pokes_seen {
                std::thread::sleep(POKE_DELAY);
                break;
            }
        }
        if stop.load(Ordering::Relaxed) {
            break;
        }
        pokes_seen = shared.lock().pokes;
        match fetch(ip) {
            Ok(c) => {
                failures = 0;
                let mut sh = shared.lock();
                if sh.last_seq != Some(c.seq) {
                    sh.last_seq = Some(c.seq);
                    if !c.text.is_empty() && c.text != sh.last {
                        sh.last = c.text.clone();
                        sh.incoming = Some(c.text);
                        sh.note = Some((
                            if c.truncated {
                                "Copied from the PC (cut at 32 KB)".to_string()
                            } else {
                                "Copied from the PC".to_string()
                            },
                            Instant::now(),
                        ));
                        drop(sh);
                        ctx.request_repaint();
                    }
                }
            }
            Err(Fail::Unsupported) => {
                shared.lock().unsupported = true;
                tracing::info!("clipboard: the host has no /v1/clipboard; sync is off");
                return;
            }
            Err(Fail::Other(e)) => {
                failures += 1;
                if failures == 3 {
                    tracing::warn!("clipboard from PC: {e}");
                }
                // Back off while the host is unreachable.
                std::thread::sleep(POLL * failures.min(10));
            }
        }
    }
}

/// Whether the window should offer a paste at all: `Event::Paste` carries
/// text only when the Mac's clipboard holds some.
pub fn worth_sending(text: &str) -> bool {
    !text.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_push_of_the_same_text_runs_the_paste_at_once() {
        let s = Sync {
            ip: Ipv4Addr::LOCALHOST,
            shared: Arc::default(),
            stop: Arc::new(AtomicBool::new(true)),
            done: Arc::default(),
        };
        s.shared.lock().last = "hello".into();
        let ran = Arc::new(AtomicBool::new(false));
        let r = ran.clone();
        assert_eq!(s.pastes_done(), 0);
        s.push("hello".into(), move || r.store(true, Ordering::Relaxed));
        assert!(ran.load(Ordering::Relaxed));
        assert_eq!(s.pastes_done(), 1);
        assert!(worth_sending("x"));
        assert!(!worth_sending("  \n"));
        assert!(s.take_incoming().is_none());
        s.shared.lock().incoming = Some("from pc".into());
        assert_eq!(s.take_incoming().as_deref(), Some("from pc"));
        assert!(s.take_incoming().is_none());
    }
}
