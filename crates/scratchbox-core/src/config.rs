//! Path and config resolution.
//!
//! XDG layout is used on every platform, macOS included. That is a deliberate departure
//! from `~/Library/Application Support`: scratchbox is a terminal tool and its config
//! belongs next to the user's other terminal tools.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};

/// Directory inside the workspace holding app state. Excluded from every note scan.
pub const APP_SUBDIR: &str = ".scratchbox";

/// Name used under both the config and data homes.
const APP_NAME: &str = "scratchbox";

const CONFIG_FILE: &str = "config.toml";
const NOTES_SUBDIR: &str = "notes";
const TRASH_SUBDIR: &str = "trash";

/// Base directories scratchbox resolves everything else against.
///
/// Constructing this explicitly is what lets tests exercise resolution without touching
/// process-global environment variables.
#[derive(Debug, Clone)]
pub struct Dirs {
    pub config_home: PathBuf,
    pub data_home: PathBuf,
    pub home: PathBuf,
}

impl Dirs {
    /// Read the real environment: `$XDG_CONFIG_HOME`, `$XDG_DATA_HOME`, and the home
    /// directory.
    pub fn from_env() -> Result<Self> {
        let home = directories::BaseDirs::new()
            .ok_or(Error::NoHomeDir)?
            .home_dir()
            .to_path_buf();
        Ok(Self::resolve(
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("XDG_DATA_HOME"),
            home,
        ))
    }

    /// Pure form of [`Dirs::from_env`], so the precedence rules are testable.
    ///
    /// Per the XDG spec a variable that is empty or relative is ignored.
    pub fn resolve(
        config_home: Option<OsString>,
        data_home: Option<OsString>,
        home: PathBuf,
    ) -> Self {
        let config_home = xdg_dir(config_home, || home.join(".config"));
        let data_home = xdg_dir(data_home, || home.join(".local").join("share"));
        Self {
            config_home,
            data_home,
            home,
        }
    }

    /// `<config_home>/scratchbox/config.toml`
    pub fn config_file(&self) -> PathBuf {
        self.config_home.join(APP_NAME).join(CONFIG_FILE)
    }

    fn default_workspace(&self) -> PathBuf {
        self.data_home.join(APP_NAME).join(NOTES_SUBDIR)
    }

    fn default_trash(&self) -> PathBuf {
        self.data_home.join(APP_NAME).join(TRASH_SUBDIR)
    }
}

fn xdg_dir(var: Option<OsString>, fallback: impl FnOnce() -> PathBuf) -> PathBuf {
    match var {
        Some(value) if Path::new(&value).is_absolute() => PathBuf::from(value),
        _ => fallback(),
    }
}

/// On-disk shape of `config.toml`. Every key is optional; a missing file means defaults.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    workspace: Option<PathBuf>,
    trash: Option<PathBuf>,
}

/// Fully resolved runtime configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Directory holding the notes. Flat: notes live directly here.
    pub workspace: PathBuf,
    /// Where deleted notes go. Defaults outside the workspace so deletions never sync.
    pub trash: PathBuf,
    /// Non-fatal problems for the front end to show once at startup.
    pub warnings: Vec<String>,
}

impl Config {
    /// Path of the config file, whether or not it exists.
    pub fn config_path() -> Result<PathBuf> {
        Ok(Dirs::from_env()?.config_file())
    }

    /// Resolve config from the real environment.
    ///
    /// Precedence for the workspace is `cli_override` > the `workspace` key > the XDG
    /// default.
    pub fn load(cli_override: Option<PathBuf>) -> Result<Self> {
        Self::load_with(&Dirs::from_env()?, cli_override)
    }

    /// [`Config::load`] against explicit base directories.
    pub fn load_with(dirs: &Dirs, cli_override: Option<PathBuf>) -> Result<Self> {
        let file = read_config_file(&dirs.config_file())?;

        let workspace = cli_override
            .or(file.workspace)
            .map(|p| expand_tilde(p, &dirs.home))
            .unwrap_or_else(|| dirs.default_workspace());

        let trash = file
            .trash
            .map(|p| expand_tilde(p, &dirs.home))
            .unwrap_or_else(|| dirs.default_trash());

        let mut warnings = Vec::new();
        if trash.starts_with(&workspace) {
            warnings.push(format!(
                "trash directory {} is inside the workspace; deleted notes will sync with it. \
                 Move `trash` outside {} in {}.",
                trash.display(),
                workspace.display(),
                dirs.config_file().display(),
            ));
        }

        Ok(Self {
            workspace,
            trash,
            warnings,
        })
    }

    /// `<workspace>/.scratchbox`, holding app state only — never a note.
    pub fn app_dir(&self) -> PathBuf {
        self.workspace.join(APP_SUBDIR)
    }

    /// Create the workspace, its app dir, and the trash dir if they are missing.
    pub fn ensure_dirs(&self) -> Result<()> {
        for dir in [self.workspace.clone(), self.app_dir(), self.trash.clone()] {
            fs::create_dir_all(&dir).map_err(|source| Error::CreateDir { path: dir, source })?;
        }
        Ok(())
    }
}

fn read_config_file(path: &Path) -> Result<ConfigFile> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(ConfigFile::default()),
        Err(source) => {
            return Err(Error::ConfigRead {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    toml::from_str(&text).map_err(|source| Error::ConfigParse {
        path: path.to_path_buf(),
        source,
    })
}

/// TOML has no shell expansion, so a leading `~` is expanded here.
fn expand_tilde(path: PathBuf, home: &Path) -> PathBuf {
    let Ok(rest) = path.strip_prefix("~") else {
        return path;
    };
    home.join(rest)
}
