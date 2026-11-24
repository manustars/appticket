//! At startup the `hd` binary reads the version of the PostgreSQL server and logs it.

mod common;

use common::{database_url, dir, hd};
use sqlx::{AssertSqlSafe, Connection, PgConnection};

fn block_on<T>(future: impl Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

/// A database made for one test on the test server, and dropped with it, also when the test
/// fails.
struct OwnDatabase {
    name: String,
}

impl OwnDatabase {
    fn create() -> Self {
        let database = Self {
            name: format!("hd_version_{}", uuid::Uuid::new_v4().simple()),
        };
        block_on(async {
            let mut admin = PgConnection::connect(&database_url()).await.unwrap();
            sqlx::raw_sql(AssertSqlSafe(format!(
                r#"CREATE DATABASE "{}""#,
                database.name
            )))
            .execute(&mut admin)
            .await
            .unwrap();
        });
        database
    }

    /// The test server's connection string, pointing at this database.
    fn url(&self) -> String {
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
            Ok::<_, sqlx::Error>(())
        });
        if let Err(e) = dropped {
            eprintln!("dropping {}: {e}", self.name);
        }
    }
}

#[test]
fn the_core_logs_the_version_of_the_database_server() {
    let database = OwnDatabase::create();
    let server_version: String = block_on(async {
        let mut db = PgConnection::connect(&database.url()).await.unwrap();
        sqlx::query_scalar("SELECT current_setting('server_version')")
            .fetch_one(&mut db)
            .await
            .unwrap()
    });
    let dir = dir();

    let migrated = hd(&dir, &["migrate"], &[("HD_DATABASE_URL", &database.url())]);

    std::fs::remove_dir_all(dir).unwrap();
    let log = String::from_utf8_lossy(&migrated.stdout);
    assert!(
        migrated.status.success(),
        "{}",
        String::from_utf8_lossy(&migrated.stderr)
    );
    assert!(
        log.lines()
            .any(|line| line.contains("PostgreSQL") && line.contains(&server_version)),
        "{server_version} is not in the log: {log}"
    );
}
