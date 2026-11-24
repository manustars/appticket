//! Where the core's database is, a connection string or separate parameters, and how the
//! connection to it is protected: verified TLS for a database on another host (ADR 0009).

use std::net::IpAddr;

use anyhow::Context;
use sqlx::postgres::{PgConnectOptions, PgSslMode};

use crate::secret;

/// The database settings of the command line, global so that every command that needs the
/// database reads the same ones. Each argument's id is its variable, as with the secrets: a global
/// argument with the id `name` would take the `name` of a command such as `hd customer add`.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct Settings {
    #[command(flatten)]
    url: secret::DatabaseUrl,
    /// Host name or address of the PostgreSQL server, when the database is given as separate
    /// parameters rather than as HD_DATABASE_URL.
    #[arg(
        id = "HD_DATABASE_HOST",
        long = "database-host",
        env = "HD_DATABASE_HOST",
        value_name = "HOST",
        global = true
    )]
    host: Option<String>,
    /// Port of the PostgreSQL server [default: 5432].
    #[arg(
        id = "HD_DATABASE_PORT",
        long = "database-port",
        env = "HD_DATABASE_PORT",
        value_name = "PORT",
        global = true
    )]
    port: Option<String>,
    /// Name of the database, dedicated to the help desk.
    #[arg(
        id = "HD_DATABASE_NAME",
        long = "database-name",
        env = "HD_DATABASE_NAME",
        value_name = "NAME",
        global = true
    )]
    name: Option<String>,
    /// User the core connects as, which owns the database and applies the migrations.
    #[arg(
        id = "HD_DATABASE_USER",
        long = "database-user",
        env = "HD_DATABASE_USER",
        value_name = "USER",
        global = true
    )]
    user: Option<String>,
    #[command(flatten)]
    password: secret::DatabasePassword,
    /// TLS to the database: verify-full, verify-ca, require, prefer or disable. Applies to the
    /// separate parameters and to an HD_DATABASE_URL without its own sslmode; giving it beside a
    /// URL with sslmode is refused. Without it: verify-full for a database on another host, and
    /// prefer (TLS when the server offers it, unverified) for one on this machine (localhost, a
    /// loopback address or a Unix socket). Anything but verify-full for another host is logged
    /// as a warning at startup.
    #[arg(
        id = "HD_DATABASE_SSLMODE",
        long = "database-sslmode",
        env = "HD_DATABASE_SSLMODE",
        value_name = "MODE",
        global = true
    )]
    sslmode: Option<String>,
    /// PEM file of certificate authorities to trust for the database's certificate, besides the
    /// public web PKI's: for a server with a private authority. Only with verify-full or
    /// verify-ca.
    #[arg(
        id = "HD_DATABASE_CA_FILE",
        long = "database-ca-file",
        env = "HD_DATABASE_CA_FILE",
        value_name = "FILE",
        global = true
    )]
    ca_file: Option<String>,
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
    /// How to connect to the database, before its TLS is chosen.
    fn options(&self) -> anyhow::Result<PgConnectOptions> {
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
    /// How to connect to the database the settings give, TLS included. Logs a warning when a
    /// database on another host is reached without verify-full.
    pub fn connect_options(self) -> anyhow::Result<PgConnectOptions> {
        // sqlx reads it only when it connects, and says then neither which file nor what for.
        if let Some(ca_file) = given(self.ca_file.clone()) {
            std::fs::read(&ca_file)
                .with_context(|| format!("reading HD_DATABASE_CA_FILE {ca_file}"))?;
        }
        let (options, warning) = self.tls_options()?;
        if let Some(warning) = warning {
            tracing::warn!("{warning}");
        }
        Ok(options)
    }

    /// How to connect to the database, and the warning to log about its TLS.
    fn tls_options(self) -> anyhow::Result<(PgConnectOptions, Option<String>)> {
        let mode = given(self.sslmode.clone())
            .map(|mode| {
                SslMode::from_variable(&mode).with_context(|| {
                    format!(
                        "HD_DATABASE_SSLMODE is {mode}, which is not one of verify-full, \
                         verify-ca, require, prefer and disable"
                    )
                })
            })
            .transpose()?;
        let ca_file = given(self.ca_file.clone());
        let database = self.read()?;
        let mut options = database.options()?;
        let url = match &database {
            Database::Url(url) => Some(UrlTls::of(url)?),
            Database::Parameters { .. } => None,
        };
        // sqlx dials PGHOSTADDR, when set, unless the connection string gives a hostaddr.
        let hostaddr = url.as_ref().and_then(|url| url.hostaddr).or_else(|| {
            std::env::var("PGHOSTADDR")
                .ok()
                .and_then(|hostaddr| hostaddr.parse().ok())
        });
        let tls = choose_tls(TlsSettings {
            place: Place::of(&options, hostaddr),
            mode,
            url_mode: url
                .as_ref()
                .filter(|url| url.carries_mode)
                .map(|_| SslMode::from(options.get_ssl_mode())),
            ca_file: ca_file.is_some(),
            url_root_cert: url.is_some_and(|url| url.carries_root_cert),
        })?;
        if let Some(mode) = tls.mode {
            options = options.ssl_mode(mode.into());
        }
        if let Some(ca_file) = ca_file {
            options = options.ssl_root_cert(ca_file);
        }
        Ok((options, tls.warning))
    }

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

/// How the connection to the database is protected, named as in libpq's `sslmode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SslMode {
    /// TLS, with a certificate from a trusted authority for the host dialled.
    VerifyFull,
    /// TLS, with a certificate from a trusted authority, for whatever host.
    VerifyCa,
    /// TLS, with any certificate.
    Require,
    /// TLS if the server offers it, with any certificate.
    Prefer,
    /// No TLS: sqlx does not try TLS for `allow`, which only a connection string can carry.
    Allow,
    /// No TLS.
    Disable,
}

