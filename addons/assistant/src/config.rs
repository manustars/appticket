//! Settings, read from `HD_ASSISTANT_*` environment variables (and a `.env` file).

use std::net::{IpAddr, SocketAddr};

use anyhow::{Context, bail};

/// How the Assistant reaches its language model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    /// The Claude Messages API (`POST <url>/v1/messages`).
    Anthropic,
    /// Any server exposing `POST <url>/v1/chat/completions`, such as Ollama, llama.cpp or vLLM.
    OpenAiCompatible,
}

/// The default model on the Claude API.
pub const DEFAULT_ANTHROPIC_MODEL: &str = "claude-opus-5-5";
pub const DEFAULT_ANTHROPIC_URL: &str = "https://api.anthropic.com";
/// Effort on the Claude API: how much the model thinks before answering.
pub const DEFAULT_EFFORT: &str = "medium";

#[derive(Debug, Clone)]
pub struct ModelSettings {
    pub provider: Provider,
    /// Base URL, without `/v1`.
    pub url: String,
    pub model: String,
    pub api_key: Option<String>,
    /// Claude API only; `None` leaves it to the model's default.
    pub effort: Option<String>,
}

impl ModelSettings {
    /// Whether customers' mail leaves the help desk's own network when the model reads it: true
    /// for the Claude API and for any server that is not on this machine or a private network.
    pub fn is_external(&self) -> bool {
        if self.provider == Provider::Anthropic {
            return true;
        }
        let Ok(url) = url::Url::parse(&self.url) else {
            return true;
        };
        match url.host() {
            Some(url::Host::Domain(name)) => {
                let name = name.to_ascii_lowercase();
                !(name == "localhost" || name.ends_with(".localhost"))
            }
            Some(url::Host::Ipv4(ip)) => {
                !(ip.is_loopback() || ip.is_private() || ip.is_link_local())
            }
            Some(url::Host::Ipv6(ip)) => {
                let ip = IpAddr::V6(ip);
                !(ip.is_loopback() || matches!(ip, IpAddr::V6(v6) if v6.is_unique_local()))
            }
            None => true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub listen: SocketAddr,
    /// The core's base URL, such as `https://helpdesk.example.com`.
    pub core_url: String,
    /// The add-on's token, printed by `hd addon add`.
    pub token: String,
    /// The event secret, printed by `hd addon add`.
    pub event_secret: String,
    pub model: ModelSettings,
}

impl Config {
    /// Reads the settings through `var`, which returns a variable's value when it is set.
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> anyhow::Result<Config> {
        let get = |name: &str| {
            var(name)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let required = |name: &str| get(name).with_context(|| format!("set {name}"));

        let listen = get("HD_ASSISTANT_LISTEN")
            .unwrap_or_else(|| "127.0.0.1:8090".into())
            .parse()
            .context("HD_ASSISTANT_LISTEN is not an address such as 127.0.0.1:8090")?;
        let provider = match get("HD_ASSISTANT_MODEL_PROVIDER").as_deref() {
            None | Some("anthropic") => Provider::Anthropic,
            Some("openai-compatible") => Provider::OpenAiCompatible,
            Some(other) => bail!(
                "HD_ASSISTANT_MODEL_PROVIDER is {other:?}: use anthropic or openai-compatible"
            ),
        };
        let model = match provider {
            Provider::Anthropic => ModelSettings {
                provider,
                url: get("HD_ASSISTANT_MODEL_URL").unwrap_or_else(|| DEFAULT_ANTHROPIC_URL.into()),
                model: get("HD_ASSISTANT_MODEL").unwrap_or_else(|| DEFAULT_ANTHROPIC_MODEL.into()),
                api_key: Some(required("HD_ASSISTANT_API_KEY")?),
                effort: match var("HD_ASSISTANT_MODEL_EFFORT") {
                    // Set but empty: send no effort, for models that do not take one.
                    Some(v) if v.trim().is_empty() => None,
                    Some(v) => Some(v.trim().to_owned()),
                    None => Some(DEFAULT_EFFORT.into()),
                },
            },
            Provider::OpenAiCompatible => ModelSettings {
                provider,
                url: required("HD_ASSISTANT_MODEL_URL")?,
                model: required("HD_ASSISTANT_MODEL")?,
                api_key: get("HD_ASSISTANT_API_KEY"),
                effort: None,
            },
        };
        let mut model = model;
        model.url = base_url(&model.url)?;
        Ok(Config {
            listen,
            core_url: base_url(&required("HD_ASSISTANT_CORE_URL")?)?,
            token: required("HD_ASSISTANT_TOKEN")?,
            event_secret: required("HD_ASSISTANT_EVENT_SECRET")?,
            model,
        })
    }

    /// Reads `.env` (in this directory or a parent) and then the environment.
    pub fn from_env() -> anyhow::Result<Config> {
        let _ = dotenvy::dotenv();
        Config::from_vars(|name| std::env::var(name).ok())
    }
}

/// Checks an http(s) URL and drops a trailing `/` and `/v1`.
fn base_url(raw: &str) -> anyhow::Result<String> {
    let parsed = url::Url::parse(raw).with_context(|| format!("{raw:?} is not a URL"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        bail!("{raw:?} must use http or https");
    }
    let trimmed = raw.trim_end_matches('/');
    Ok(trimmed.strip_suffix("/v1").unwrap_or(trimmed).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config(pairs: &[(&str, &str)]) -> anyhow::Result<Config> {
        let vars: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::from_vars(|name| vars.get(name).cloned())
    }

    const CORE: [(&str, &str); 3] = [
        ("HD_ASSISTANT_CORE_URL", "http://127.0.0.1:8080/"),
        ("HD_ASSISTANT_TOKEN", "t"),
        ("HD_ASSISTANT_EVENT_SECRET", "s"),
    ];

    #[test]
    fn the_claude_api_is_the_default_and_needs_a_key() {
        assert!(config(&CORE).is_err(), "no API key");
        let mut vars = CORE.to_vec();
        vars.push(("HD_ASSISTANT_API_KEY", "k"));
        let c = config(&vars).unwrap();
        assert_eq!(c.listen.to_string(), "127.0.0.1:8090");
        assert_eq!(c.core_url, "http://127.0.0.1:8080");
        assert_eq!(c.model.provider, Provider::Anthropic);
        assert_eq!(c.model.model, DEFAULT_ANTHROPIC_MODEL);
        assert_eq!(c.model.effort.as_deref(), Some(DEFAULT_EFFORT));
        assert!(c.model.is_external());
    }

    #[test]
    fn a_local_server_is_not_external() {
        let mut vars = CORE.to_vec();
        vars.extend([
            ("HD_ASSISTANT_MODEL_PROVIDER", "openai-compatible"),
            ("HD_ASSISTANT_MODEL_URL", "http://127.0.0.1:11434/v1/"),
            ("HD_ASSISTANT_MODEL", "a-local-model"),
        ]);
        let c = config(&vars).unwrap();
        assert_eq!(c.model.url, "http://127.0.0.1:11434");
        assert!(!c.model.is_external());
        assert_eq!(c.model.api_key, None);

        vars.push(("HD_ASSISTANT_MODEL_URL", "https://models.example.com"));
        let remote = config(&vars).unwrap();
        assert!(remote.model.is_external());
    }

    #[test]
    fn unknown_providers_are_refused() {
        let mut vars = CORE.to_vec();
        vars.push(("HD_ASSISTANT_MODEL_PROVIDER", "carrier-pigeon"));
        assert!(config(&vars).is_err());
    }
}
