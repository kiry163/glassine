//! Platform boundary.
//!
//! This module is the only place allowed to reference the `windows` crate. The
//! spec makes that a hard rule because it is what keeps the rendering pipeline
//! testable without a desktop and makes the macOS port a rewrite of one module.

#[cfg(windows)]
pub mod win;
