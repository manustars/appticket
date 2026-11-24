//! Where the core's database is: a connection string, or separate parameters (ADR 0009).

use anyhow::Context;
use sqlx::postgres::PgConnectOptions;

use crate::secret;

/// The database settings of the command line, global so that every command that needs the
/// database reads the same ones.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct Settings {
    #[command(flatten)]
    url: secret::DatabaseUrl,
    /// Host name or address of the PostgreSQL server, when the database is given as separate
    /// parameters rather than as HD_DATABASE_URL.
    #[arg(
        long = "database-host",
        env = "HD_DATABASE_HOST",
        value_name = "HOST",
        global = true
    )]
    host: Option<String>,
    /// Port of the PostgreSQL server [default: 5432].
    #[arg(
        long = "database-port",
        env = "HD_DATABASE_PORT",
        value_name = "PORT",
        global = true
    )]
    port: Option<String>,
    /// Name of the database, dedicated to the help desk.
    #[arg(
        long = "database-name",
        env = "HD_DATABASE_NAME",
        value_name = "NAME",
        global = true
    )]
    name: Option<String>,
    /// User the core connects as, which owns the database and applies the migrations.
    #[arg(
        long = "database-user",
        env = "HD_DATABASE_USER",
        value_name = "USER",
        global = true
    )]
    user: Option<String>,
    #[command(flatten)]
    password: secret::DatabasePassword,
}

/// The database to connect to.
#[derive(PartialEq, Eq)]
pub enum Database {
    /// A connection string.
    Url(String),
    /// Separate parameters, used as they are: none needs percent-encoding.
    Parameters {
        host: String,
        port: u16,
        name: String,
        user: String,
        password: String,
    },
}

// By hand, so that neither the connection string nor the password ends up in a message.
impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Url(_) => f.write_str("Url(..)"),
            Self::Parameters {
                host,
                port,
                name,
                user,
                password: _,
            } => f
                .debug_struct("Parameters")
                .field("host", host)
                .field("port", port)
                .field("name", name)
                .field("user", user)
                .finish_non_exhaustive(),
        }
    }
}

impl Database {
    /// How to connect to the database.
    pub fn connect_options(&self) -> anyhow::Result<PgConnectOptions> {
        match self {
            Self::Url(url) => url
                .parse()
                .context("HD_DATABASE_URL is not a PostgreSQL connection string"),
            Self::Parameters {
                host,
                port,
                name,
                user,
                password,
            } => Ok(PgConnectOptions::new_without_pgpass()
                .host(host)
                .port(*port)
                .database(name)
                .username(user)
                .password(password)),
        }
    }
}

impl Settings {
    /// The database the settings give: a connection string, or every separate parameter but the
    /// port, which defaults to 5432. Giving both ways, or the separate parameters in part, is an
    /// error naming the variables.
    pub fn read(self) -> anyhow::Result<Database> {
        let url = self.url.read()?;
        let password = self.password.read()?;
        let host = given(self.host);
        let port = given(self.port);
        let name = given(self.name);
        let user = given(self.user);
        // The port counts as given, so that one left over beside a connection string is noticed.
        let parameters = [
            ("HD_DATABASE_HOST", host.is_some()),
            ("HD_DATABASE_PORT", port.is_some()),
            ("HD_DATABASE_NAME", name.is_some()),
            ("HD_DATABASE_USER", user.is_some()),
            (PASSWORD_VARIABLES, password.is_some()),
        ];
        let given_parameters: Vec<_> = parameters
            .iter()
            .filter(|(_, given)| *given)
            .map(|(variable, _)| *variable)
            .collect();
        if let Some(url) = url {
            if !given_parameters.is_empty() {
                anyhow::bail!(
                    "give the database as HD_DATABASE_URL (or HD_DATABASE_URL_FILE) or as separate \
                     parameters, not both: {} also set",
                    given_parameters.join(", ")
                );
            }
            return Ok(Database::Url(url));
        }
        let (Some(host), Some(name), Some(user), Some(password)) = (host, name, user, password)
        else {
            if given_parameters.is_empty() {
                anyhow::bail!(
                    "set HD_DATABASE_URL or HD_DATABASE_URL_FILE, or the separate parameters \
                     HD_DATABASE_HOST, HD_DATABASE_NAME, HD_DATABASE_USER and HD_DATABASE_PASSWORD \
                     (or HD_DATABASE_PASSWORD_FILE), with HD_DATABASE_PORT unless it is 5432 \
                     (hd --help names the matching options)"
                );
            }
            let missing: Vec<_> = parameters
                .iter()
                .filter(|(variable, given)| !given && *variable != "HD_DATABASE_PORT")
                .map(|(variable, _)| *variable)
                .collect();
            anyhow::bail!(
                "the database is given as separate parameters in part: set {} too",
                missing.join(", ")
            );
        };
        let port = match port {
            Some(port) => port
                .parse()
                .ok()
                .filter(|port| *port != 0)
                .with_context(|| {
                    format!("HD_DATABASE_PORT is {port}, which is not a port number")
                })?,
            None => DEFAULT_PORT,
        };
        Ok(Database::Parameters {
            host,
            port,
            name,
            user,
            password,
        })
    }
}

