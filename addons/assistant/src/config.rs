//! Settings, read from `HD_ASSISTANT_*` environment variables (and a `.env` file); each secret
//! can be read from a file instead.
//!
//! There is no default model service: whoever installs the Assistant chooses the provider and
//! the model, since sending customers' mail to a service is their decision (ADR 0006).

use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

/// A model service the Assistant knows how to call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    /// The Claude API (Messages API).
    Anthropic,
    /// The OpenAI API.
    OpenAi,
    /// Mistral AI's API.
    Mistral,
    /// Google's Gemini API, through its OpenAI-compatible endpoint.
    Gemini,
    /// A deployment on Azure OpenAI.
    AzureOpenAi,
    /// Ollama, usually on this machine.
    Ollama,
    /// Any other server speaking the OpenAI chat completions protocol (vLLM, llama.cpp…).
    OpenAiCompatible,
}

/// How a provider's model is called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// `POST <url>/v1/messages` (Claude).
    Messages,
    /// OpenAI-style chat completions.
    ChatCompletions,
}

/// How the API key travels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Auth {
    /// `x-api-key: <key>` (Claude).
    XApiKey,
    /// `Authorization: Bearer <key>`, when a key is set.
    Bearer,
    /// `api-key: <key>` (Azure OpenAI).
    ApiKeyHeader,
}

/// How the answer is held to the JSON the Assistant asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonOutput {
    /// The service constrains the answer to the JSON Schema.
    Schema,
    /// The service only promises a JSON object; the schema goes in the instructions.
    Object,
}

impl Provider {
    pub const ALL: [Provider; 7] = [
        Provider::Anthropic,
        Provider::OpenAi,
        Provider::Mistral,
        Provider::Gemini,
        Provider::AzureOpenAi,
        Provider::Ollama,
        Provider::OpenAiCompatible,
    ];

