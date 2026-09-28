//! Platform boundary.
//!
//! This module is the only place allowed to reference the `windows` crate. The
//! spec makes that a hard rule because it is what keeps the rendering pipeline
//! testable without a desktop and makes the macOS port a rewrite of one module.
//!
//! It starts small — `--check-config` needs a work area to resolve an anchor
//! against — and grows with the window, tray, and move-mode tasks.

#[cfg(windows)]
mod win;

#[cfg(windows)]
pub use win::*;
