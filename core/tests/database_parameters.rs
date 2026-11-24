//! The `hd` binary connects to a database given as separate parameters, as a DBA hands them over:
//! a password used as it is, with no percent-encoding.

mod common;

use std::process::Output;

use common::{database_url, dir, hd};
use sqlx::postgres::{PgConnectOptions, PgConnection};
use sqlx::{AssertSqlSafe, Connection};

/// A password with the characters a connection string would need percent-encoded.
const PASSWORD: &str = "desk@42/blue:sky";

/// A role with `PASSWORD` and a database it owns, made for one test on the test server and
/// dropped with it, also when the test fails.
struct OwnDatabase {
    host: String,
    port: String,
    role: String,
    name: String,
}

fn block_on<T>(future: impl Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

impl OwnDatabase {
    fn create() -> Self {
        let url = database_url();
        let options: PgConnectOptions = url.parse().unwrap();
        let id = uuid::Uuid::new_v4().simple();
        let database = Self {
            host: options.get_host().to_owned(),
            port: options.get_port().to_string(),
            role: format!("hd_parameters_{id}"),
            name: format!("hd_parameters_{id}"),
        };
        block_on(async {
            let mut admin = PgConnection::connect(&url).await.unwrap();
            sqlx::raw_sql(AssertSqlSafe(format!(
                r#"CREATE ROLE "{}" LOGIN PASSWORD '{PASSWORD}'"#,
                database.role
            )))
            .execute(&mut admin)
            .await
            .unwrap();
            sqlx::raw_sql(AssertSqlSafe(format!(
                r#"CREATE DATABASE "{}" OWNER "{}""#,
                database.name, database.role
            )))
            .execute(&mut admin)
            .await
            .unwrap();
        });
        database
    }

    /// The separate parameters of the database, with `password` given by the caller.
    fn vars<'a>(&'a self, password: (&'a str, &'a str)) -> Vec<(&'a str, &'a str)> {
        vec![
            ("HD_DATABASE_HOST", self.host.as_str()),
            ("HD_DATABASE_PORT", self.port.as_str()),
            ("HD_DATABASE_NAME", self.name.as_str()),
            ("HD_DATABASE_USER", self.role.as_str()),
            password,
        ]
    }
}

impl Drop for OwnDatabase {
    fn drop(&mut self) {
        let dropped = block_on(async {
            let mut admin = PgConnection::connect(&database_url()).await?;
            sqlx::raw_sql(AssertSqlSafe(format!(
                r#"DROP DATABASE IF EXISTS "{}" WITH (FORCE)"#,
                self.name
            )))
            .execute(&mut admin)
            .await?;
            sqlx::raw_sql(AssertSqlSafe(format!(
                r#"DROP ROLE IF EXISTS "{}""#,
                self.role
            )))
            .execute(&mut admin)
            .await?;
            Ok::<_, sqlx::Error>(())
        });
        if let Err(e) = dropped {
            eprintln!("dropping {} and {}: {e}", self.name, self.role);
        }
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
    let database = OwnDatabase::create();
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
    let database = OwnDatabase::create();
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
