//! The `hd` binary reads its secrets from files, as Compose and Kubernetes give them.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The test database, as the other tests reach it.
fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .or_else(|_| dotenvy::var("DATABASE_URL"))
        .expect("DATABASE_URL is set for the tests")
}

/// A directory of its own, which is also where `hd` runs, so that no `.env` is found.
fn dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hd-secrets-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Runs `hd` with only the variables given, and those the system needs to start a process.
fn hd(dir: &Path, args: &[&str], vars: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hd"));
    command.args(args).current_dir(dir).env_clear();
    for name in ["PATH", "SystemRoot", "SYSTEMROOT", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command.envs(vars.iter().copied()).output().unwrap()
}

#[test]
fn the_database_url_is_read_from_its_file() {
    let dir = dir();
    let file = dir.join("database-url");
    std::fs::write(&file, format!("{}\n", database_url())).unwrap();

    let output = hd(
        &dir,
        &["migrate"],
        &[("HD_DATABASE_URL_FILE", file.to_str().unwrap())],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn the_database_url_and_its_file_together_are_refused() {
    let dir = dir();
    let file = dir.join("database-url");
    let url = database_url();
    std::fs::write(&file, &url).unwrap();

    let output = hd(
        &dir,
        &["migrate"],
        &[
            ("HD_DATABASE_URL", url.as_str()),
            ("HD_DATABASE_URL_FILE", file.to_str().unwrap()),
        ],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("HD_DATABASE_URL ") && stderr.contains("HD_DATABASE_URL_FILE"),
        "{stderr}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}
