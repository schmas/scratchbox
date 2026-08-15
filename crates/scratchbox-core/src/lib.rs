//! Headless core for scratchbox.
//!
//! Contains no terminal or widget types: everything here is usable from the TUI, the
//! CLI, and tests alike.

pub mod config;
mod error;
pub mod foldersync;
pub mod naming;
pub mod note;
pub mod store;
pub mod suppress;
pub mod watcher;

/// Re-exported because [`naming::new_note_name`] takes a `Zoned`, so callers need the
/// same `jiff` this crate was built against.
pub use jiff;

pub use config::{APP_SUBDIR, Config, Dirs};
pub use error::{Error, Result};
pub use foldersync::FolderSync;
pub use note::{Format, InvalidNoteId, NoteId, NoteMeta};
pub use store::{Store, StoreEvent, WorkspaceHealth};
pub use suppress::Suppressor;
