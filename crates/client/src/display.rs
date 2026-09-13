//! Asking a PC why its picture is black, and letting it be fixed from here.
//!
//! When a stream is connected and every frame is black, the one thing the
//! Mac cannot do is look at the PC to find out why. The PC answers instead,
//! over the same control API that can put it to sleep, and the answer is a
//! sentence a person can act on rather than a dump of display settings.

use anyhow::Result;
use brolink_core::api::DisplayRequest;
use brolink_core::{http, CONTROL_PORT};
use serde_json::Value;
use std::net::Ipv4Addr;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(25);

/// What the PC said, in a form the stream's notice can show.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Help {
    /// One sentence naming the cause, empty while nothing is known.
    pub message: String,
    /// The PC's advanced-colour desktop is what breaks the capture, and it
    /// can be turned off from here.
    pub hdr_is_on: bool,
    /// What that mode is called on the PC, for the button that turns it
    /// off: "HDR" or "wide colour".
    pub mode: String,
    /// A fix is on its way to the PC.
    pub busy: bool,
}

pub fn ask(ip: Ipv4Addr) -> Result<Value> {
    http::get_json::<Value>((ip, CONTROL_PORT), "/v1/display", TIMEOUT)
}

/// Turn the PC's HDR desktop off (or back on) and return its new state.
pub fn set_hdr(ip: Ipv4Addr, on: bool) -> Result<Value> {
    http::post_json(
        (ip, CONTROL_PORT),
        "/v1/display",
        &DisplayRequest { advanced_color: on },
        TIMEOUT,
    )
}

/// Whether any of the PC's displays is composing in advanced colour.
pub fn hdr_is_on(report: &Value) -> bool {
    displays(report).any(|d| d["enabled"] == Value::Bool(true))
}

fn displays(report: &Value) -> impl Iterator<Item = &Value> {
    report["advanced_color"]["displays"]
        .as_array()
        .map(|ds| ds.iter())
        .unwrap_or_default()
}

/// What Windows calls the mode it is in. A PC that has lost its monitor
/// usually ends up in wide colour rather than HDR proper, and the two are
/// different switches in the PC's own settings.
fn colour_mode(report: &Value) -> &'static str {
    if displays(report)
        .any(|d| d["enabled"] == Value::Bool(true) && d["wide_color_enforced"] == Value::Bool(true))
    {
        "wide colour"
    } else {
        "HDR"
    }
}

/// True when Windows lists no monitor that is actually plugged in. The
/// devices it remembers stay listed with `Present: false`.
fn no_monitor(report: &Value) -> bool {
    match report["windows"]["monitors"].as_array() {
        Some(monitors) => !monitors.iter().any(|m| m["Present"] == Value::Bool(true)),
        None => false,
    }
}

/// The brightest pixel the PC sampled on its own desktop, if it looked.
fn desktop_brightness(report: &Value) -> Option<u64> {
    report["windows"]["desktop"]["max"].as_u64()
}

