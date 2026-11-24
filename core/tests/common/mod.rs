//! Running the `hd` binary as a process, as an install starts it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The test database, as the other tests reach it.
pub fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .or_else(|_| dotenvy::var("DATABASE_URL"))
        .expect("DATABASE_URL is set for the tests")
}

/// A directory of its own, which is also where `hd` runs, so that no `.env` is found.
pub fn dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hd-process-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Runs `hd` with only the variables given, and those the system needs to start a process.
pub fn hd(dir: &Path, args: &[&str], vars: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hd"));
    command.args(args).current_dir(dir).env_clear();
    for name in ["PATH", "SystemRoot", "SYSTEMROOT", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command.envs(vars.iter().copied()).output().unwrap()
}
