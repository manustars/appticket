//! The `hd` binary reads its secrets from files, as Compose and Kubernetes give them. Each test
//! gives it a database of its own, never the developer's.

#[allow(dead_code, reason = "not every helper is used here")]
mod common;

use common::{OwnDatabase, dir, hd};

#[test]
fn the_database_url_is_read_from_its_file() {
    let database = OwnDatabase::create("secrets");
    let dir = dir();
    let file = dir.join("database-url");
    std::fs::write(&file, format!("{}\n", database.url())).unwrap();

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
    // Refused before connecting; should it connect, it reaches a database of its own.
    let database = OwnDatabase::create("secrets");
    let dir = dir();
    let file = dir.join("database-url");
    let url = database.url();
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