/// One sentence about why the picture is black, in the order that matters:
/// the causes that can be fixed from here come first.
pub fn verdict(report: &Value) -> Help {
    let hdr = hdr_is_on(report);
    let dark = desktop_brightness(report).is_some_and(|max| max <= 16);
    let headless = no_monitor(report);
    let message = if hdr && headless {
        return Help {
            message: format!(
                "The PC has no monitor attached, but Windows is still \
                 composing its desktop in {}. With no display left to \
                 describe the brightness, the capture converts every frame \
                 to black.",
                colour_mode(report)
            ),
            hdr_is_on: true,
            mode: colour_mode(report).into(),
            busy: false,
        };
    } else if hdr {
        return Help {
            message: format!(
                "The PC's desktop is composed in {}. The capture converts \
                 it to an ordinary picture, and on this PC that conversion \
                 is coming out black.",
                colour_mode(report)
            ),
            hdr_is_on: true,
            mode: colour_mode(report).into(),
            busy: false,
        };
    } else if dark && headless {
        "The PC has no monitor attached and its desktop is drawing nothing. \
         Attach a monitor or a dummy plug, or enable a virtual display."
    } else if dark {
        "The PC's own desktop is black: its display is off or asleep. Wake \
         it with a key press, or check the PC's power settings."
    } else if report["windows"]["locked"] == Value::Bool(true) {
        "The PC is showing its lock screen, which Sunshine cannot capture. \
         Sign in on the PC, or let Sunshine run as a service."
    } else if headless {
        "No monitor is attached to the PC, so Windows is drawing to a \
         placeholder display that captures as black. Attach a monitor or a \
         dummy plug, or enable a virtual display."
    } else if desktop_brightness(report).is_some() {
        "The PC's desktop has a picture, so the capture is what is failing. \
         Restarting the stream, or Sunshine, is the next thing to try."
    } else {
        return Help::default();
    };
    Help {
        message: message.into(),
        hdr_is_on: false,
        mode: String::new(),
        busy: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(hdr: bool, present: bool, max: u64, locked: bool) -> Value {
        serde_json::json!({
            "windows": {
                "locked": locked,
                "monitors": [{"FriendlyName": "Generic Monitor", "Present": present}],
                "desktop": {"max": max, "mean": 100, "lit": 998, "samples": 1000},
            },
            "advanced_color": {"supported": true, "displays": [
                {"display": "\\\\.\\DISPLAY1", "supported": true, "enabled": hdr,
                 "bits_per_color": if hdr { 10 } else { 8 }}
            ]},
        })
    }

    #[test]
    fn an_hdr_desktop_without_a_monitor_is_named_and_offered_a_fix() {
        let h = verdict(&report(true, false, 255, false));
        assert!(h.message.contains("no monitor"), "{}", h.message);
        assert!(h.message.contains("HDR"), "{}", h.message);
        assert!(h.hdr_is_on);
    }

    #[test]
    fn a_desktop_windows_forces_into_wide_colour_is_named_as_such() {
        let mut r = report(true, false, 255, false);
        r["advanced_color"]["displays"][0]["wide_color_enforced"] = Value::Bool(true);
        r["advanced_color"]["displays"][0]["supported"] = Value::Bool(false);
        let h = verdict(&r);
        assert!(h.message.contains("wide colour"), "{}", h.message);
        assert_eq!(h.mode, "wide colour");
        assert!(
            h.hdr_is_on,
            "an unsupported display still has it switched on"
        );
    }

    #[test]
    fn hdr_with_a_monitor_still_offers_the_switch() {
        let h = verdict(&report(true, true, 255, false));
        assert!(h.message.contains("HDR"), "{}", h.message);
        assert_eq!(h.mode, "HDR");
        assert!(h.hdr_is_on);
    }

    #[test]
    fn a_dark_desktop_and_a_lock_screen_are_told_apart() {
        let dark = verdict(&report(false, true, 3, false));
        assert!(dark.message.contains("off or asleep"), "{}", dark.message);
        assert!(!dark.hdr_is_on);
        let locked = verdict(&report(false, true, 255, true));
        assert!(locked.message.contains("lock screen"), "{}", locked.message);
    }

    #[test]
    fn a_desktop_with_a_picture_blames_the_capture() {
        let h = verdict(&report(false, true, 255, false));
        assert!(
            h.message.contains("capture is what is failing"),
            "{}",
            h.message
        );
        assert!(!h.hdr_is_on);
    }

    #[test]
    fn a_report_that_says_nothing_yields_no_verdict() {
        assert_eq!(verdict(&serde_json::json!({})), Help::default());
        // A PC that could not sample its desktop still names what it knows.
        let mut r = report(false, false, 0, false);
        r["windows"]["desktop"] = Value::Null;
        assert!(verdict(&r).message.contains("No monitor"));
    }
}
