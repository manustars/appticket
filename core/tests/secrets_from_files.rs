//! The `hd` binary reads its secrets from files, as Compose and Kubernetes give them.

mod common;

use common::{database_url, dir, hd};

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
