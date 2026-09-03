//! Adaptive bitrate: turn measured loss/RTT into a new target.
//!
//! The encoder is configured on the ffmpeg command line, so a change here is
//! a *recommendation* the host applies by restarting the pipeline. Changes are
//! rate-limited so a burst of loss cannot flap the encoder every second.

use crate::config::{MAX_BITRATE_KBPS, MIN_BITRATE_KBPS};

/// Seconds the controller wants the host to wait between encoder restarts.
pub const COOLDOWN_SECS: u64 = 6;

#[derive(Debug, Clone)]
pub struct AbrController {
    pub current_kbps: u32,
    pub min_kbps: u32,
    pub max_kbps: u32,
    high_loss: u32,
    low_loss: u32,
}

impl AbrController {
    pub fn new(current_kbps: u32, max_kbps: u32) -> Self {
        let current = current_kbps.clamp(MIN_BITRATE_KBPS, MAX_BITRATE_KBPS);
        let max = max_kbps
            .clamp(MIN_BITRATE_KBPS, MAX_BITRATE_KBPS)
            .max(current);
        Self {
            current_kbps: current,
            min_kbps: MIN_BITRATE_KBPS.max(max / 8),
            max_kbps: max,
            high_loss: 0,
            low_loss: 0,
        }
    }

    /// Observe one second of stats. Returns a new bitrate when it is time to
    /// move, or `None` to keep the current encoder.
    pub fn observe(&mut self, loss_pct: f32, rtt_ms: f32) -> Option<u32> {
        let loss = loss_pct.max(0.0);
        let congested = loss >= 8.0 || rtt_ms >= 180.0;
        let healthy = loss < 2.0 && rtt_ms < 80.0;

        if congested {
            self.high_loss = self.high_loss.saturating_add(1);
            self.low_loss = 0;
        } else if healthy {
            self.low_loss = self.low_loss.saturating_add(1);
            self.high_loss = 0;
        } else {
            self.high_loss = 0;
            self.low_loss = 0;
        }

        if self.high_loss >= 2 {
            self.high_loss = 0;
            if self.current_kbps <= self.min_kbps {
                return None;
            }
            let next = ((self.current_kbps as f32) * 0.7) as u32;
            let next = next.max(self.min_kbps).min(self.current_kbps - 1);
            if next < self.current_kbps {
                self.current_kbps = next;
                return Some(next);
            }
        }
        if self.low_loss >= 8 {
            self.low_loss = 0;
            if self.current_kbps >= self.max_kbps {
                return None;
            }
            let next = ((self.current_kbps as f32) * 1.15) as u32;
            let next = next.max(self.current_kbps + 1).min(self.max_kbps);
            if next > self.current_kbps {
                self.current_kbps = next;
                return Some(next);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sustained_loss_drops_the_bitrate() {
        let mut a = AbrController::new(20_000, 25_000);
        assert!(a.observe(0.0, 20.0).is_none());
        assert!(
            a.observe(12.0, 40.0).is_none(),
            "one bad second is not enough"
        );
        let next = a.observe(12.0, 40.0).expect("two bad seconds step down");
        assert!(next < 20_000, "{next}");
        assert_eq!(a.current_kbps, next);
    }

    #[test]
    fn a_healthy_link_ramps_back_up() {
        let mut a = AbrController::new(10_000, 25_000);
        let mut last = None;
        for _ in 0..8 {
            last = a.observe(0.0, 20.0);
        }
        let up = last.expect("eight clean seconds step up");
        assert!(up > 10_000, "{up}");
        assert!(up <= 25_000);
    }

    #[test]
    fn never_leaves_the_legal_range() {
        let mut a = AbrController::new(MIN_BITRATE_KBPS, MIN_BITRATE_KBPS);
        for _ in 0..10 {
            assert!(a.observe(50.0, 400.0).is_none());
        }
        assert_eq!(a.current_kbps, MIN_BITRATE_KBPS);
    }

    #[test]
    fn mixed_stats_do_not_flap() {
        let mut a = AbrController::new(20_000, 25_000);
        assert!(a.observe(12.0, 20.0).is_none());
        assert!(a.observe(0.0, 20.0).is_none());
        assert!(a.observe(12.0, 20.0).is_none());
        assert_eq!(a.current_kbps, 20_000);
    }
}
