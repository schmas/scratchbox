//! Note identity and format.

use std::path::{Component, Path};
use std::time::SystemTime;

use crate::naming;

/// A note's bare file name — never a path.
///
/// The inner field is private on purpose. The order manifest is plain text, invites hand
/// editing, and arrives from other devices through whatever syncs the workspace, so it is
/// untrusted input. A manifest line reading `../../.ssh/id_rsa` would otherwise resolve to
/// a real file, pass an existence check, render in the note list, and let a delete move the
/// user's SSH key to the trash. Every `NoteId` therefore goes through [`NoteId::new`].
///
/// Exposing the field — even for a "quick" internal construction — reopens that hole.
///
/// ```
/// use scratchbox_core::NoteId;
/// assert!(NoteId::new("2026-08-15-1548.md").is_ok());
/// assert!(NoteId::new("../../.ssh/id_rsa").is_err());
/// ```
///
/// The field cannot be set directly, which is what makes [`NoteId::new`] the only way in:
///
/// ```compile_fail
/// use scratchbox_core::NoteId;
/// let escaped = NoteId("../../.ssh/id_rsa".to_string());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NoteId(String);

/// Why a candidate name is not a usable note name.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InvalidNoteId {
    #[error("note name is empty")]
    Empty,
    #[error("note name {0:?} contains a path separator")]
    Separator(String),
    #[error("note name {0:?} is not a single plain file name")]
    NotPlainName(String),
    #[error("note name {0:?} contains a NUL byte")]
    NulByte(String),
    #[error("note name {0:?} starts with a dot")]
    Hidden(String),
}

impl NoteId {
    /// Validate a bare file name.
    ///
    /// Rejects empty names, `.` and `..`, anything holding a path separator or a NUL byte,
    /// absolute paths, and names beginning with `.` — the last of which also keeps dotfiles
    /// and `.scratchbox` out of the note list by construction.
    pub fn new(name: &str) -> Result<Self, InvalidNoteId> {
        if name.is_empty() {
            return Err(InvalidNoteId::Empty);
        }
        if name.contains('\0') {
            return Err(InvalidNoteId::NulByte(name.to_owned()));
        }
        // Backslash is a legal file-name character on Unix, so the component check below
        // would accept `a\b`. Windows would read it as a separator; reject it everywhere.
        if name.contains('/') || name.contains('\\') {
            return Err(InvalidNoteId::Separator(name.to_owned()));
        }
        // Checked before the dot rule so these report what is actually wrong with them.
        if name == "." || name == ".." {
            return Err(InvalidNoteId::NotPlainName(name.to_owned()));
        }
        if name.starts_with('.') {
            return Err(InvalidNoteId::Hidden(name.to_owned()));
        }

        // One `Normal` component that survives normalization unchanged. This is what
        // rejects `.`, `..`, `/x`, `a/`, `./x`, and Windows drive prefixes in one check.
        let mut components = Path::new(name).components();
        match (components.next(), components.next()) {
            (Some(Component::Normal(only)), None) if only == name => Ok(Self(name.to_owned())),
            _ => Err(InvalidNoteId::NotPlainName(name.to_owned())),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for NoteId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Syntax family of a note, derived from its extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    Markdown,
    Json,
    PlainText,
    Java,
    TypeScript,
    JavaScript,
    Css,
    Html,
}

impl Format {
    /// Map an extension to a format. Case-insensitive; anything unrecognized is plain text.
    pub fn from_extension(ext: &str) -> Self {
        match ext.to_ascii_lowercase().as_str() {
            "md" => Self::Markdown,
            "json" => Self::Json,
            "java" => Self::Java,
            "ts" => Self::TypeScript,
            "js" => Self::JavaScript,
            "css" => Self::Css,
            "html" => Self::Html,
            _ => Self::PlainText,
        }
    }

    /// Map a whole note name. A name without an extension is plain text.
    pub fn from_name(name: &str) -> Self {
        match naming::split_extension(name) {
            (_, Some(ext)) => Self::from_extension(ext),
            (_, None) => Self::PlainText,
        }
    }

    /// Default extension used when creating a note of this format.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Markdown => "md",
            Self::Json => "json",
            Self::PlainText => "txt",
            Self::Java => "java",
            Self::TypeScript => "ts",
            Self::JavaScript => "js",
            Self::Css => "css",
            Self::Html => "html",
        }
    }
}

/// What the note list needs to know about a note without reading its contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteMeta {
    pub id: NoteId,
    pub format: Format,
    pub modified: SystemTime,
    /// True once the rename-on-first-content has happened. Derived from the name's shape
    /// rather than stored, so there is no sidecar to keep in sync.
    pub slugged: bool,
}

impl NoteMeta {
    /// Derive format and slug state from the name.
    pub fn from_id(id: NoteId, modified: SystemTime) -> Self {
        let format = Format::from_name(id.as_str());
        let slugged = naming::is_slugged(&id);
        Self {
            id,
            format,
            modified,
            slugged,
        }
    }
}
