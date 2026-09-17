//! Native X11 (EWMH/ICCCM) surface for the window2 tools.
//!
//! Everything here is pure Rust through `x11rb`: the crate's default features never
//! link libxcb, and the extensions used (SHM, XTest, XFixes, XComposite) come from
//! `x11rb-protocol`'s wire definitions. That matters on this machine, where no X11
//! development package is installed at all: there is nothing to `pkg-config`.
//!
//! The module is deliberately self-contained. `helper.rs` only mounts it; the tool
//! table, the dispatcher and the state live here so the helper's own surface (the
//! seven sky.window tools) is never entangled with the window2 surface.

pub mod capture;
pub mod connection;
pub mod element;
pub mod input;
pub mod launch;
pub mod waitfor;
pub mod window;
pub mod window2;

pub use connection::{X11Connection, X11Error};
pub use window::{WindowGeometry, X11Window};

/// The backend id reported for windows and actions that came through this module.
///
/// Distinct from `windowing::X11_BACKEND` ("x11"), which is the `wmctrl`+`xprop`
/// backend: that one needs external binaries this machine does not have, while this
/// one talks to the server directly. Keeping the ids distinct is what lets health
/// report which of the two actually works.
pub const X11_NATIVE_BACKEND: &str = "x11-native";
