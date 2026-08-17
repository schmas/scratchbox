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
const LOG_SUBDIR: &str = "log";

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
    /// The data home this config resolved against, retained so [`Config::log_dir`] derives
    /// from it rather than reading the environment a second time.
    pub data_home: PathBuf,
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
            data_home: dirs.data_home.clone(),
            warnings,
        })
    }

    /// `<workspace>/.scratchbox`, holding app state only — never a note.
    pub fn app_dir(&self) -> PathBuf {
        self.workspace.join(APP_SUBDIR)
    }

    /// `<data_home>/scratchbox/log` — where diagnostics go when they are asked for.
    ///
    /// Outside the workspace on purpose, and that is the whole reason it is not
    /// `<workspace>/.scratchbox/log`: a log line describing a filesystem event, written
    /// inside the watched tree, produces that same event one debounce window later, which is
    /// logged, forever. This is the same directory family the trash defaults into, so it
    /// resolves nothing new.
    ///
    /// Deliberately *not* created by [`Config::ensure_dirs`]. Diagnostics are off unless
    /// `RUST_LOG` asks for them, and a directory left behind for a feature that never ran
    /// would contradict that.
    ///
    /// ```
    /// use std::path::PathBuf;
    /// use scratchbox_core::{Config, Dirs};
    ///
    /// let dirs = Dirs {
    ///     config_home: PathBuf::from("/nowhere/config"),
    ///     data_home: PathBuf::from("/data"),
    ///     home: PathBuf::from("/home/someone"),
    /// };
    /// let config = Config::load_with(&dirs, None).unwrap();
    ///
    /// assert_eq!(config.log_dir(), PathBuf::from("/data/scratchbox/log"));
    /// ```
    pub fn log_dir(&self) -> PathBuf {
        self.data_home.join(APP_NAME).join(LOG_SUBDIR)
    }

    /// Do the trash and the workspace sit inside one another, either way round?
    ///
    /// Both directions are dangerous and neither is a normal setup. A trash inside the
    /// workspace syncs deleted notes, which is what moving the trash out was for. A
    /// workspace inside the trash is worse: emptying the trash would take every live note
    /// with it. Callers that delete consult this and refuse rather than proceed.
    ///
    /// Component-wise, so a trash at `<workspace>-old` is correctly seen as separate.
    pub fn trash_overlaps_workspace(&self) -> bool {
        self.trash.starts_with(&self.workspace) || self.workspace.starts_with(&self.trash)
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
