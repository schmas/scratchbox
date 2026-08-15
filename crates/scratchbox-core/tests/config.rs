//! Config resolution tests.
//!
//! These build [`Dirs`] explicitly instead of setting `$XDG_*`, because environment
//! variables are process-global and these tests run on parallel threads. The env-reading
//! path is covered separately through [`Dirs::resolve`], which is pure.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use scratchbox_core::{APP_SUBDIR, Config, Dirs};
use tempfile::TempDir;

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

#[test]
fn missing_config_file_yields_defaults() {
    let tmp = TempDir::new().unwrap();
    let dirs = dirs_in(tmp.path());

    let config = Config::load_with(&dirs, None).unwrap();

    assert_eq!(config.workspace, tmp.path().join("data/scratchbox/notes"));
    assert_eq!(config.trash, tmp.path().join("data/scratchbox/trash"));
    assert!(config.warnings.is_empty());
}

#[test]
fn workspace_key_is_honored() {
    let tmp = TempDir::new().unwrap();
    let dirs = dirs_in(tmp.path());
    write_config(&dirs, "workspace = \"/somewhere/notes\"\n");

    let config = Config::load_with(&dirs, None).unwrap();

    assert_eq!(config.workspace, PathBuf::from("/somewhere/notes"));
}

#[test]
fn cli_override_beats_the_config_key() {
    let tmp = TempDir::new().unwrap();
    let dirs = dirs_in(tmp.path());
    write_config(&dirs, "workspace = \"/from/config\"\n");

    let config = Config::load_with(&dirs, Some(PathBuf::from("/from/cli"))).unwrap();

    assert_eq!(config.workspace, PathBuf::from("/from/cli"));
}

#[test]
fn tilde_in_config_paths_expands_to_home() {
    let tmp = TempDir::new().unwrap();
    let dirs = dirs_in(tmp.path());
    write_config(&dirs, "workspace = \"~/notes\"\n");

    let config = Config::load_with(&dirs, None).unwrap();

    assert_eq!(config.workspace, dirs.home.join("notes"));
}

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

#[test]
fn xdg_falls_back_to_dot_config_when_unset_or_relative() {
    let home = PathBuf::from("/home/someone");

    let unset = Dirs::resolve(None, None, home.clone());
    assert_eq!(unset.config_home, home.join(".config"));
    assert_eq!(unset.data_home, home.join(".local").join("share"));

    // The XDG spec says a relative value must be ignored, not joined.
    let relative = Dirs::resolve(
        Some(OsString::from("relative/config")),
        Some(OsString::from("")),
        home.clone(),
    );
    assert_eq!(relative.config_home, home.join(".config"));
    assert_eq!(relative.data_home, home.join(".local").join("share"));
}

#[test]
fn ensure_dirs_creates_workspace_app_dir_and_trash() {
    let tmp = TempDir::new().unwrap();
    let dirs = dirs_in(tmp.path());

    let config = Config::load_with(&dirs, None).unwrap();
    config.ensure_dirs().unwrap();

    assert!(config.workspace.is_dir());
    assert!(config.workspace.join(APP_SUBDIR).is_dir());
    assert!(config.trash.is_dir());
}

#[test]
fn default_trash_stays_outside_an_overridden_workspace() {
    let tmp = TempDir::new().unwrap();
    let dirs = dirs_in(tmp.path());
    // The dangerous case D11 was revised for: a workspace inside a synced cloud folder.
    let cloud = tmp.path().join("Google Drive/notes");

    let config = Config::load_with(&dirs, Some(cloud.clone())).unwrap();

    assert_eq!(config.workspace, cloud);
    assert!(
        !config.trash.starts_with(&cloud),
        "default trash {} landed inside the synced workspace",
        config.trash.display()
    );
    assert!(config.warnings.is_empty());
}

#[test]
fn trash_configured_inside_the_workspace_warns() {
    let tmp = TempDir::new().unwrap();
    let dirs = dirs_in(tmp.path());
    write_config(&dirs, "workspace = \"/notes\"\ntrash = \"/notes/.trash\"\n");

    let config = Config::load_with(&dirs, None).unwrap();

    assert_eq!(config.warnings.len(), 1);
    assert!(
        config.warnings[0].contains("inside the workspace"),
        "unexpected warning: {}",
        config.warnings[0]
    );
}
