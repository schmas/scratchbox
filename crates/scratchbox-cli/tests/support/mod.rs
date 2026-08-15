//! A sandboxed workspace, trash, and config file, plus the means to run the CLI against
//! them.
//!
//! Shared by every CLI test binary. Each child process gets its own `XDG_*` environment, so
//! config resolution is exercised for real without any test touching the environment of the
//! process it is running in.

#![allow(dead_code)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

/// A workspace, a trash beside it, and a config file naming both.
pub struct Sandbox {
    _tmp: TempDir,
    pub root: PathBuf,
}

impl Sandbox {
    pub fn new() -> Self {
        Self::with_trash("trash")
    }

    /// `trash` is relative to the sandbox root, so a test can point it somewhere it should
    /// not be.
    pub fn with_trash(trash: &str) -> Self {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();

        let workspace = root.join("notes");
        let trash = root.join(trash);
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(root.join("config/scratchbox")).unwrap();
        fs::write(
            root.join("config/scratchbox/config.toml"),
            format!(
                "workspace = {:?}\ntrash = {:?}\n",
                workspace.display().to_string(),
                trash.display().to_string()
            ),
        )
        .unwrap();

        Self { _tmp: tmp, root }
    }

    pub fn workspace(&self) -> PathBuf {
        self.root.join("notes")
    }

    pub fn trash(&self) -> PathBuf {
        self.root.join("trash")
    }

    pub fn note(&self, name: &str, body: &str) {
        fs::write(self.workspace().join(name), body).unwrap();
    }

    pub fn read(&self, name: &str) -> String {
        fs::read_to_string(self.workspace().join(name)).unwrap()
    }

    pub fn note_names(&self) -> Vec<String> {
        names_in(&self.workspace())
    }

    /// Run the CLI with `input` piped in. Stdin is a pipe, never a terminal.
    pub fn run(&self, args: &[&str], input: &[u8]) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_scratchbox"))
            .args(args)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        child.stdin.take().unwrap().write_all(input).unwrap();
        child.wait_with_output().unwrap()
    }
}

pub fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with('.'))
        .collect();
    names.sort();
    names
}

pub fn code(output: &Output) -> i32 {
    output.status.code().unwrap()
}

pub fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