/// How messages name the password parameter, which can also be read from a file.
const PASSWORD_VARIABLES: &str = "HD_DATABASE_PASSWORD (or HD_DATABASE_PASSWORD_FILE)";

/// The port of the PostgreSQL server when HD_DATABASE_PORT is not given.
const DEFAULT_PORT: u16 = 5432;

/// A parameter trimmed of surrounding whitespace; a blank one counts as not given, as an unset
/// variable does.
fn given(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connection_string_alone_gives_the_database() {
        let settings = Settings {
            url: secret::DatabaseUrl::given("postgres://helpdesk:secret@db.example.com/helpdesk"),
            ..Settings::default()
        };

        assert_eq!(
            settings.read().unwrap(),
            Database::Url("postgres://helpdesk:secret@db.example.com/helpdesk".to_owned())
        );
    }

    /// The separate parameters, every one of them given.
    fn parameters() -> Settings {
        Settings {
            host: Some("db.example.com".to_owned()),
            port: Some("6543".to_owned()),
            name: Some("helpdesk".to_owned()),
            user: Some("helpdesk_core".to_owned()),
            password: secret::DatabasePassword::given("p@ss/w:rd"),
            ..Settings::default()
        }
    }

    #[test]
    fn separate_parameters_give_the_database_with_the_password_as_it_is() {
        assert_eq!(
            parameters().read().unwrap(),
            Database::Parameters {
                host: "db.example.com".to_owned(),
                port: 6543,
                name: "helpdesk".to_owned(),
                user: "helpdesk_core".to_owned(),
                password: "p@ss/w:rd".to_owned(),
            }
        );
        assert_eq!(
            Settings {
                port: None,
                ..parameters()
            }
            .read()
            .unwrap(),
            Database::Parameters {
                host: "db.example.com".to_owned(),
                port: 5432,
                name: "helpdesk".to_owned(),
                user: "helpdesk_core".to_owned(),
                password: "p@ss/w:rd".to_owned(),
            }
        );
    }

    fn dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("hd-database-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_connection_string_with_any_separate_parameter_is_refused_naming_it() {
        let dir = dir();
        let url_file = dir.join("database-url");
        std::fs::write(
            &url_file,
            "postgres://helpdesk:secret@db.example.com/helpdesk",
        )
        .unwrap();
        let password_file = dir.join("database-password");
        std::fs::write(&password_file, "p@ss/w:rd").unwrap();
        let some = |value: &str| Some(value.to_owned());
        let one_parameter = [
            (
                "HD_DATABASE_HOST",
                Settings {
                    host: some("db.example.com"),
                    ..Settings::default()
                },
            ),
            (
                "HD_DATABASE_PORT",
                Settings {
                    port: some("5432"),
                    ..Settings::default()
                },
            ),
            (
                "HD_DATABASE_NAME",
                Settings {
                    name: some("helpdesk"),
                    ..Settings::default()
                },
            ),
            (
                "HD_DATABASE_USER",
                Settings {
                    user: some("helpdesk_core"),
                    ..Settings::default()
                },
            ),
            (
                "HD_DATABASE_PASSWORD",
                Settings {
                    password: secret::DatabasePassword::given("p@ss/w:rd"),
                    ..Settings::default()
                },
            ),
            (
                "HD_DATABASE_PASSWORD",
                Settings {
                    password: secret::DatabasePassword::from_file(password_file.clone()),
                    ..Settings::default()
                },
            ),
        ];

        for url in [
            secret::DatabaseUrl::given("postgres://helpdesk:secret@db.example.com/helpdesk"),
            secret::DatabaseUrl::from_file(url_file.clone()),
        ] {
            for (variable, settings) in one_parameter.clone() {
                let error = Settings {
                    url: url.clone(),
                    ..settings
                }
                .read()
                .unwrap_err()
                .to_string();
                assert!(
                    error.contains("HD_DATABASE_URL") && error.contains(variable),
                    "{error}"
                );
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn separate_parameters_given_in_part_are_refused_naming_what_is_missing() {
        let cases = [
            (
                Settings {
                    password: secret::DatabasePassword::default(),
                    ..parameters()
                },
                vec!["HD_DATABASE_PASSWORD"],
            ),
            (
                Settings {
                    host: Some(" ".to_owned()),
                    user: None,
                    ..parameters()
                },
                vec!["HD_DATABASE_HOST", "HD_DATABASE_USER"],
            ),
            (
                Settings {
                    port: Some("5432".to_owned()),
                    ..Settings::default()
                },
                vec![
                    "HD_DATABASE_HOST",
                    "HD_DATABASE_NAME",
                    "HD_DATABASE_USER",
                    "HD_DATABASE_PASSWORD",
                ],
            ),
        ];

        for (settings, missing) in cases {
            let error = settings.read().unwrap_err().to_string();
            assert!(error.contains("in part"), "{error}");
            for variable in [
                "HD_DATABASE_HOST",
                "HD_DATABASE_PORT",
                "HD_DATABASE_NAME",
                "HD_DATABASE_USER",
                "HD_DATABASE_PASSWORD",
            ] {
                assert_eq!(
                    error.contains(variable),
                    missing.contains(&variable),
                    "{variable}: {error}"
                );
            }
        }
    }

    #[test]
    fn the_password_and_its_file_together_are_refused() {
        let dir = dir();
        let file = dir.join("database-password");
        std::fs::write(&file, "p@ss/w:rd").unwrap();
        let error = Settings {
            password: secret::DatabasePassword::given_and_from_file("p@ss/w:rd", file),
            ..parameters()
        }
        .read()
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("HD_DATABASE_PASSWORD ") && error.contains("HD_DATABASE_PASSWORD_FILE"),
            "{error}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn without_any_database_setting_both_ways_to_give_it_are_named() {
        let error = Settings::default().read().unwrap_err().to_string();
        for variable in [
            "HD_DATABASE_URL",
            "HD_DATABASE_URL_FILE",
            "HD_DATABASE_HOST",
            "HD_DATABASE_NAME",
            "HD_DATABASE_USER",
            "HD_DATABASE_PASSWORD",
        ] {
            assert!(error.contains(variable), "{variable}: {error}");
        }
    }

    #[test]
    fn a_port_that_is_not_a_port_number_is_refused() {
        for port in ["0", "65536", "postgres"] {
            let error = Settings {
                port: Some(port.to_owned()),
                ..parameters()
            }
            .read()
            .unwrap_err()
            .to_string();
            assert!(
                error.contains("HD_DATABASE_PORT") && error.contains(port),
                "{error}"
            );
        }
    }
}
