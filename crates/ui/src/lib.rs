//! The look shared by the BroLink host and client.
//!
//! Both apps are small egui programs aimed at people who are not developers.
//! egui's defaults read as a debugging tool, so this crate owns everything
//! that makes the two windows look like one product: the palette, the
//! bundled typeface, the widget styling, and a handful of composite widgets
//! (cards, list rows, status pills, the stream overlay) the screens are built
//! from. Keeping it here means the host and client cannot drift apart.

pub mod theme;
pub mod widgets;

pub use theme::{apply, Palette, PALETTE};
pub use widgets::*;
