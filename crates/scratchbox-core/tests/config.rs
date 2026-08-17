//! Config resolution tests.
//!
//! These build [`Dirs`] explicitly instead of setting `$XDG_*`, because environment
//! variables are process-global and these tests run on parallel threads. The env-reading
//! path is covered separately through [`Dirs::resolve`], which is pure. The fixture below
//! preserves that: it creates a directory, not an environment variable.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use rstest::{fixture, rstest};
use scratchbox_core::{APP_SUBDIR, Config, Dirs};
use tempfile::TempDir;

/// A config root and the [`Dirs`] resolved against it.
///
/// The `TempDir` is held here rather than handed back separately: dropped early it takes the
/// directory with it, and every path in `Dirs` would then point at nothing.
///
/// `root` is public to the tests because two of them assert against it —
/// `missing_config_file_yields_defaults` expects `<root>/data/scratchbox/notes`, and
/// `default_trash_stays_outside_an_overridden_workspace` builds `<root>/Google Drive/notes`. A
/// fixture that hid the root could not serve either, which is the same reason
/// `crates/scratchbox-cli/tests/support/mod.rs` exposes its own.
struct Sandbox {
    _tmp: TempDir,
    root: PathBuf,
    dirs: Dirs,
}

#[fixture]
fn sandbox() -> Sandbox {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let dirs = dirs_in(&root);
    Sandbox {
        _tmp: tmp,
        root,
        dirs,
    }
}

fn dirs_in(root: &Path) -> Dirs {
    Dirs {
        config_home: root.join("config"),
        data_home: root.join("data"),
        home: root.join("home"),
    }
}

fn write_config(dirs: &Dirs, contents: &str) {
    let path = dirs.config_file();
    fs::create_dir_all(path.parent().expect("config file has a parent")).unwrap();
    fs::write(path, contents).unwrap();
}

#[rstest]
fn missing_config_file_yields_defaults(sandbox: Sandbox) {
    let config = Config::load_with(&sandbox.dirs, None).unwrap();

    assert_eq!(config.workspace, sandbox.root.join("data/scratchbox/notes"));
    assert_eq!(config.trash, sandbox.root.join("data/scratchbox/trash"));
    assert!(config.warnings.is_empty());
}

#[rstest]
fn workspace_key_is_honored(sandbox: Sandbox) {
    write_config(&sandbox.dirs, "workspace = \"/somewhere/notes\"\n");

    let config = Config::load_with(&sandbox.dirs, None).unwrap();

    assert_eq!(config.workspace, PathBuf::from("/somewhere/notes"));
}

#[rstest]
fn cli_override_beats_the_config_key(sandbox: Sandbox) {
    write_config(&sandbox.dirs, "workspace = \"/from/config\"\n");

    let config = Config::load_with(&sandbox.dirs, Some(PathBuf::from("/from/cli"))).unwrap();

    assert_eq!(config.workspace, PathBuf::from("/from/cli"));
}

#[rstest]
fn tilde_in_config_paths_expands_to_home(sandbox: Sandbox) {
    write_config(&sandbox.dirs, "workspace = \"~/notes\"\n");

    let config = Config::load_with(&sandbox.dirs, None).unwrap();

    assert_eq!(config.workspace, sandbox.dirs.home.join("notes"));
}

/// No tempdir and no config file, so it takes no fixture: `Dirs::resolve` is pure.
#[test]
fn xdg_config_home_is_honored_when_absolute() {
    let dirs = Dirs::resolve(
        Some(OsString::from("/custom/config")),
        Some(OsString::from("/custom/data")),
        PathBuf::from("/home/someone"),
    );

    assert_eq!(dirs.config_home, PathBuf::from("/custom/config"));
    assert_eq!(dirs.data_home, PathBuf::from("/custom/data"));
    assert_eq!(
        dirs.config_file(),
        PathBuf::from("/custom/config/scratchbox/config.toml")
    );
}

/// Two rows rather than two halves of one body, so a failure names which value was ignored
/// wrongly. Takes no fixture for the same reason as the test above.
#[rstest]
#[case::unset(None, None)]
// The XDG spec says a relative value must be ignored rather than joined, and an empty value is
// relative.
#[case::relative_and_empty(Some("relative/config"), Some(""))]
fn xdg_falls_back_to_dot_config_when_unset_or_relative(
    #[case] config_home: Option<&str>,
    #[case] data_home: Option<&str>,
) {
    let home = PathBuf::from("/home/someone");

    let dirs = Dirs::resolve(
        config_home.map(OsString::from),
        data_home.map(OsString::from),
        home.clone(),
    );

    assert_eq!(dirs.config_home, home.join(".config"));
    assert_eq!(dirs.data_home, home.join(".local").join("share"));
}

#[rstest]
fn ensure_dirs_creates_workspace_app_dir_and_trash(sandbox: Sandbox) {
    let config = Config::load_with(&sandbox.dirs, None).unwrap();
    config.ensure_dirs().unwrap();

    assert!(config.workspace.is_dir());
    assert!(config.workspace.join(APP_SUBDIR).is_dir());
    assert!(config.trash.is_dir());
}

#[rstest]
fn default_trash_stays_outside_an_overridden_workspace(sandbox: Sandbox) {
    // The dangerous case D11 was revised for: a workspace inside a synced cloud folder.
    let cloud = sandbox.root.join("Google Drive/notes");

    let config = Config::load_with(&sandbox.dirs, Some(cloud.clone())).unwrap();

    assert_eq!(config.workspace, cloud);
    assert!(
        !config.trash.starts_with(&cloud),
        "default trash {} landed inside the synced workspace",
        config.trash.display()
    );
    assert!(config.warnings.is_empty());
}

#[rstest]
fn trash_configured_inside_the_workspace_warns(sandbox: Sandbox) {
    write_config(
        &sandbox.dirs,
        "workspace = \"/notes\"\ntrash = \"/notes/.trash\"\n",
    );

    let config = Config::load_with(&sandbox.dirs, None).unwrap();

    assert_eq!(config.warnings.len(), 1);
    assert!(
        config.warnings[0].contains("inside the workspace"),
        "unexpected warning: {}",
        config.warnings[0]
    );
}