impl SslMode {
    /// A mode HD_DATABASE_SSLMODE can give, as libpq names it in any case; not `allow`, which
    /// sqlx treats as `disable`.
    fn from_variable(mode: &str) -> Option<Self> {
        [
            Self::VerifyFull,
            Self::VerifyCa,
            Self::Require,
            Self::Prefer,
            Self::Disable,
        ]
        .into_iter()
        .find(|known| known.name().eq_ignore_ascii_case(mode))
    }

    /// The mode as `sslmode` names it.
    fn name(self) -> &'static str {
        match self {
            Self::VerifyFull => "verify-full",
            Self::VerifyCa => "verify-ca",
            Self::Require => "require",
            Self::Prefer => "prefer",
            Self::Allow => "allow",
            Self::Disable => "disable",
        }
    }

    /// What the mode leaves unchecked, compared with `verify-full`.
    fn weakness(self) -> &'static str {
        match self {
            Self::VerifyFull => "nothing",
            Self::VerifyCa => "the server's certificate is not checked against its host name",
            Self::Require => "the server's certificate is not verified",
            Self::Prefer => {
                "the connection is encrypted only if the server offers TLS, and the server's \
                 certificate is not verified"
            }
            Self::Allow | Self::Disable => "the connection is not encrypted",
        }
    }
}

impl From<PgSslMode> for SslMode {
    fn from(mode: PgSslMode) -> Self {
        match mode {
            PgSslMode::VerifyFull => Self::VerifyFull,
            PgSslMode::VerifyCa => Self::VerifyCa,
            PgSslMode::Require => Self::Require,
            PgSslMode::Prefer => Self::Prefer,
            PgSslMode::Allow => Self::Allow,
            PgSslMode::Disable => Self::Disable,
        }
    }
}

impl From<SslMode> for PgSslMode {
    fn from(mode: SslMode) -> Self {
        match mode {
            SslMode::VerifyFull => Self::VerifyFull,
            SslMode::VerifyCa => Self::VerifyCa,
            SslMode::Require => Self::Require,
            SslMode::Prefer => Self::Prefer,
            SslMode::Allow => Self::Allow,
            SslMode::Disable => Self::Disable,
        }
    }
}

/// A parameter trimmed of surrounding whitespace; a blank one counts as not given, as an unset
/// variable does.
fn given(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// Where the database is, which decides the TLS mode when none is given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Place {
    /// On the core's own host: a loopback address, `localhost` or a Unix socket.
    OwnHost,
    /// On another host.
    Elsewhere,
}

