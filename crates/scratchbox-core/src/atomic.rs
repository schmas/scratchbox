//! Writing a file so that a reader never sees it half-written.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;

use crate::error::{Error, Result};

/// Owner-only. A scratchpad collects API keys and passwords whether or not it was meant
/// to, and the two creation paths would otherwise disagree — a temp file starts at 0600
/// while a plain create follows the umask.
#[cfg(unix)]
const FILE_MODE: u32 = 0o600;

/// Prefix for in-flight files. The leading dot keeps them out of note listings — and out
/// of a note name, since `NoteId` refuses names starting with a dot.
pub(crate) const TEMP_PREFIX: &str = ".tmp-";

/// Write `contents` to `path` through a temp file in the same directory.
///
/// Same directory, not the system temp dir: the final rename has to stay on one filesystem
/// to be atomic, and a workspace on a cloud mount is not on the same filesystem as `/tmp`.
pub(crate) fn write_atomically(path: &Path, contents: &[u8]) -> Result<()> {
    let staging = staging_path(path)?;

    let result = (|| {
        let mut file = create(&staging).map_err(Error::io("write", &staging))?;
        file.write_all(contents)
            .map_err(Error::io("write", &staging))?;
        file.sync_all().map_err(Error::io("flush", &staging))
    })();

    if let Err(error) = result {
        let _ = fs::remove_file(&staging);
        return Err(error);
    }

    fs::rename(&staging, path).map_err(Error::io("replace", path))
}

fn staging_path(path: &Path) -> Result<std::path::PathBuf> {
    let invalid = || Error::Io {
        action: "write",
        path: path.to_path_buf(),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path has no file name to stage beside",
        ),
    };
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(invalid)?;
    let parent = path.parent().ok_or_else(invalid)?;
    Ok(parent.join(format!("{TEMP_PREFIX}{name}")))
}

/// Create or truncate a file, owner-only where the platform has a notion of it.
pub(crate) fn create(path: &Path) -> std::io::Result<File> {
    options().create(true).truncate(true).open(path)
}

/// Create a file, failing if the name is already taken.
pub(crate) fn create_new(path: &Path) -> std::io::Result<File> {
    options().create_new(true).open(path)
}

fn options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(FILE_MODE);
    }
    options
}
