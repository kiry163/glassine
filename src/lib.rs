//! glassine — a Windows desktop transparent overlay.
//!
//! Everything except [`platform`] is platform-free, which is what makes the
//! rendering pipeline testable without a desktop and the macOS port a rewrite
//! of one module. `main.rs` is a thin binary that wires these pieces together.
//!
//! The library target exists so each module's public surface is externally
//! visible rather than dead code until a later task wires it up.

pub mod geometry;