impl Place {
    /// Where sqlx connects with `options`: a Unix socket, or the host's address, which
    /// `hostaddr` overrides. A host on this machine dialled at another address is elsewhere,
    /// and so is another host dialled through a tunnel on this machine, whose certificate
    /// verify-full checks against the host's name.
    fn of(options: &PgConnectOptions, hostaddr: Option<IpAddr>) -> Self {
        let host = options.get_host();
        // sqlx takes a host starting with a slash as a socket's directory.
        if options.get_socket().is_some() || host.starts_with('/') {
            return Self::OwnHost;
        }
        if crate::imap::is_local(host) && hostaddr.is_none_or(|address| address.is_loopback()) {
            Self::OwnHost
        } else {
            Self::Elsewhere
        }
    }
}

/// What a connection string carries about TLS, with the spellings sqlx accepts.
struct UrlTls {
    /// Whether it carries an `sslmode` (or `ssl-mode`).
    carries_mode: bool,
    /// Whether it carries authorities to trust: `sslrootcert`, `ssl-root-cert` or `ssl-ca`.
    carries_root_cert: bool,
    /// The address to dial instead of the host's, the last one given as sqlx takes it.
    hostaddr: Option<IpAddr>,
}

impl UrlTls {
    fn of(url: &str) -> anyhow::Result<Self> {
        let url = url::Url::parse(url).context("HD_DATABASE_URL is not a connection string")?;
        let mut tls = Self {
            carries_mode: false,
            carries_root_cert: false,
            hostaddr: None,
        };
        for (key, value) in url.query_pairs() {
            match &*key {
                "sslmode" | "ssl-mode" => tls.carries_mode = true,
                "sslrootcert" | "ssl-root-cert" | "ssl-ca" => tls.carries_root_cert = true,
                "hostaddr" => tls.hostaddr = value.parse().ok(),
                _ => {}
            }
        }
        Ok(tls)
    }
}

/// What decides the TLS of the connection to the database.
#[derive(Clone, Copy, Debug)]
struct TlsSettings {
    place: Place,
    /// HD_DATABASE_SSLMODE.
    mode: Option<SslMode>,
    /// The `sslmode` the connection string carries.
    url_mode: Option<SslMode>,
    /// Whether HD_DATABASE_CA_FILE is given.
    ca_file: bool,
    /// Whether the connection string carries its own authorities (`sslrootcert`).
    url_root_cert: bool,
}

/// The TLS of the connection to the database.
#[derive(Debug, PartialEq, Eq)]
struct Tls {
    /// The mode to connect with; none leaves sqlx's default (`prefer`), as for a database on the
    /// core's own host before TLS had a default.
    mode: Option<SslMode>,
    /// What to log as a warning at startup.
    warning: Option<String>,
}

