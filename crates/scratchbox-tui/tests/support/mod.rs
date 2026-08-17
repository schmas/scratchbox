//! A sandboxed data home and workspace, plus the means to run the TUI binary against them.
//!
//! New in this phase. This test directory previously held only in-process tests over `App`,
//! which cannot observe what the *binary* decides at startup — and diagnostics are decided
//! there. `RUST_LOG`, `$XDG_DATA_HOME`, and `tracing`'s global default are all process-global,
//! and `crates/scratchbox-core/tests/config.rs` records why this repo does not set such
//! variables from a test thread: they are shared and the suite runs on parallel threads. So
//! each test gets its own child process, following `crates/scratchbox-cli/tests/support`.

#![allow(dead_code)]

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

/// A data home and a workspace beside it, both inside one temp directory.
pub struct Sandbox {
    _tmp: TempDir,
    pub root: PathBuf,
}

impl Sandbox {
    pub fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::create_dir_all(root.join("notes")).unwrap();
        Self { _tmp: tmp, root }
    }

    pub fn workspace(&self) -> PathBuf {
        self.root.join("notes")
    }

    /// What the child sees as `$XDG_DATA_HOME`.
    pub fn data_home(&self) -> PathBuf {
        self.root.join("data")
    }

    /// Where the child would put diagnostics, whether or not it does.
    ///
    /// Spelled out here rather than asked of `Config`, so a test asserting the directory is
    /// *absent* does not depend on the code under test to say where absent means.
    pub fn log_dir(&self) -> PathBuf {
        self.data_home().join("scratchbox").join("log")
    }

    /// The rotating appender's files, oldest name first.
    pub fn log_files(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(self.log_dir()) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// Draw one frame off-screen against `workspace`, then exit.
    ///
    /// `--bench-first-frame` is what makes this possible without a TTY: it renders to a
    /// `TestBackend` and returns, so startup — config resolution, `ensure_dirs`, and the
    /// diagnostics decision — all run for real while the terminal is never touched.
    ///
    /// `RUST_LOG` and `SCRATCHBOX_LOG_DIR` are cleared before `extra` is applied, so a
    /// developer with `export RUST_LOG=info` in their profile cannot change what these tests
    /// observe.
    pub fn run(&self, workspace: &PathBuf, extra: &[(&str, &str)]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_scratchbox-tui"));
        command
            .args(["--bench-first-frame", "--workspace"])
            .arg(workspace)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_DATA_HOME", self.data_home())
            .env_remove("RUST_LOG")
            .env_remove("SCRATCHBOX_LOG_DIR")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in extra {
            command.env(key, value);
        }

        command.output().unwrap()
    }
}

pub fn code(output: &Output) -> i32 {
    output.status.code().unwrap()
}

pub fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
