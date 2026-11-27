//! Settings: a configuration file in TOML, named by `HD_ZABBIX_CONFIG` (`zabbix.toml` in the
//! working directory by default), and three secrets, each given in its variable or read from the
//! file its `_FILE` variant names, as the Assistant reads its own (also from a `.env` file).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail};
use serde::Deserialize;

use crate::kinds::{KindMapping, Kinds};
use crate::rules::Rules;

/// The add-on's settings.
#[derive(Clone)]
pub struct Config {
    /// The core's base URL, such as `https://helpdesk.example.com`.
    pub core_url: String,
    /// The add-on's token, printed by `hd addon add`.
    pub core_token: String,
    /// The base URL of Zabbix's frontend, such as `https://zabbix.example.com`; the API is
    /// `<url>/api_jsonrpc.php`.
    pub zabbix_url: String,
    /// An API token of a Zabbix user that reads the host groups and their hosts.
    pub zabbix_token: String,
    /// How long a call to Zabbix may take.
    pub zabbix_timeout: Duration,
    /// The system's name as customers' external references name it, such as `Zabbix`.
    pub system: String,
    /// Time between two reads of Zabbix's hosts.
    pub interval: Duration,
    /// The asset kind of each host.
    pub kinds: Kinds,
    /// What each alert does.
    pub rules: Rules,
    /// The base URL of Zabbix's frontend as agents' browsers reach it, for the links of the live
    /// details; `zabbix_url` unless the configuration gives another.
    pub frontend_url: String,
    /// Where the add-on answers the core's reads of live details.
    pub listen: SocketAddr,
    /// The event secret, printed by `hd addon add`, with which the core signs its reads; needed
    /// to answer them, not by `check`.
    pub event_secret: Option<String>,
    /// The secret Zabbix's webhook media type sends with each event, read from the file
    /// `webhook.secret_file` names; none without a `[webhook]` section, when the add-on receives
    /// no alerts.
    pub webhook_secret: Option<String>,
}

/// Everything but the two tokens and the event secret.
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("core_url", &self.core_url)
            .field("zabbix_url", &self.zabbix_url)
            .field("zabbix_timeout", &self.zabbix_timeout)
            .field("system", &self.system)
            .field("interval", &self.interval)
            .field("kinds", &self.kinds)
            .field("rules", &self.rules)
            .field("frontend_url", &self.frontend_url)
            .field("listen", &self.listen)
            .finish_non_exhaustive()
    }
}

/// The configuration file as written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    core: CoreSection,
    zabbix: ZabbixSection,
    #[serde(default)]
    sync: SyncSection,
    assets: AssetsSection,
    #[serde(default)]
    alerts: AlertsSection,
    #[serde(default)]
    addon: AddonSection,
    #[serde(default)]
    webhook: Option<WebhookSection>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WebhookSection {
    secret_file: String,
}

