//! BroLink's design system: the tokens and components every window and
//! overlay is built from.
//!
//! The apps are small egui programs aimed at people who are not developers.
//! egui's defaults read as a debugging tool, so this crate owns everything
//! that makes the viewer, the Sharing page and the stream overlay look like
//! one product, and like the product site: the palette, the type scale on
//! Geist and Geist Mono, the spacing grid, radii and elevation
//! ([`theme`]), and the components screens are assembled from ([`widgets`]):
//! cards, grouped lists and rows, buttons in five kinds, menus, tabs,
//! switches, notices, tags, keycaps. Screens use these and nothing inline,
//! so the parts cannot drift apart.

pub mod theme;
pub mod widgets;

pub use theme::{apply, column, radius, size, space, Palette, PALETTE};
pub use widgets::*;