/// The TLS mode the settings give, and the warning to log for a database on another host reached
/// without `verify-full`. Giving the mode in both the connection string and HD_DATABASE_SSLMODE
/// is an error, as is a CA file that the mode would not use.
fn choose_tls(settings: TlsSettings) -> anyhow::Result<Tls> {
    let (mode, set_by) = match (settings.mode, settings.url_mode) {
        (Some(mode), Some(url_mode)) => anyhow::bail!(
            "HD_DATABASE_URL carries sslmode={} and HD_DATABASE_SSLMODE is {}: give the mode in \
             one of them, not both",
            url_mode.name(),
            mode.name()
        ),
        (Some(mode), None) => (Some(mode), "set by HD_DATABASE_SSLMODE"),
        (None, Some(url_mode)) => (Some(url_mode), "set by HD_DATABASE_URL"),
        (None, None) => match settings.place {
            Place::OwnHost => (None, "the default"),
            Place::Elsewhere => (Some(SslMode::VerifyFull), "the default"),
        },
    };
    if settings.ca_file {
        if settings.url_root_cert {
            anyhow::bail!(
                "HD_DATABASE_URL carries sslrootcert and HD_DATABASE_CA_FILE is set: give the \
                 authorities to trust in one of them, not both"
            );
        }
        if !matches!(mode, Some(SslMode::VerifyFull | SslMode::VerifyCa)) {
            anyhow::bail!(
                "HD_DATABASE_CA_FILE is used only to verify the server's certificate, which \
                 sslmode {} ({set_by}) does not do: use verify-full",
                mode.unwrap_or(SslMode::Prefer).name()
            );
        }
    }
    let warning = match (settings.place, mode) {
        (Place::Elsewhere, Some(mode)) if mode != SslMode::VerifyFull => Some(format!(
            "the database is not on this machine and is reached with sslmode {} ({set_by}): {}; \
             verify-full is the default",
            mode.name(),
            mode.weakness(),
        )),
        _ => None,
    };
    Ok(Tls { mode, warning })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The TLS settings when nothing is given, for a database at `place`.
    fn nothing_given(place: Place) -> TlsSettings {
        TlsSettings {
            place,
            mode: None,
            url_mode: None,
            ca_file: false,
            url_root_cert: false,
        }
    }

    #[test]
    fn a_database_on_another_host_is_verified_by_default() {
        assert_eq!(
            choose_tls(nothing_given(Place::Elsewhere)).unwrap(),
            Tls {
                mode: Some(SslMode::VerifyFull),
                warning: None,
            }
        );
    }

    #[test]
    fn a_database_on_the_cores_own_host_keeps_sqlxs_default() {
        assert_eq!(
            choose_tls(nothing_given(Place::OwnHost)).unwrap(),
            Tls {
                mode: None,
                warning: None,
            }
        );
    }

    #[test]
    fn the_mode_of_the_variable_is_used_and_a_weaker_one_elsewhere_is_warned_of_by_name() {
        let modes = [
            (SslMode::VerifyFull, "verify-full"),
            (SslMode::VerifyCa, "verify-ca"),
            (SslMode::Require, "require"),
            (SslMode::Prefer, "prefer"),
            (SslMode::Disable, "disable"),
        ];
        for (mode, name) in modes {
            for place in [Place::OwnHost, Place::Elsewhere] {
                let tls = choose_tls(TlsSettings {
                    mode: Some(mode),
                    ..nothing_given(place)
                })
                .unwrap();
                assert_eq!(tls.mode, Some(mode));
                match &tls.warning {
                    Some(warning) => {
                        assert_eq!(place, Place::Elsewhere, "{name}: {warning}");
                        assert_ne!(mode, SslMode::VerifyFull, "{warning}");
                        assert!(
                            warning.contains(&format!("sslmode {name} "))
                                && warning.contains("HD_DATABASE_SSLMODE"),
                            "{warning}"
                        );
                    }
                    None => assert!(
                        place == Place::OwnHost || mode == SslMode::VerifyFull,
                        "{name} elsewhere is not warned of"
                    ),
                }
            }
        }
    }

    #[test]
    fn the_mode_the_connection_string_carries_is_kept_and_a_weaker_one_elsewhere_is_warned_of() {
        let carried = |place, url_mode| {
            choose_tls(TlsSettings {
                url_mode: Some(url_mode),
                ..nothing_given(place)
            })
            .unwrap()
        };

        let required = carried(Place::Elsewhere, SslMode::Require);
        assert_eq!(required.mode, Some(SslMode::Require));
        let warning = required.warning.unwrap();
        assert!(
            warning.contains("sslmode require ") && warning.contains("HD_DATABASE_URL"),
            "{warning}"
        );
        let allowed = carried(Place::Elsewhere, SslMode::Allow).warning.unwrap();
        assert!(allowed.contains("sslmode allow "), "{allowed}");
        assert_eq!(
            carried(Place::Elsewhere, SslMode::VerifyFull),
            Tls {
                mode: Some(SslMode::VerifyFull),
                warning: None,
            }
        );
        assert_eq!(
            carried(Place::OwnHost, SslMode::Disable),
            Tls {
                mode: Some(SslMode::Disable),
                warning: None,
            }
        );
    }

    #[test]
    fn a_mode_in_both_the_connection_string_and_the_variable_is_refused() {
        for place in [Place::OwnHost, Place::Elsewhere] {
            let error = choose_tls(TlsSettings {
                mode: Some(SslMode::VerifyFull),
                url_mode: Some(SslMode::VerifyFull),
                ..nothing_given(place)
            })
            .unwrap_err()
            .to_string();
            assert!(
                error.contains("HD_DATABASE_URL") && error.contains("HD_DATABASE_SSLMODE"),
                "{error}"
            );
        }
    }

    #[test]
    fn a_ca_file_goes_with_a_mode_that_verifies_the_certificate() {
        let with_ca_file = |settings| {
            choose_tls(TlsSettings {
                ca_file: true,
                ..settings
            })
        };
        for verifying in [SslMode::VerifyFull, SslMode::VerifyCa] {
            with_ca_file(TlsSettings {
                mode: Some(verifying),
                ..nothing_given(Place::OwnHost)
            })
            .unwrap();
            with_ca_file(TlsSettings {
                url_mode: Some(verifying),
                ..nothing_given(Place::OwnHost)
            })
            .unwrap();
        }
        assert_eq!(
            with_ca_file(nothing_given(Place::Elsewhere)).unwrap().mode,
            Some(SslMode::VerifyFull)
        );

        // Another mode would not use it, and the default on the core's own host is one of them.
        let not_verifying = [
            TlsSettings {
                mode: Some(SslMode::Require),
                ..nothing_given(Place::Elsewhere)
            },
            TlsSettings {
                url_mode: Some(SslMode::Prefer),
                ..nothing_given(Place::Elsewhere)
            },
            nothing_given(Place::OwnHost),
        ];
        for settings in not_verifying {
            let error = with_ca_file(settings).unwrap_err().to_string();
            assert!(
                error.contains("HD_DATABASE_CA_FILE") && error.contains("verify-full"),
                "{error}"
            );
        }
    }

    #[test]
    fn a_ca_file_beside_the_authorities_the_connection_string_carries_is_refused() {
        let error = choose_tls(TlsSettings {
            url_mode: Some(SslMode::VerifyFull),
            url_root_cert: true,
            ca_file: true,
            ..nothing_given(Place::Elsewhere)
        })
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("HD_DATABASE_CA_FILE") && error.contains("sslrootcert"),
            "{error}"
        );
    }

    /// The settings of a connection string.
    fn url(url: &str) -> Settings {
        Settings {
            url: secret::DatabaseUrl::given(url),
            ..Settings::default()
        }
    }

    /// The mode a connection is made with, and whether a warning is logged for it.
    fn connected_with(settings: Settings) -> (&'static str, bool) {
        let (options, warning) = settings.tls_options().unwrap();
        let mode = match options.get_ssl_mode() {
            PgSslMode::VerifyFull => "verify-full",
            PgSslMode::VerifyCa => "verify-ca",
            PgSslMode::Require => "require",
            PgSslMode::Prefer => "prefer",
            PgSslMode::Allow => "allow",
            PgSslMode::Disable => "disable",
        };
        (mode, warning.is_some())
    }

    #[test]
    fn the_cores_own_host_is_where_sqlx_connects() {
        // sqlx's default, prefer, unless PGSSLMODE says otherwise, as the tests do not.
        for own_host in [
            "postgres://hd:hd@localhost/hd",
            "postgres://hd:hd@LOCALHOST:55432/hd",
            "postgres://hd:hd@127.0.0.1/hd",
            "postgres://hd:hd@127.0.0.53/hd",
            "postgres://hd:hd@[::1]/hd",
            "postgres://hd@%2Fvar%2Frun%2Fpostgresql/hd",
            "postgres:///hd?host=/var/run/postgresql",
            "postgres://hd:hd@db.example.com/hd?host=/var/run/postgresql",
            "postgres://hd:hd@db.example.com/hd?host=localhost",
            "postgres://hd:hd@localhost/hd?hostaddr=127.0.0.1",
        ] {
            assert_eq!(
                connected_with(url(own_host)),
                ("prefer", false),
                "{own_host}"
            );
        }
        for elsewhere in [
            "postgres://hd:hd@db.example.com/hd",
            "postgres://hd:hd@192.0.2.10/hd",
            "postgres://hd:hd@[2001:db8::10]/hd",
            "postgres://hd-db-rw.helpdesk:5432/hd",
            "postgres://hd:hd@localhost.example.com/hd",
            "postgres://hd:hd@localhost/hd?host=db.example.com",
            // Dialled at another address than the host's...
            "postgres://hd:hd@localhost/hd?hostaddr=192.0.2.10",
            // ...or through a tunnel on this machine to a host verified by its name.
            "postgres://hd:hd@db.example.com/hd?hostaddr=127.0.0.1",
        ] {
            assert_eq!(
                connected_with(url(elsewhere)),
                ("verify-full", false),
                "{elsewhere}"
            );
        }
        assert_eq!(connected_with(parameters()), ("verify-full", false));
        for own_host in ["localhost", "127.0.0.1", "::1", "/var/run/postgresql"] {
            assert_eq!(
                connected_with(Settings {
                    host: Some(own_host.to_owned()),
                    ..parameters()
                }),
                ("prefer", false),
                "{own_host}"
            );
        }
    }

    #[test]
    fn the_sslmode_variable_applies_to_both_ways_of_giving_the_database() {
        let require = |settings| Settings {
            sslmode: Some("require".to_owned()),
            ..settings
        };
        assert_eq!(
            connected_with(require(url("postgres://hd:hd@db.example.com/hd"))),
            ("require", true)
        );
        assert_eq!(connected_with(require(parameters())), ("require", true));
        assert_eq!(
            connected_with(require(url("postgres://hd:hd@127.0.0.1/hd"))),
            ("require", false)
        );
        // Blank, as an unset variable.
        assert_eq!(
            connected_with(Settings {
                sslmode: Some(" ".to_owned()),
                ..parameters()
            }),
            ("verify-full", false)
        );
        let error = Settings {
            sslmode: Some("verify_full".to_owned()),
            ..parameters()
        }
        .tls_options()
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("HD_DATABASE_SSLMODE") && error.contains("verify_full"),
            "{error}"
        );
    }

    #[test]
    fn the_sslmode_of_the_connection_string_is_read_in_both_spellings_sqlx_knows() {
        for carried in [
            "postgres://hd:hd@db.example.com/hd?sslmode=require",
            "postgres://hd:hd@db.example.com/hd?ssl-mode=require",
            "postgres://hd:hd@db.example.com/hd?sslmode=disable&ssl-mode=require",
        ] {
            assert_eq!(connected_with(url(carried)), ("require", true), "{carried}");
            let error = Settings {
                sslmode: Some("verify-full".to_owned()),
                ..url(carried)
            }
            .tls_options()
            .unwrap_err()
            .to_string();
            assert!(error.contains("not both"), "{error}");
        }
        assert_eq!(
            connected_with(url("postgres://hd:hd@db.example.com/hd?sslmode=verify-ca")),
            ("verify-ca", true)
        );
    }

    #[test]
    fn the_ca_file_is_refused_beside_any_spelling_of_the_connection_strings_own() {
        for spelling in ["sslrootcert", "ssl-root-cert", "ssl-ca"] {
            let error = Settings {
                ca_file: Some("/etc/hd/database-ca/ca.crt".to_owned()),
                ..url(&format!(
                    "postgres://hd:hd@db.example.com/hd?{spelling}=/etc/ssl/db-ca.crt"
                ))
            }
            .tls_options()
            .unwrap_err()
            .to_string();
            assert!(error.contains("HD_DATABASE_CA_FILE"), "{spelling}: {error}");
        }
        let (options, warning) = Settings {
            ca_file: Some("/etc/hd/database-ca/ca.crt".to_owned()),
            ..parameters()
        }
        .tls_options()
        .unwrap();
        assert!(matches!(options.get_ssl_mode(), PgSslMode::VerifyFull));
        assert!(warning.is_none());
    }

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

    #[test]
    fn a_ca_file_that_cannot_be_read_is_named() {
        let dir = dir();
        // Missing, or a folder where a file should be.
        let folder = dir.join("database-ca-folder.crt");
        std::fs::create_dir(&folder).unwrap();
        for ca_file in [dir.join("database-ca.crt"), folder] {
            let error = Settings {
                ca_file: Some(ca_file.display().to_string()),
                ..parameters()
            }
            .connect_options()
            .unwrap_err();
            let error = format!("{error:#}");
            assert!(
                error.contains("HD_DATABASE_CA_FILE")
                    && error.contains(&ca_file.display().to_string()),
                "{error}"
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
