//! Secrets given in a variable or read from a file.
//!
//! Every secret of the core can be read from a file, so that Compose keeps secrets out of its
//! environment file and Kubernetes can mount them (ADR 0009). A secret is an argument whose
//! environment value is hidden (`hide_env_values`), next to a `_FILE` argument naming the file;
//! a test checks that every such argument has one.

use std::path::{Path, PathBuf};

use anyhow::Context;

/// Declares the arguments of a secret: `--<long>` / `<ENV>` with its value hidden, and
/// `--<long>-file` / `<ENV>_FILE`. The value is reached only through `read`, so that giving both
/// is refused wherever the secret is used.
macro_rules! secret {
    ($name:ident {
        long: $long:literal,
        env: $env:literal,
        value_name: $value_name:literal,
        help: $help:literal $(,
        global: $global:literal)? $(,)?
    }) => {
        #[derive(clap::Args, Debug, Clone, Default)]
        pub struct $name {
            #[arg(id = $env, long = $long, env = $env, hide_env_values = true,
                value_name = $value_name, help = $help $(, global = $global)?)]
            value: Option<String>,
            #[arg(id = concat!($env, "_FILE"), long = concat!($long, "-file"),
                env = concat!($env, "_FILE"), value_name = "FILE",
                help = concat!("A file holding ", $env, ", such as a mounted secret")
                $(, global = $global)?)]
            file: Option<PathBuf>,
        }

        impl $name {
            /// The secret, from its variable or its file.
            pub fn read(self) -> anyhow::Result<Option<String>> {
                read($env, self.value, self.file.as_deref())
            }

            /// The secret given as a value, as tests set it.
            #[cfg(test)]
            #[allow(dead_code, reason = "not every secret is set by a test")]
            pub fn given(value: &str) -> Self {
                Self {
                    value: Some(value.to_owned()),
                    file: None,
                }
            }

            /// The secret given as a file, as tests set it.
            #[cfg(test)]
            #[allow(dead_code, reason = "not every secret is set by a test")]
            pub fn from_file(file: PathBuf) -> Self {
                Self {
                    value: None,
                    file: Some(file),
                }
            }
        }
    };
}

secret!(DatabaseUrl {
    long: "database-url",
    env: "HD_DATABASE_URL",
    value_name: "URL",
    help: "PostgreSQL connection string",
    global: true,
});
secret!(ImapUrl {
    long: "imap-url",
    env: "HD_IMAP_URL",
    value_name: "URL",
    help: "Mailbox to read, as imaps://user:password@host[:port]/MAILBOX (imap:// only for a server \
     on this machine). Without it no mail is read.",
});
secret!(SmtpUrl {
    long: "smtp-url",
    env: "HD_SMTP_URL",
    value_name: "URL",
    help: "Server that sends replies: smtps://user:password@host[:port] or \
     smtp://user:password@host[:port]?tls=required (plain smtp:// only for a server on this \
     machine). Without it replies wait in the outbox.",
});
secret!(AgentPassword {
    long: "password",
    env: "HD_AGENT_PASSWORD",
    value_name: "PASSWORD",
    help: "The password to set; prefer the variable or its file to the option, which other users of \
     the machine can see. Without it one is generated and printed once.",
});
secret!(OidcClientSecret {
    long: "oidc-client-secret",
    env: "HD_OIDC_CLIENT_SECRET",
    value_name: "SECRET",
    help: "The client's secret",
});

/// The secret `variable` gives, either as its `value` or read from its `_FILE` variant's `file`.
/// Either is trimmed of surrounding whitespace. A blank value counts as not given, as an unset
/// variable does; a blank file, a missing one or giving both is an error naming the variables.
pub fn read(
    variable: &str,
    value: Option<String>,
    file: Option<&Path>,
) -> anyhow::Result<Option<String>> {
    let value = value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    match (value, file) {
        (Some(_), Some(_)) => anyhow::bail!("set {variable} or {variable}_FILE, not both"),
        (Some(value), None) => Ok(Some(value)),
        (None, Some(file)) => {
            let content = std::fs::read_to_string(file)
                .with_context(|| format!("reading {variable}_FILE {}", file.display()))?;
            let secret = content.trim();
            if secret.is_empty() {
                anyhow::bail!("{variable}_FILE {} is empty", file.display());
            }
            Ok(Some(secret.to_owned()))
        }
        (None, None) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("hd-secret-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_secret_comes_from_its_variable_or_from_its_file_trimmed() {
        let dir = dir();
        let file = dir.join("imap-url");
        std::fs::write(&file, "imaps://helpdesk:p%40ss@mail.example.com/INBOX\n").unwrap();

        assert_eq!(
            read("HD_IMAP_URL", Some("given".to_owned()), None).unwrap(),
            Some("given".to_owned())
        );
        assert_eq!(
            read("HD_IMAP_URL", None, Some(&file)).unwrap(),
            Some("imaps://helpdesk:p%40ss@mail.example.com/INBOX".to_owned())
        );
        assert_eq!(read("HD_IMAP_URL", None, None).unwrap(), None);
        assert_eq!(
            read("HD_IMAP_URL", Some(String::new()), None).unwrap(),
            None
        );
        assert_eq!(
            read("HD_IMAP_URL", Some(" \n".to_owned()), None).unwrap(),
            None
        );
        assert_eq!(
            read("HD_IMAP_URL", Some(" given\n".to_owned()), None).unwrap(),
            Some("given".to_owned())
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn giving_a_secret_and_its_file_is_refused_naming_both() {
        let dir = dir();
        let file = dir.join("smtp-url");
        std::fs::write(&file, "smtps://helpdesk:secret@mail.example.com").unwrap();

        let error = read("HD_SMTP_URL", Some("given".to_owned()), Some(&file))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("HD_SMTP_URL ") && error.contains("HD_SMTP_URL_FILE"),
            "{error}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_missing_or_empty_file_is_refused_naming_the_variable_and_the_path() {
        let dir = dir();
        let missing = dir.join("missing");
        let empty = dir.join("empty");
        std::fs::write(&empty, " \n").unwrap();

        for file in [&missing, &empty] {
            let error = format!(
                "{:#}",
                read("HD_DATABASE_URL", None, Some(file)).unwrap_err()
            );
            assert!(
                error.contains("HD_DATABASE_URL_FILE")
                    && error.contains(&file.display().to_string()),
                "{error}"
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
