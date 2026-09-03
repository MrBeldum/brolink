//! Client-side clipboard bridge.

use brolink_core::proto::{clipboard_or_skip, ControlMsg};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

pub struct ClipboardBridge {
    inner: Option<arboard::Clipboard>,
    last_hash: u64,
    echo: u64,
}

impl ClipboardBridge {
    pub fn new() -> Self {
        Self {
            inner: arboard::Clipboard::new().ok(),
            last_hash: 0,
            echo: 0,
        }
    }

    pub fn poll_outgoing(&mut self) -> Option<ControlMsg> {
        let clip = self.inner.as_mut()?;
        let text = clip.get_text().ok()?;
        let hash = hash_text(&text);
        if hash == self.last_hash || hash == self.echo {
            return None;
        }
        self.last_hash = hash;
        clipboard_or_skip(&text)
    }

    pub fn apply_remote(&mut self, text: &str) {
        let hash = hash_text(text);
        self.echo = hash;
        self.last_hash = hash;
        if let Some(clip) = self.inner.as_mut() {
            let _ = clip.set_text(text);
        }
    }
}

fn hash_text(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_stable() {
        assert_eq!(hash_text("a"), hash_text("a"));
        assert_ne!(hash_text("a"), hash_text("b"));
    }
}
