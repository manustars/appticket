//! Running the `hd` binary as a process, as an install starts it, against a database of its own.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use sqlx::{AssertSqlSafe, Connection, PgConnection};

/// The test server, as the other tests reach it. Its own database is the developer's: a test
/// that runs `hd` connects to an [`OwnDatabase`] instead.
pub fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .or_else(|_| dotenvy::var("DATABASE_URL"))
        .expect("DATABASE_URL is set for the tests")
}

pub fn block_on<T>(future: impl Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

/// A database made for one test on the test server, and dropped with it, also when the test
/// fails; with a PostgreSQL user of its own that owns it, if the test asks for one.
pub struct OwnDatabase {
    pub name: String,
    pub owner: Option<String>,
}

impl OwnDatabase {
    /// A database named after `purpose`, owned by the user of `DATABASE_URL`.
    pub fn create(purpose: &str) -> Self {
        Self::make(purpose, None)
    }

    /// A database named after `purpose`, owned by a user of the same name with `password`.
    pub fn create_with_owner(purpose: &str, password: &str) -> Self {
        Self::make(purpose, Some(password))
    }

    fn make(purpose: &str, password: Option<&str>) -> Self {
        let name = format!("hd_{purpose}_{}", uuid::Uuid::new_v4().simple());
        let database = Self {
            owner: password.map(|_| name.clone()),
            name,
        };
        block_on(async {
            let mut admin = PgConnection::connect(&database_url()).await.unwrap();
            let mut create = format!(r#"CREATE DATABASE "{}""#, database.name);
            if let (Some(owner), Some(password)) = (&database.owner, password) {
                sqlx::raw_sql(AssertSqlSafe(format!(
                    r#"CREATE ROLE "{owner}" LOGIN PASSWORD '{password}'"#
                )))
                .execute(&mut admin)
                .await
                .unwrap();
                create.push_str(&format!(r#" OWNER "{owner}""#));
            }
            sqlx::raw_sql(AssertSqlSafe(create))
                .execute(&mut admin)
                .await
                .unwrap();
        });
        database
    }

    /// The test server's connection string, pointing at this database.
    pub fn url(&self) -> String {
        let mut url = url::Url::parse(&database_url()).unwrap();
        url.set_path(&self.name);
        url.into()
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
            if let Some(owner) = &self.owner {
                sqlx::raw_sql(AssertSqlSafe(format!(r#"DROP ROLE IF EXISTS "{owner}""#)))
                    .execute(&mut admin)
                    .await?;
            }
            Ok::<_, sqlx::Error>(())
        });
        if let Err(e) = dropped {
            eprintln!("dropping {}: {e}", self.name);
        }
    }
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
