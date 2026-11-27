//! The `hd` binary connects to a database given as separate parameters, as a DBA hands them over:
//! a password used as it is, with no percent-encoding.

#[allow(dead_code, reason = "not every helper is used here")]
mod common;

use std::process::Output;

use common::{OwnDatabase, dir, hd};

/// A password with the characters a connection string would need percent-encoded.
const PASSWORD: &str = "desk@42/blue:sky";

/// A database of its own on the test server, owned by a PostgreSQL user of its own with
/// `PASSWORD`, and where the server is.
struct Parameters {
    database: OwnDatabase,
    host: String,
    port: String,
}

impl Parameters {
    fn create() -> Self {
        let database = OwnDatabase::create_with_owner("parameters", PASSWORD);
        let url = url::Url::parse(&database.url()).unwrap();
        Self {
            host: url.host_str().unwrap().to_owned(),
            port: url.port().unwrap_or(5432).to_string(),
            database,
        }
    }

    /// The separate parameters of the database, with `password` given by the caller.
    fn vars<'a>(&'a self, password: (&'a str, &'a str)) -> Vec<(&'a str, &'a str)> {
        vec![
            ("HD_DATABASE_HOST", self.host.as_str()),
            ("HD_DATABASE_PORT", self.port.as_str()),
            ("HD_DATABASE_NAME", self.database.name.as_str()),
            ("HD_DATABASE_USER", self.database.owner.as_deref().unwrap()),
            password,
        ]
    }
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn the_core_connects_with_separate_parameters_and_the_password_as_it_is() {
    let database = Parameters::create();
    let dir = dir();

    let migrated = hd(
        &dir,
        &["migrate"],
        &database.vars(("HD_DATABASE_PASSWORD", PASSWORD)),
    );
    // The server checks the password: a wrong one does not connect.
    let refused = hd(
        &dir,
        &["migrate"],
        &database.vars(("HD_DATABASE_PASSWORD", "desk@42/blue:sea")),
    );

    std::fs::remove_dir_all(dir).unwrap();
    assert_success(&migrated);
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("connecting to the database"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
}

#[test]
fn the_database_password_is_read_from_its_file() {
    let database = Parameters::create();
    let dir = dir();
    let file = dir.join("database-password");
    std::fs::write(&file, format!("{PASSWORD}\n")).unwrap();

    let migrated = hd(
        &dir,
        &["migrate"],
        &database.vars(("HD_DATABASE_PASSWORD_FILE", file.to_str().unwrap())),
    );

    std::fs::remove_dir_all(dir).unwrap();
    assert_success(&migrated);
}
