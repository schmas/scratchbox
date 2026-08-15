use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not determine the home directory")]
    NoHomeDir,

    #[error("could not read config file {path}")]
    ConfigRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not parse config file {path}")]
    ConfigParse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("could not create directory {path}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(transparent)]
    InvalidNoteId(#[from] crate::note::InvalidNoteId),

    #[error("could not {action} {path}")]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A name that resolves outside the workspace — a symlink pointing elsewhere, most
    /// likely. Refused rather than followed.
    #[error("note {name} resolves to {resolved}, which is outside the workspace")]
    EscapesWorkspace { name: String, resolved: PathBuf },

    #[error("note {path} is not valid UTF-8")]
    NotUtf8 { path: PathBuf },

    #[error("could not find a free name for {name} after {tried} attempts")]
    NoFreeName { name: String, tried: usize },

    #[error("could not watch the workspace")]
    Watch(#[from] notify::Error),
}

impl Error {
    pub(crate) fn io(
        action: &'static str,
        path: impl Into<PathBuf>,
    ) -> impl FnOnce(std::io::Error) -> Self {
        let path = path.into();
        move |source| Self::Io {
            action,
            path,
            source,
        }
    }
}