    /// The value of `HD_ASSISTANT_MODEL_PROVIDER`.
    pub fn name(self) -> &'static str {
        match self {
            Provider::Anthropic => "anthropic",
            Provider::OpenAi => "openai",
            Provider::Mistral => "mistral",
            Provider::Gemini => "gemini",
            Provider::AzureOpenAi => "azure-openai",
            Provider::Ollama => "ollama",
            Provider::OpenAiCompatible => "openai-compatible",
        }
    }

    pub fn parse(name: &str) -> Option<Provider> {
        Provider::ALL.into_iter().find(|p| p.name() == name)
    }

    /// The base URL used when `HD_ASSISTANT_MODEL_URL` is not set; `None` when it must be.
    pub fn default_url(self) -> Option<&'static str> {
        match self {
            Provider::Anthropic => Some("https://api.anthropic.com"),
            Provider::OpenAi => Some("https://api.openai.com/v1"),
            Provider::Mistral => Some("https://api.mistral.ai/v1"),
            Provider::Gemini => Some("https://generativelanguage.googleapis.com/v1beta/openai"),
            Provider::Ollama => Some("http://127.0.0.1:11434"),
            Provider::AzureOpenAi | Provider::OpenAiCompatible => None,
        }
    }

    pub fn protocol(self) -> Protocol {
        match self {
            Provider::Anthropic => Protocol::Messages,
            _ => Protocol::ChatCompletions,
        }
    }

    pub fn auth(self) -> Auth {
        match self {
            Provider::Anthropic => Auth::XApiKey,
            Provider::AzureOpenAi => Auth::ApiKeyHeader,
            _ => Auth::Bearer,
        }
    }

    /// Whether `HD_ASSISTANT_API_KEY` is required.
    /// How many model calls run at once unless `HD_ASSISTANT_MODEL_CONCURRENCY` says otherwise:
    /// one for self-hosted servers, which are often a single GPU, four for hosted services.
    pub fn default_concurrency(self) -> usize {
        match self {
            Provider::Ollama | Provider::OpenAiCompatible => 1,
            _ => 4,
        }
    }

    pub fn needs_key(self) -> bool {
        !matches!(self, Provider::Ollama | Provider::OpenAiCompatible)
    }

    pub fn json_output(self) -> JsonOutput {
        match self {
            // Documented with `response_format: {type: "json_schema", ...}` on chat completions.
            Provider::OpenAi | Provider::Mistral | Provider::AzureOpenAi => JsonOutput::Schema,
            // Gemini's compatibility layer shows structured output only through SDK helpers and
            // ignores parameters it does not know; Ollama lists `response_format` without
            // documenting the `json_schema` form; an unknown server may support neither.
            Provider::Gemini | Provider::Ollama | Provider::OpenAiCompatible => JsonOutput::Object,
            // Claude has its own structured outputs (`output_config.format`).
            Provider::Anthropic => JsonOutput::Schema,
        }
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The names `HD_ASSISTANT_MODEL_PROVIDER` takes, for error messages.
fn provider_names() -> String {
    Provider::ALL.map(Provider::name).join(", ")
}

/// Effort on the Claude API, how much the model thinks before answering, unless
/// `HD_ASSISTANT_MODEL_EFFORT` says otherwise.
pub const DEFAULT_EFFORT: &str = "medium";
#[derive(Debug, Clone)]
pub struct ModelSettings {
    pub provider: Provider,
    /// The base URL. For `anthropic` and `ollama`, the server without `/v1`; for Azure, the
    /// resource (`https://<resource>.openai.azure.com`); otherwise the base the provider's
    /// OpenAI client libraries take, ending in `/v1` or similar.
    pub url: String,
    /// The model's id; on Azure, the deployment's name.
    pub model: String,
    pub api_key: Option<String>,
    /// Claude API only; `None` leaves it to the model's default.
    pub effort: Option<String>,
    /// Azure OpenAI only: a dated `api-version` for the deployments path; `None` or `v1` for
    /// the v1 API.
    pub azure_api_version: Option<String>,
    /// At most this many model calls at the same time.
    pub concurrency: usize,
    /// `openai-compatible` only: whether the model thinks before answering, sent as
    /// `chat_template_kwargs.enable_thinking` (vLLM, SGLang); `None` sends nothing, which leaves
    /// it to the server.
    pub thinking: Option<bool>,
}

impl ModelSettings {
    /// Whether customers' mail leaves this machine when the model reads it: false only for a
    /// self-hosted model service (Ollama or an OpenAI-compatible server) on a loopback address.
    /// A hosted provider is external even behind a local proxy, which forwards the mail.
    pub fn is_external(&self) -> bool {
        match self.provider {
            Provider::Ollama | Provider::OpenAiCompatible => !is_loopback(&self.url),
            _ => true,
        }
    }

    /// Where a request goes.
    pub fn endpoint(&self) -> String {
        let url = &self.url;
        match self.provider {
            Provider::Anthropic => format!("{url}/v1/messages"),
            Provider::Ollama => format!("{url}/v1/chat/completions"),
            Provider::AzureOpenAi => match self.azure_api_version.as_deref().unwrap_or("v1") {
                // The v1 API takes the deployment as the body's `model`, with no version.
                "v1" => format!("{url}/openai/v1/chat/completions"),
                version => format!(
                    "{url}/openai/deployments/{}/chat/completions?api-version={version}",
                    self.model
                ),
            },
            _ => format!("{url}/chat/completions"),
        }
    }
}

fn is_loopback(raw: &str) -> bool {
    let Ok(url) = url::Url::parse(raw) else {
        return false;
    };
    match url.host() {
        Some(url::Host::Domain(name)) => {
            let name = name.to_ascii_lowercase();
            name == "localhost" || name.ends_with(".localhost")
        }
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => IpAddr::V6(ip).is_loopback(),
        None => false,
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
        let secret = |name: &str| {
            debug_assert!(SECRETS.contains(&name), "{name} is not listed in SECRETS");
            let file = get(&format!("{name}_FILE")).map(PathBuf::from);
            read_secret(name, get(name), file.as_deref())
        };
        let required_secret =
            |name: &str| secret(name)?.with_context(|| format!("set {name} or {name}_FILE"));

        let listen = get("HD_ASSISTANT_LISTEN")
            .unwrap_or_else(|| "127.0.0.1:8090".into())
            .parse()
            .context("HD_ASSISTANT_LISTEN is not an address such as 127.0.0.1:8090")?;
        let provider = match get("HD_ASSISTANT_MODEL_PROVIDER") {
            None => bail!(
                "set HD_ASSISTANT_MODEL_PROVIDER to the service that runs the model: one of {}",
                provider_names()
            ),
            Some(name) => Provider::parse(&name).with_context(|| {
                format!(
                    "HD_ASSISTANT_MODEL_PROVIDER is {name:?}: use one of {}",
                    provider_names()
                )
            })?,
        };
        let model = get("HD_ASSISTANT_MODEL").with_context(|| match provider {
            Provider::AzureOpenAi => {
                "set HD_ASSISTANT_MODEL to the name of the Azure deployment".into()
            }
            _ => format!("set HD_ASSISTANT_MODEL to the id of a model {provider} offers"),
        })?;
        let raw_url = match (get("HD_ASSISTANT_MODEL_URL"), provider.default_url()) {
            (Some(url), _) => url,
            (None, Some(default)) => default.to_owned(),
            (None, None) => bail!(match provider {
                Provider::AzureOpenAi =>
                    "set HD_ASSISTANT_MODEL_URL to the Azure OpenAI resource, such as \
                     https://<resource>.openai.azure.com",
                _ =>
                    "set HD_ASSISTANT_MODEL_URL to the server's base URL, such as \
                      http://127.0.0.1:8000/v1",
            }),
        };
        // Read even when the provider takes no key, so that giving both is refused there too.
        let api_key = secret("HD_ASSISTANT_API_KEY")?;
        if api_key.is_none() && provider.needs_key() {
            bail!(
                "set HD_ASSISTANT_API_KEY or HD_ASSISTANT_API_KEY_FILE to the {provider} API key"
            );
        }
        // Azure: the v1 API unless a dated version is asked for (structured outputs need
        // 2024-08-01-preview or later).
        let azure_api_version = match provider {
            Provider::AzureOpenAi => get("HD_ASSISTANT_AZURE_API_VERSION"),
            _ => None,
        };
        let thinking = match get("HD_ASSISTANT_THINKING").as_deref() {
            None => None,
            Some(value) if provider != Provider::OpenAiCompatible => bail!(
                "HD_ASSISTANT_THINKING is {value:?}, but it applies only to openai-compatible"
            ),
            Some("on") => Some(true),
            Some("off") => Some(false),
            Some(other) => bail!("HD_ASSISTANT_THINKING is {other:?}: use on or off"),
        };
        let concurrency = match get("HD_ASSISTANT_MODEL_CONCURRENCY") {
            None => provider.default_concurrency(),
            Some(v) => v.parse().ok().filter(|n| *n >= 1).with_context(|| {
                format!("HD_ASSISTANT_MODEL_CONCURRENCY is {v:?}: give a whole number, 1 or more")
            })?,
        };
        let model = ModelSettings {
            provider,
            url: model_url(provider, &raw_url)?,
            model,
            api_key,
            effort: match (provider, var("HD_ASSISTANT_MODEL_EFFORT")) {
                (Provider::Anthropic, None) => Some(DEFAULT_EFFORT.into()),
                // Set but empty: send no effort, for models that do not take one.
                (Provider::Anthropic, Some(v)) => {
                    Some(v.trim().to_owned()).filter(|v| !v.is_empty())
                }
                _ => None,
            },
            azure_api_version,
            concurrency,
            thinking,
        };
        Ok(Config {
            listen,
            core_url: checked_url(
                &get("HD_ASSISTANT_CORE_URL").context("set HD_ASSISTANT_CORE_URL")?,
            )?,
            token: required_secret("HD_ASSISTANT_TOKEN")?,
            event_secret: required_secret("HD_ASSISTANT_EVENT_SECRET")?,
            model,
        })
    }

    /// Reads `.env` (in this directory or a parent) and then the environment.
    pub fn from_env() -> anyhow::Result<Config> {
        let _ = dotenvy::dotenv();
        Config::from_vars(|name| std::env::var(name).ok())
    }
}

/// The Assistant's secrets. Each can be given in its variable or read from the file its `_FILE`
/// variant names, such as a mounted secret (ADR 0009); every secret is read through
/// [`read_secret`], and a test checks that every variable read is either listed here or known
/// not to be a secret.
const SECRETS: [&str; 3] = [
    "HD_ASSISTANT_TOKEN",
    "HD_ASSISTANT_EVENT_SECRET",
    "HD_ASSISTANT_API_KEY",
];

/// The secret `variable` gives, either as its `value` or read from its `_FILE` variant's `file`,
/// trimmed of surrounding whitespace; `value` is already trimmed, and `None` when blank. A blank
/// file, a missing one or giving both is an error naming the variables, as in the core.
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

/// Checks an http(s) URL and drops a trailing `/`.
fn checked_url(raw: &str) -> anyhow::Result<String> {
    let parsed = url::Url::parse(raw).with_context(|| format!("{raw:?} is not a URL"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        bail!("{raw:?} must use http or https");
    }
    Ok(raw.trim_end_matches('/').to_owned())
}

/// The model URL in the form [`ModelSettings::endpoint`] expects.
fn model_url(provider: Provider, raw: &str) -> anyhow::Result<String> {
    let url = checked_url(raw)?;
    Ok(match provider {
        // These take the server's address; tolerate the `/v1` some guides add.
        Provider::Anthropic | Provider::Ollama => {
            url.strip_suffix("/v1").unwrap_or(&url).to_owned()
        }
        Provider::AzureOpenAi => url.strip_suffix("/openai").unwrap_or(&url).to_owned(),
        // A server's address alone means the usual `/v1`.
        Provider::OpenAiCompatible if url::Url::parse(&url)?.path() == "/" => {
            format!("{url}/v1")
        }
        _ => url,
    })
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

    fn with(extra: &[(&'static str, &'static str)]) -> Vec<(&'static str, &'static str)> {
        let mut vars = vec![
            ("HD_ASSISTANT_CORE_URL", "http://127.0.0.1:8080/"),
            ("HD_ASSISTANT_TOKEN", "t"),
            ("HD_ASSISTANT_EVENT_SECRET", "s"),
        ];
        vars.extend_from_slice(extra);
        vars
    }

    fn error(pairs: &[(&str, &str)]) -> String {
        format!("{:#}", config(pairs).unwrap_err())
    }

    /// A new temporary directory for secret files.
    fn dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("hd-assistant-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The token, event secret and key of a provider that needs one, each given in its variable
    /// unless `file` names it: that one is read from its `_FILE` variant.
    fn anthropic_with_secret_file(file: &str, path: &str) -> anyhow::Result<Config> {
        let mut vars = vec![
            ("HD_ASSISTANT_CORE_URL", "http://127.0.0.1:8080"),
            ("HD_ASSISTANT_MODEL_PROVIDER", "anthropic"),
            ("HD_ASSISTANT_MODEL", "claude-opus-5-5"),
        ];
        let file_variable = format!("{file}_FILE");
        for (secret, value) in [
            ("HD_ASSISTANT_TOKEN", "t"),
            ("HD_ASSISTANT_EVENT_SECRET", "s"),
            ("HD_ASSISTANT_API_KEY", "k"),
        ] {
            if secret == file {
                vars.push((&file_variable, path));
            } else {
                vars.push((secret, value));
            }
        }
        config(&vars)
    }

    #[test]
    fn each_secret_can_be_read_from_its_file_trimmed() {
        let dir = dir();
        let path = dir.join("secret");
        std::fs::write(&path, "  invented-secret-from-file\n").unwrap();
        let path = path.display().to_string();

        let c = anthropic_with_secret_file("HD_ASSISTANT_TOKEN", &path).unwrap();
        assert_eq!(c.token, "invented-secret-from-file");
        let c = anthropic_with_secret_file("HD_ASSISTANT_EVENT_SECRET", &path).unwrap();
        assert_eq!(c.event_secret, "invented-secret-from-file");
        // anthropic needs a key: the one from the file is enough.
        let c = anthropic_with_secret_file("HD_ASSISTANT_API_KEY", &path).unwrap();
        assert_eq!(
            c.model.api_key.as_deref(),
            Some("invented-secret-from-file")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_missing_unreadable_or_empty_file_is_refused_naming_the_variable_and_the_path() {
        let dir = dir();
        let missing = dir.join("missing");
        let empty = dir.join("empty");
        std::fs::write(&empty, " \n").unwrap();
        // A directory cannot be read as a file.
        let unreadable = dir.join("a-directory");
        std::fs::create_dir(&unreadable).unwrap();

        for file in [&missing, &empty, &unreadable] {
            let path = file.display().to_string();
            let error = format!(
                "{:#}",
                anthropic_with_secret_file("HD_ASSISTANT_TOKEN", &path).unwrap_err()
            );
            assert!(
                error.contains("HD_ASSISTANT_TOKEN_FILE") && error.contains(&path),
                "{error}"
            );
            // Refused even where the key is optional: a file was named.
            let error = error_with_file("HD_ASSISTANT_API_KEY_FILE", &path);
            assert!(
                error.contains("HD_ASSISTANT_API_KEY_FILE") && error.contains(&path),
                "{error}"
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The error of an `ollama` configuration, which needs no key, with `variable` set to `value`.
    fn error_with_file(variable: &str, value: &str) -> String {
        let mut vars: Vec<(&str, &str)> = with(&[
            ("HD_ASSISTANT_MODEL_PROVIDER", "ollama"),
            ("HD_ASSISTANT_MODEL", "m"),
        ]);
        vars.push((variable, value));
        error(&vars)
    }

    #[test]
    fn every_secret_can_be_read_from_a_file_and_giving_both_is_refused() {
        // The variables that are not secrets. One read but listed neither here nor in SECRETS
        // fails the test, so that a new secret cannot be read without its file.
        const SETTINGS: [&str; 9] = [
            "HD_ASSISTANT_LISTEN",
            "HD_ASSISTANT_CORE_URL",
            "HD_ASSISTANT_MODEL_PROVIDER",
            "HD_ASSISTANT_MODEL",
            "HD_ASSISTANT_MODEL_URL",
            "HD_ASSISTANT_MODEL_EFFORT",
            "HD_ASSISTANT_AZURE_API_VERSION",
            "HD_ASSISTANT_THINKING",
            "HD_ASSISTANT_MODEL_CONCURRENCY",
        ];
        let read = std::cell::RefCell::new(std::collections::BTreeSet::new());
        for provider in Provider::ALL {
            let mut vars = HashMap::from([
                ("HD_ASSISTANT_LISTEN", "127.0.0.1:8090"),
                ("HD_ASSISTANT_CORE_URL", "http://127.0.0.1:8080"),
                ("HD_ASSISTANT_TOKEN", "t"),
                ("HD_ASSISTANT_EVENT_SECRET", "s"),
                ("HD_ASSISTANT_MODEL_PROVIDER", provider.name()),
                ("HD_ASSISTANT_MODEL", "m"),
                ("HD_ASSISTANT_MODEL_URL", "http://127.0.0.1:8000/v1"),
                ("HD_ASSISTANT_API_KEY", "k"),
                ("HD_ASSISTANT_MODEL_EFFORT", "low"),
                ("HD_ASSISTANT_AZURE_API_VERSION", "2024-10-21"),
                ("HD_ASSISTANT_MODEL_CONCURRENCY", "2"),
            ]);
            if provider == Provider::OpenAiCompatible {
                vars.insert("HD_ASSISTANT_THINKING", "off");
            }
            let configured = Config::from_vars(|name| {
                read.borrow_mut().insert(name.to_owned());
                vars.get(name).map(|value| value.to_string())
            });
            // A configuration refused early would leave the later variables unread.
            if let Err(error) = configured {
                panic!("{provider}: {error:#}");
            }
        }
        let read = read.into_inner();
        for name in &read {
            let secret = name.strip_suffix("_FILE").unwrap_or(name);
            assert!(
                SETTINGS.contains(&name.as_str()) || SECRETS.contains(&secret),
                "{name} is read but is neither in SECRETS nor a setting: a secret goes in \
                 SECRETS and is read through read_secret, a setting goes in this test's list"
            );
        }

        let dir = dir();
        let file = dir.join("secret");
        std::fs::write(&file, "invented-secret-from-file").unwrap();
        let file = file.display().to_string();
        for secret in SECRETS {
            let secret_file = format!("{secret}_FILE");
            assert!(
                read.contains(secret) && read.contains(&secret_file),
                "{secret} is in SECRETS, but {secret} and {secret_file} are not both read"
            );
            // ollama needs no key: giving both is refused all the same.
            let mut vars: Vec<(&str, &str)> = with(&[
                ("HD_ASSISTANT_MODEL_PROVIDER", "ollama"),
                ("HD_ASSISTANT_MODEL", "m"),
                ("HD_ASSISTANT_API_KEY", "k"),
            ]);
            vars.push((&secret_file, &file));
            let error = error(&vars);
            assert!(
                error.contains(&format!("{secret} ")) && error.contains(&secret_file),
                "{error}"
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn there_is_no_default_provider_or_model() {
        let missing = error(&with(&[]));
        assert!(
            missing.contains("set HD_ASSISTANT_MODEL_PROVIDER"),
            "{missing}"
        );
        for name in [
            "anthropic",
            "openai",
            "mistral",
            "gemini",
            "azure-openai",
            "ollama",
            "openai-compatible",
        ] {
            assert!(missing.contains(name), "{missing} lists {name}");
        }
        let unknown = error(&with(&[("HD_ASSISTANT_MODEL_PROVIDER", "carrier-pigeon")]));
        assert!(unknown.contains("\"carrier-pigeon\"") && unknown.contains("azure-openai"));

        let no_model = error(&with(&[
            ("HD_ASSISTANT_MODEL_PROVIDER", "anthropic"),
            ("HD_ASSISTANT_API_KEY", "k"),
        ]));
        assert!(no_model.contains("set HD_ASSISTANT_MODEL"), "{no_model}");
        let ollama_no_model = error(&with(&[("HD_ASSISTANT_MODEL_PROVIDER", "ollama")]));
        assert!(ollama_no_model.contains("set HD_ASSISTANT_MODEL"));
    }

    #[test]
    fn hosted_providers_need_a_key_and_have_a_url() {
        for (name, url) in [
            ("anthropic", "https://api.anthropic.com"),
            ("openai", "https://api.openai.com/v1"),
            ("mistral", "https://api.mistral.ai/v1"),
            (
                "gemini",
                "https://generativelanguage.googleapis.com/v1beta/openai",
            ),
        ] {
            let base = [
                ("HD_ASSISTANT_MODEL_PROVIDER", name),
                ("HD_ASSISTANT_MODEL", "some-model"),
            ];
            let no_key = error(&with(&base));
            assert!(
                no_key.contains("set HD_ASSISTANT_API_KEY"),
                "{name}: {no_key}"
            );
            let mut vars = with(&base);
            vars.push(("HD_ASSISTANT_API_KEY", "k"));
            let c = config(&vars).unwrap();
            assert_eq!(c.model.url, url);
            assert!(c.model.is_external(), "{name}");
        }
    }

    #[test]
    fn anthropic_keeps_its_effort_setting() {
        let base = [
            ("HD_ASSISTANT_MODEL_PROVIDER", "anthropic"),
            ("HD_ASSISTANT_MODEL", "claude-opus-5-5"),
            ("HD_ASSISTANT_API_KEY", "k"),
        ];
        let c = config(&with(&base)).unwrap();
        assert_eq!(c.listen.to_string(), "127.0.0.1:8090");
        assert_eq!(c.core_url, "http://127.0.0.1:8080");
        assert_eq!(c.model.effort.as_deref(), Some(DEFAULT_EFFORT));
        assert_eq!(c.model.endpoint(), "https://api.anthropic.com/v1/messages");
        let mut none = with(&base);
        none.push(("HD_ASSISTANT_MODEL_EFFORT", ""));
        assert_eq!(config(&none).unwrap().model.effort, None);
    }

    #[test]
    fn azure_needs_its_resource_and_uses_the_v1_api_unless_dated() {
        let base = [
            ("HD_ASSISTANT_MODEL_PROVIDER", "azure-openai"),
            ("HD_ASSISTANT_MODEL", "triage-deployment"),
            ("HD_ASSISTANT_API_KEY", "k"),
        ];
        assert!(error(&with(&base)).contains("openai.azure.com"));
        let mut vars = with(&base);
        vars.push(("HD_ASSISTANT_MODEL_URL", "https://desk.openai.azure.com/"));
        assert_eq!(
            config(&vars).unwrap().model.endpoint(),
            "https://desk.openai.azure.com/openai/v1/chat/completions"
        );
        vars.push(("HD_ASSISTANT_AZURE_API_VERSION", "2024-10-21"));
        assert_eq!(
            config(&vars).unwrap().model.endpoint(),
            "https://desk.openai.azure.com/openai/deployments/triage-deployment/chat/completions?api-version=2024-10-21"
        );
        vars.push(("HD_ASSISTANT_AZURE_API_VERSION", "v1"));
        assert_eq!(
            config(&vars).unwrap().model.endpoint(),
            "https://desk.openai.azure.com/openai/v1/chat/completions"
        );
    }

    #[test]
    fn thinking_is_left_to_the_server_unless_set() {
        let compatible = |extra: &[(&'static str, &'static str)]| {
            let mut vars = with(&[
                ("HD_ASSISTANT_MODEL_PROVIDER", "openai-compatible"),
                ("HD_ASSISTANT_MODEL", "m"),
                ("HD_ASSISTANT_MODEL_URL", "http://127.0.0.1:8000/v1"),
            ]);
            vars.extend_from_slice(extra);
            config(&vars)
        };
        assert_eq!(compatible(&[]).unwrap().model.thinking, None);
        let off = compatible(&[("HD_ASSISTANT_THINKING", "off")]).unwrap();
        assert_eq!(off.model.thinking, Some(false));
        let on = compatible(&[("HD_ASSISTANT_THINKING", "on")]).unwrap();
        assert_eq!(on.model.thinking, Some(true));
        assert!(compatible(&[("HD_ASSISTANT_THINKING", "low")]).is_err());
        let elsewhere = error(&with(&[
            ("HD_ASSISTANT_MODEL_PROVIDER", "openai"),
            ("HD_ASSISTANT_MODEL", "m"),
            ("HD_ASSISTANT_API_KEY", "k"),
            ("HD_ASSISTANT_THINKING", "off"),
        ]));
        assert!(
            elsewhere.contains("applies only to openai-compatible"),
            "{elsewhere}"
        );
    }

    #[test]
    fn self_hosted_servers_get_one_model_call_at_a_time() {
        let with_provider = |provider: &'static str, extra: &[(&'static str, &'static str)]| {
            let mut vars = with(&[
                ("HD_ASSISTANT_MODEL_PROVIDER", provider),
                ("HD_ASSISTANT_MODEL", "m"),
                ("HD_ASSISTANT_MODEL_URL", "http://127.0.0.1:8000/v1"),
                ("HD_ASSISTANT_API_KEY", "k"),
            ]);
            vars.extend_from_slice(extra);
            config(&vars)
        };
        assert_eq!(with_provider("ollama", &[]).unwrap().model.concurrency, 1);
        assert_eq!(
            with_provider("openai-compatible", &[])
                .unwrap()
                .model
                .concurrency,
            1
        );
        assert_eq!(
            with_provider("anthropic", &[]).unwrap().model.concurrency,
            4
        );
        assert_eq!(with_provider("openai", &[]).unwrap().model.concurrency, 4);
        let two = with_provider("ollama", &[("HD_ASSISTANT_MODEL_CONCURRENCY", "2")]);
        assert_eq!(two.unwrap().model.concurrency, 2);
        for bad in ["0", "many", "-1"] {
            let refused = with_provider("ollama", &[("HD_ASSISTANT_MODEL_CONCURRENCY", bad)]);
            assert!(
                format!("{:#}", refused.unwrap_err()).contains("HD_ASSISTANT_MODEL_CONCURRENCY")
            );
        }
    }

    #[test]
    fn only_local_models_skip_the_privacy_warning() {
        let ollama = config(&with(&[
            ("HD_ASSISTANT_MODEL_PROVIDER", "ollama"),
            ("HD_ASSISTANT_MODEL", "a-local-model"),
        ]))
        .unwrap();
        assert_eq!(ollama.model.api_key, None);
        assert_eq!(
            ollama.model.endpoint(),
            "http://127.0.0.1:11434/v1/chat/completions"
        );
        assert!(!ollama.model.is_external());
        let remote_ollama = config(&with(&[
            ("HD_ASSISTANT_MODEL_PROVIDER", "ollama"),
            ("HD_ASSISTANT_MODEL", "a-local-model"),
            ("HD_ASSISTANT_MODEL_URL", "http://192.0.2.20:11434"),
        ]))
        .unwrap();
        assert!(
            remote_ollama.model.is_external(),
            "Ollama elsewhere warns too"
        );

        let compatible = |url: &'static str| {
            config(&with(&[
                ("HD_ASSISTANT_MODEL_PROVIDER", "openai-compatible"),
                ("HD_ASSISTANT_MODEL", "a-local-model"),
                ("HD_ASSISTANT_MODEL_URL", url),
            ]))
            .unwrap()
            .model
        };
        let local = compatible("http://127.0.0.1:8000");
        assert_eq!(
            local.endpoint(),
            "http://127.0.0.1:8000/v1/chat/completions"
        );
        assert!(!local.is_external());
        assert!(!compatible("http://localhost:8000/v1").is_external());
        assert!(compatible("http://192.0.2.10:8000/v1").is_external());
        assert!(compatible("https://models.example.com/v1").is_external());
        assert!(
            error(&with(&[
                ("HD_ASSISTANT_MODEL_PROVIDER", "openai-compatible"),
                ("HD_ASSISTANT_MODEL", "m"),
            ]))
            .contains("set HD_ASSISTANT_MODEL_URL")
        );
    }
}
