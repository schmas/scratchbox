//! Headless core for scratchbox.
//!
//! Contains no terminal or widget types: everything here is usable from the TUI, the
//! CLI, and tests alike.

pub mod config;
mod error;

pub use config::{APP_SUBDIR, Config, Dirs};
pub use error::{Error, Result};
