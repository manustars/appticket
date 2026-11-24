//! The PostgreSQL versions the core runs on: 16 and later (ADR 0009).

use anyhow::Context;
use sqlx::PgPool;

/// The oldest major version of PostgreSQL the help desk supports.
pub const MINIMUM: u32 = 16;

/// A version of the PostgreSQL server, as its `server_version_num` setting gives it: 160004 for
/// 16.4, and 90624 for 9.6.24, from before 10, when the major version had two numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version(u32);

impl Version {
    /// The first number of the version, which is the whole major version from 10 on.
    fn major(self) -> u32 {
        self.0 / 10_000
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.major() >= 10 {
            write!(f, "{}.{}", self.major(), self.0 % 10_000)
        } else {
            write!(
                f,
                "{}.{}.{}",
                self.major(),
                self.0 / 100 % 100,
                self.0 % 100
            )
        }
    }
}

/// Reads the version of the database's server and logs it, and refuses a server older than
/// [`MINIMUM`], so that the core stops before a migration fails on it.
pub async fn check(db: &PgPool) -> anyhow::Result<()> {
    let (number, text): (String, String) = sqlx::query_as(
        "SELECT current_setting('server_version_num'), current_setting('server_version')",
    )
    .fetch_one(db)
    .await
    .context("reading the database server's version")?;
    // The text can name the build too, such as the distribution's package.
    tracing::info!(version = %text, "the database server is PostgreSQL");
    supported_version(&number)?;
    Ok(())
}

/// The server's version, from its `server_version_num` setting, refused when it is older than
/// [`MINIMUM`] or cannot be read.
fn supported_version(server_version_num: &str) -> anyhow::Result<Version> {
    let version = Version(server_version_num.trim().parse().map_err(|_| {
        anyhow::anyhow!(
            "cannot read the database server's version: its server_version_num is \
             '{server_version_num}', which is not a PostgreSQL version number"
        )
    })?);
    if version.major() < MINIMUM {
        anyhow::bail!(
            "the database server is PostgreSQL {version}, and the help desk needs PostgreSQL \
             {MINIMUM} or later"
        );
    }
    Ok(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sixteen_and_later_are_supported() {
        for (number, version) in [
            ("160000", "16.0"),
            ("160004", "16.4"),
            ("180001", "18.1"),
            ("190000", "19.0"),
        ] {
            assert_eq!(supported_version(number).unwrap().to_string(), version);
        }
    }

    #[test]
    fn an_older_server_is_refused_naming_its_version_and_the_minimum() {
        // Before 10, the second number was part of the major version too.
        for (number, version) in [("150010", "15.10"), ("120000", "12.0"), ("90624", "9.6.24")] {
            let error = supported_version(number).unwrap_err().to_string();
            assert!(
                error.contains(&format!("PostgreSQL {version},"))
                    && error.contains("PostgreSQL 16 or later"),
                "{error}"
            );
        }
    }

    #[test]
    fn a_version_that_cannot_be_read_is_refused_quoting_it() {
        for number in ["", "16.4", "sixteen"] {
            let error = supported_version(number).unwrap_err().to_string();
            assert!(
                error.contains(&format!("server_version_num is '{number}'")),
                "{error}"
            );
        }
    }
}
