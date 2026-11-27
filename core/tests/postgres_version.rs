//! At startup the `hd` binary reads the version of the PostgreSQL server and logs it.

#[allow(dead_code, reason = "not every helper is used here")]
mod common;

use common::{OwnDatabase, block_on, dir, hd};
use sqlx::{Connection, PgConnection};

#[test]
fn the_core_logs_the_version_of_the_database_server() {
    let database = OwnDatabase::create("version");
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
