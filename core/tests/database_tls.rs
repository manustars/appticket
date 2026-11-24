//! The `hd` binary verifies the database's certificate against the authorities of
//! HD_DATABASE_CA_FILE, as for a database whose certificate comes from a private authority.

#[allow(dead_code, reason = "not every helper is used here")]
mod common;

use common::{dir, hd};

/// The TLS test database and the file of its authority, as `scripts/tls-test-database.sh`
/// prints them. Without them the test is skipped, but not in CI, which always starts it.
fn tls_database() -> Option<(String, String)> {
    let var = |name| std::env::var(name).or_else(|_| dotenvy::var(name)).ok();
    match (
        var("HD_TEST_TLS_DATABASE_URL"),
        var("HD_TEST_TLS_DATABASE_CA_FILE"),
    ) {
        (Some(url), Some(ca_file)) => Some((url, ca_file)),
        _ => {
            assert!(
                std::env::var_os("CI").is_none(),
                "CI starts the TLS test database: set HD_TEST_TLS_DATABASE_URL and \
                 HD_TEST_TLS_DATABASE_CA_FILE"
            );
            eprintln!("skipped: no TLS test database (scripts/tls-test-database.sh)");
            None
        }
    }
}

#[test]
#[ignore = "needs the TLS test database (scripts/tls-test-database.sh)"]
fn the_core_verifies_the_databases_certificate_against_the_ca_file() {
    let Some((url, ca_file)) = tls_database() else {
        return;
    };
    let dir = dir();

    let verified = hd(
        &dir,
        &["migrate"],
        &[
            ("HD_DATABASE_URL", url.as_str()),
            ("HD_DATABASE_SSLMODE", "verify-full"),
            ("HD_DATABASE_CA_FILE", ca_file.as_str()),
        ],
    );
    // Without the test authority the certificate is from nobody the core trusts.
    let unverified = hd(
        &dir,
        &["migrate"],
        &[
            ("HD_DATABASE_URL", url.as_str()),
            ("HD_DATABASE_SSLMODE", "verify-full"),
        ],
    );

    std::fs::remove_dir_all(dir).unwrap();
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );
    let refused = String::from_utf8_lossy(&unverified.stderr);
    assert!(!unverified.status.success());
    assert!(refused.contains("UnknownIssuer"), "{refused}");
}