/// The fewest characters of the webhook's secret.
const WEBHOOK_SECRET_MIN: usize = 16;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CoreSection {
    url: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ZabbixSection {
    url: String,
    #[serde(default = "default_system")]
    system: String,
    #[serde(default = "default_timeout_seconds")]
    timeout_seconds: u64,
    #[serde(default)]
    frontend_url: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SyncSection {
    #[serde(default = "default_interval_minutes")]
    interval_minutes: u64,
}

impl Default for SyncSection {
    fn default() -> Self {
        SyncSection {
            interval_minutes: default_interval_minutes(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AssetsSection {
    default_kind: String,
    #[serde(default)]
    kinds: Vec<KindMapping>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct AlertsSection {
    /// The rules as written, each read with its position, so that a sentence names it.
    #[serde(default)]
    rules: Vec<toml::Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddonSection {
    #[serde(default = "default_listen")]
    listen: String,
}

impl Default for AddonSection {
    fn default() -> Self {
        AddonSection {
            listen: default_listen(),
        }
    }
}

fn default_listen() -> String {
    "127.0.0.1:8091".into()
}

fn default_system() -> String {
    "Zabbix".into()
}

fn default_timeout_seconds() -> u64 {
    30
}

fn default_interval_minutes() -> u64 {
    5
}

/// The add-on's secrets. Each can be given in its variable or read from the file its `_FILE`
/// variant names, such as a mounted secret.
const SECRETS: [&str; 3] = [
    "HD_ZABBIX_TOKEN",
    "HD_ZABBIX_API_TOKEN",
    "HD_ZABBIX_EVENT_SECRET",
];

impl Config {
    /// Reads the configuration file `text` (read from `path`, which errors name) and the secrets
    /// through `var`, which returns a variable's value when it is set.
    pub fn parse(
        path: &Path,
        text: &str,
        var: impl Fn(&str) -> Option<String>,
    ) -> anyhow::Result<Config> {
        let file: File = toml::from_str(text)
            .with_context(|| format!("the configuration file {} is not valid", path.display()))?;
        let get = |name: &str| {
            var(name)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let secret = |name: &str| {
            debug_assert!(SECRETS.contains(&name), "{name} is not listed in SECRETS");
            let file = get(&format!("{name}_FILE")).map(PathBuf::from);
            read_secret(name, get(name), file.as_deref())?
                .with_context(|| format!("set {name} or {name}_FILE"))
        };

        let system = file.zabbix.system.trim().to_owned();
        if system.is_empty() {
            bail!("zabbix.system is blank: give the system's name, such as Zabbix");
        }
        if file.zabbix.timeout_seconds == 0 {
            bail!("zabbix.timeout_seconds is 0: give 1 or more");
        }
        if file.sync.interval_minutes == 0 {
            bail!("sync.interval_minutes is 0: give 1 or more");
        }
        let rules = Rules::read(file.alerts.rules)
            .map_err(anyhow::Error::msg)
            .with_context(|| format!("the configuration file {} is not valid", path.display()))?;
        let default = file.assets.default_kind.trim().to_owned();
        if default.is_empty() {
            bail!("assets.default_kind is blank: give an asset kind of the help desk's list");
        }
        let listen = file.addon.listen.trim();
        let listen: SocketAddr = listen.parse().map_err(|_| {
            anyhow::anyhow!("addon.listen is {listen:?}, not an address such as 127.0.0.1:8091")
        })?;
        let zabbix_url = checked_url("zabbix.url", &file.zabbix.url)?;
        let frontend_url = match &file.zabbix.frontend_url {
            Some(url) => checked_url("zabbix.frontend_url", url)?,
            None => zabbix_url.clone(),
        };
        let webhook_secret = match &file.webhook {
            Some(webhook) => Some(webhook_secret(&webhook.secret_file)?),
            None => None,
        };
        Ok(Config {
            core_url: checked_url("core.url", &file.core.url)?,
            core_token: secret("HD_ZABBIX_TOKEN")?,
            zabbix_url,
            zabbix_token: secret("HD_ZABBIX_API_TOKEN")?,
            zabbix_timeout: Duration::from_secs(file.zabbix.timeout_seconds),
            system,
            interval: Duration::from_secs(file.sync.interval_minutes * 60),
            kinds: Kinds {
                mappings: file.assets.kinds,
                default,
            },
            rules,
            frontend_url,
            listen,
            event_secret: read_secret(
                "HD_ZABBIX_EVENT_SECRET",
                get("HD_ZABBIX_EVENT_SECRET"),
                get("HD_ZABBIX_EVENT_SECRET_FILE")
                    .map(PathBuf::from)
                    .as_deref(),
            )?,
            webhook_secret,
        })
    }

    /// Reads `.env` (in this directory or a parent), then the configuration file that
    /// `HD_ZABBIX_CONFIG` names and the environment.
    pub fn from_env() -> anyhow::Result<Config> {
        let _ = dotenvy::dotenv();
        let path = PathBuf::from(
            std::env::var("HD_ZABBIX_CONFIG")
                .ok()
                .filter(|p| !p.trim().is_empty())
                .unwrap_or_else(|| "zabbix.toml".into()),
        );
        let text = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "reading the configuration file {} (HD_ZABBIX_CONFIG names it)",
                path.display()
            )
        })?;
        Config::parse(&path, &text, |name| std::env::var(name).ok())
    }
}

/// The secret `variable` gives, either as its `value` or read from its `_FILE` variant's `file`,
/// trimmed of surrounding whitespace; `value` is already trimmed, and `None` when blank. A blank
/// file, a missing one or giving both is an error naming the variables.
fn read_secret(
    variable: &str,
    value: Option<String>,
    file: Option<&Path>,
) -> anyhow::Result<Option<String>> {
    match (value, file) {
        (Some(_), Some(_)) => bail!("set {variable} or {variable}_FILE, not both"),
        (Some(value), None) => Ok(Some(value)),
        (None, Some(file)) => {
            let content = std::fs::read_to_string(file)
                .with_context(|| format!("reading {variable}_FILE {}", file.display()))?;
            let secret = content.trim();
            if secret.is_empty() {
                bail!("{variable}_FILE {} is empty", file.display());
            }
            Ok(Some(secret.to_owned()))
        }
        (None, None) => Ok(None),
    }
}

/// The webhook's secret, read from the file `webhook.secret_file` names, trimmed of surrounding
/// whitespace.
fn webhook_secret(file: &str) -> anyhow::Result<String> {
    let file = file.trim();
    if file.is_empty() {
        bail!("webhook.secret_file is blank: name the file that holds the webhook's secret");
    }
    let content = std::fs::read_to_string(file)
        .with_context(|| format!("reading webhook.secret_file {file}"))?;
    let secret = content.trim();
    if secret.is_empty() {
        bail!("webhook.secret_file {file} is empty");
    }
    let characters = secret.chars().count();
    if characters < WEBHOOK_SECRET_MIN {
        bail!(
            "webhook.secret_file {file} holds {characters} characters: give a secret of \
             {WEBHOOK_SECRET_MIN} or more"
        );
    }
    Ok(secret.to_owned())
}

/// Checks an http(s) URL and drops a trailing `/`.
fn checked_url(setting: &str, raw: &str) -> anyhow::Result<String> {
    let raw = raw.trim();
    let parsed =
        url::Url::parse(raw).with_context(|| format!("{setting} is {raw:?}, not a URL"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        bail!("{setting} is {raw:?}: it must use http or https");
    }
    Ok(raw.trim_end_matches('/').to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const FULL: &str = r#"
[core]
url = "https://helpdesk.example.com/"

[zabbix]
url = "https://zabbix.example.com/"
system = "Monitoring"
timeout_seconds = 10
frontend_url = "https://monitoring.example.com/zabbix/"

[sync]
interval_minutes = 2

[assets]
default_kind = "Server"

[addon]
listen = "0.0.0.0:8091"

[[assets.kinds]]
template = "Linux by Zabbix agent"
kind = "Server"

[[assets.kinds]]
tag = "device"
value = "printer"
kind = "Printer"
"#;

    const SMALLEST: &str = r#"
[core]
url = "http://127.0.0.1:8080"

[zabbix]
url = "http://127.0.0.1:8180"

[assets]
default_kind = "Server"
"#;

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let vars: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| vars.get(name).cloned()
    }

    const TOKENS: [(&str, &str); 3] = [
        ("HD_ZABBIX_TOKEN", "core-token"),
        ("HD_ZABBIX_API_TOKEN", "zabbix-token"),
        ("HD_ZABBIX_EVENT_SECRET", "event-secret"),
    ];

    fn parse(text: &str) -> anyhow::Result<Config> {
        Config::parse(Path::new("zabbix.toml"), text, vars(&TOKENS))
    }

    fn error(text: &str) -> String {
        format!("{:#}", parse(text).unwrap_err())
    }

    #[test]
    fn the_file_gives_the_settings_and_the_variables_the_secrets() {
        let config = parse(FULL).unwrap();
        assert_eq!(config.core_url, "https://helpdesk.example.com");
        assert_eq!(config.core_token, "core-token");
        assert_eq!(config.zabbix_url, "https://zabbix.example.com");
        assert_eq!(config.zabbix_token, "zabbix-token");
        assert_eq!(config.zabbix_timeout, Duration::from_secs(10));
        assert_eq!(config.system, "Monitoring");
        assert_eq!(config.interval, Duration::from_secs(120));
        assert_eq!(config.kinds.default, "Server");
        assert_eq!(config.frontend_url, "https://monitoring.example.com/zabbix");
        assert_eq!(config.listen.to_string(), "0.0.0.0:8091");
        assert_eq!(config.event_secret.as_deref(), Some("event-secret"));
        assert_eq!(
            config
                .kinds
                .mappings
                .iter()
                .map(|m| m.kind.as_str())
                .collect::<Vec<_>>(),
            ["Server", "Printer"]
        );
    }

    #[test]
    fn the_example_in_the_readme_s_folder_is_a_valid_configuration() {
        let config = parse(include_str!("../zabbix.example.toml")).unwrap();
        assert_eq!(config.kinds.mappings.len(), 2);
        assert_eq!(config.rules.rules().len(), 4);
    }

    #[test]
    fn the_settings_as_logged_leave_the_tokens_out() {
        let shown = format!("{:?}", parse(FULL).unwrap());
        assert!(shown.contains("https://zabbix.example.com"), "{shown}");
        assert!(
            !shown.contains("core-token")
                && !shown.contains("zabbix-token")
                && !shown.contains("event-secret"),
            "{shown}"
        );
    }

    #[test]
    fn what_is_left_out_has_a_default() {
        let config = parse(SMALLEST).unwrap();
        assert_eq!(config.system, "Zabbix");
        assert_eq!(config.zabbix_timeout, Duration::from_secs(30));
        assert_eq!(config.interval, Duration::from_secs(300));
        assert!(config.kinds.mappings.is_empty());
        assert_eq!(config.frontend_url, "http://127.0.0.1:8180");
        assert_eq!(config.listen.to_string(), "127.0.0.1:8091");
    }

    #[test]
    fn a_file_that_does_not_fit_is_refused_with_a_sentence_naming_it() {
        let unknown = SMALLEST.replace("[assets]", "[assets]\ndefault_knd = \"Server\"");
        assert!(
            error(&unknown).contains("default_knd"),
            "{}",
            error(&unknown)
        );
        assert!(error("[core]\n").contains("zabbix.toml"));
        let bad_mapping =
            format!("{SMALLEST}\n[[assets.kinds]]\nvalue = \"x\"\nkind = \"Server\"\n");
        let refused = error(&bad_mapping);
        assert!(
            refused.contains("zabbix.toml")
                && refused.contains("assets.kinds")
                && refused.contains("the mapping to Server names neither a template nor a tag"),
            "{refused}"
        );
        let ftp = SMALLEST.replace("http://127.0.0.1:8180", "ftp://zabbix.example.com");
        assert_eq!(
            error(&ftp),
            "zabbix.url is \"ftp://zabbix.example.com\": it must use http or https"
        );
        let blank = SMALLEST.replace("\"Server\"", "\" \"");
        assert!(error(&blank).starts_with("assets.default_kind is blank"));
        let never = format!("{SMALLEST}\n[sync]\ninterval_minutes = 0\n");
        assert_eq!(error(&never), "sync.interval_minutes is 0: give 1 or more");
    }

    #[test]
    fn each_secret_can_be_read_from_its_file_and_giving_both_is_refused() {
        let dir = std::env::temp_dir().join(format!("hd-zabbix-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("token");
        std::fs::write(&file, "  from-the-file\n").unwrap();
        let path = file.to_str().unwrap();
        for secret in SECRETS {
            let file_variable = format!("{secret}_FILE");
            let mut pairs: Vec<(&str, &str)> = TOKENS
                .iter()
                .copied()
                .filter(|(n, _)| *n != secret)
                .collect();
            pairs.push((&file_variable, path));
            let config = Config::parse(Path::new("z.toml"), SMALLEST, vars(&pairs)).unwrap();
            let read = match secret {
                "HD_ZABBIX_TOKEN" => config.core_token,
                "HD_ZABBIX_API_TOKEN" => config.zabbix_token,
                _ => config.event_secret.unwrap(),
            };
            assert_eq!(read, "from-the-file");

            pairs.push((secret, "too"));
            let both = Config::parse(Path::new("z.toml"), SMALLEST, vars(&pairs)).unwrap_err();
            assert_eq!(
                format!("{both:#}"),
                format!("set {secret} or {secret}_FILE, not both")
            );

            let missing: Vec<(&str, &str)> = TOKENS
                .iter()
                .copied()
                .filter(|(n, _)| *n != secret)
                .collect();
            let none = Config::parse(Path::new("z.toml"), SMALLEST, vars(&missing));
            if secret == "HD_ZABBIX_EVENT_SECRET" {
                // Only answering the core's reads needs it, which `check` does not.
                assert_eq!(none.unwrap().event_secret, None);
            } else {
                assert_eq!(
                    format!("{:#}", none.unwrap_err()),
                    format!("set {secret} or {secret}_FILE")
                );
            }
        }
        std::fs::write(&file, "\n").unwrap();
        let empty = Config::parse(
            Path::new("z.toml"),
            SMALLEST,
            vars(&[
                ("HD_ZABBIX_TOKEN", "t"),
                ("HD_ZABBIX_EVENT_SECRET", "s"),
                ("HD_ZABBIX_API_TOKEN_FILE", path),
            ]),
        )
        .unwrap_err();
        assert!(format!("{empty:#}").contains("is empty"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_webhook_s_secret_is_read_from_the_file_the_configuration_names() {
        assert_eq!(parse(SMALLEST).unwrap().webhook_secret, None, "no webhook");
        let dir = std::env::temp_dir().join(format!("hd-zabbix-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("webhook-secret");
        let with_webhook = format!(
            "{SMALLEST}\n[webhook]\nsecret_file = {:?}\n",
            file.to_str().unwrap()
        );
        std::fs::write(&file, "  0123456789abcdef-webhook\n").unwrap();
        assert_eq!(
            parse(&with_webhook).unwrap().webhook_secret.as_deref(),
            Some("0123456789abcdef-webhook")
        );
        assert!(
            !format!("{:?}", parse(&with_webhook).unwrap()).contains("0123456789abcdef"),
            "the secret is not logged"
        );

        std::fs::write(&file, "\n").unwrap();
        let empty = error(&with_webhook);
        assert!(
            empty.starts_with("webhook.secret_file ") && empty.ends_with(" is empty"),
            "{empty}"
        );
        std::fs::write(&file, "short").unwrap();
        let short = error(&with_webhook);
        assert!(
            short.ends_with(" holds 5 characters: give a secret of 16 or more"),
            "{short}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
        let missing = error(&with_webhook);
        assert!(
            missing.starts_with("reading webhook.secret_file "),
            "{missing}"
        );
        let blank = format!("{SMALLEST}\n[webhook]\nsecret_file = \" \"\n");
        assert_eq!(
            error(&blank),
            "webhook.secret_file is blank: name the file that holds the webhook's secret"
        );
    }

    #[test]
    fn the_alert_rules_are_read_in_order_and_an_invalid_one_stops_the_add_on() {
        assert!(parse(SMALLEST).unwrap().rules.is_default());
        let with_alerts = format!("{SMALLEST}\n[alerts]\n");
        assert!(parse(&with_alerts).unwrap().rules.is_default());

        let rules = format!(
            "{SMALLEST}\n[[alerts.rules]]\nname = \"Backups\"\ntrigger_contains = \"backup\"\n\
             outcome = \"ignore\"\n\n[[alerts.rules]]\noutcome = \"open\"\npriority = \"low\"\n"
        );
        let config = parse(&rules).unwrap();
        assert!(!config.rules.is_default());
        assert_eq!(config.rules.rules()[0].name.as_deref(), Some("Backups"));
        assert_eq!(config.rules.rules().len(), 2);

        let invalid = rules.replace("\"low\"", "\"soon\"");
        assert_eq!(
            error(&invalid),
            "the configuration file zabbix.toml is not valid: alerts.rules: rule 2: priority is \
             \"soon\": give low, normal, high or urgent"
        );
    }
}
