//! Asking the language model for one JSON answer, over the Claude Messages API or the OpenAI chat
//! completions protocol, which the other providers speak.

use std::fmt;
use std::time::Duration;

use serde_json::{Value, json};

use crate::config::{Auth, JsonOutput, ModelSettings, Protocol};

/// The Messages API version the requests are written for.
const ANTHROPIC_VERSION: &str = "2026-10-21";
/// Lets the Claude API retry a request its safety classifiers decline on another model.
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
/// Models that take `fallbacks: "default"`.
const FALLBACK_MODELS: &[&str] = &[
    "claude-opus-5-5",
    "claude-opus-5",
    "claude-sonnet-5-5",
    "claude-fable-5-2",
    "claude-fable-5-1",
    "claude-fable-5",
];
/// Room for the model's thinking and its answer; the answers themselves are short.
const MAX_TOKENS: u32 = 16_000;
/// How long one request may take: models can need tens of seconds.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);

/// One question for the model.
#[derive(Debug, Clone)]
pub struct Task<'a> {
    /// A short name for logs and for the schema.
    pub name: &'static str,
    pub system: &'a str,
    pub input: String,
    /// The JSON Schema the answer must follow.
    pub schema: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelError {
    /// The model declined to answer.
    Refused,
    /// The answer was cut off before it ended.
    Truncated,
    /// The answer is not the JSON object asked for.
    Invalid(String),
    /// The service is busy, failing or unreachable: worth trying again later.
    Unavailable(String),
    /// The request was refused (bad key, unknown model, bad request): retrying will not help.
    Rejected(String),
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModelError::Refused => f.write_str("the model declined to answer"),
            ModelError::Truncated => f.write_str("the model's answer was cut off"),
            ModelError::Invalid(why) => write!(f, "the model's answer is not usable: {why}"),
            ModelError::Unavailable(why) => write!(f, "the model is unavailable: {why}"),
            ModelError::Rejected(why) => write!(f, "the model service refused the request: {why}"),
        }
    }
}

impl std::error::Error for ModelError {}

#[derive(Clone)]
pub struct Model {
    settings: ModelSettings,
    http: reqwest::Client,
    /// Waits before the second and third attempt of a request that met a busy service.
    retry_delays: Vec<Duration>,
}

impl fmt::Debug for Model {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Model")
            .field("provider", &self.settings.provider)
            .field("url", &self.settings.url)
            .field("model", &self.settings.model)
            .finish_non_exhaustive()
    }
}

impl Model {
    pub fn new(settings: ModelSettings) -> anyhow::Result<Model> {
        Ok(Model {
            settings,
            http: hd_addon_sdk::http_client(REQUEST_TIMEOUT)?,
            retry_delays: vec![Duration::from_secs(5), Duration::from_secs(20)],
        })
    }

    /// Replaces the waits between attempts (tests use none).
    pub fn with_retry_delays(mut self, delays: Vec<Duration>) -> Model {
        self.retry_delays = delays;
        self
    }

    pub fn settings(&self) -> &ModelSettings {
        &self.settings
    }

    /// The model's answer to `task`, parsed as a JSON object. Retries a busy service twice.
    pub async fn answer(&self, task: &Task<'_>) -> Result<Value, ModelError> {
        let mut attempt = 0;
        loop {
            let outcome = match self.settings.provider.protocol() {
                Protocol::Messages => self.anthropic(task).await,
                Protocol::ChatCompletions => self.chat_completions(task).await,
            };
            match outcome {
                Err(Attempt::Busy { why, retry_after }) => {
                    let Some(delay) = self.retry_delays.get(attempt) else {
                        return Err(ModelError::Unavailable(why));
                    };
                    let delay = retry_after.map_or(*delay, |r| r.min(Duration::from_secs(60)));
                    tracing::info!(task = task.name, %why, ?delay, "the model is busy, trying again");
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
                Err(Attempt::Failed(error)) => return Err(error),
                Ok(text) => return parse_object(&text),
            }
        }
    }

    async fn send(&self, request: reqwest::RequestBuilder) -> Result<Value, Attempt> {
        let response = request.send().await.map_err(|e| Attempt::Busy {
            why: format!("{e}"),
            retry_after: None,
        })?;
        let status = response.status();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse().ok())
            .map(Duration::from_secs);
        let text = response.text().await.map_err(|e| Attempt::Busy {
            why: format!("{e}"),
            retry_after: None,
        })?;
        if status.is_success() {
            return serde_json::from_str(&text)
                .map_err(|e| Attempt::Failed(ModelError::Invalid(format!("not JSON: {e}"))));
        }
        let why = format!("{status}: {}", error_message(&text));
        if status.as_u16() == 429 || status.as_u16() == 529 || status.is_server_error() {
            Err(Attempt::Busy { why, retry_after })
        } else {
            Err(Attempt::Failed(ModelError::Rejected(why)))
        }
    }

    async fn anthropic(&self, task: &Task<'_>) -> Result<String, Attempt> {
        let s = &self.settings;
        let mut output = json!({
            "format": { "type": "json_schema", "schema": task.schema },
        });
        if let Some(effort) = &s.effort {
            output["effort"] = json!(effort);
        }
        let mut body = json!({
            "model": s.model,
            "max_tokens": MAX_TOKENS,
            "system": task.system,
            "messages": [{ "role": "user", "content": task.input }],
            "output_config": output,
        });
        let mut request = self
            .http
            .post(s.endpoint())
            .header("x-api-key", s.api_key.as_deref().unwrap_or_default())
            .header("anthropic-version", ANTHROPIC_VERSION);
        if FALLBACK_MODELS.contains(&s.model.as_str()) {
            body["fallbacks"] = json!("default");
            request = request.header("anthropic-beta", FALLBACK_BETA);
        }
        let response = self.send(request.json(&body)).await?;
        anthropic_text(&response).map_err(Attempt::Failed)
    }

    async fn chat_completions(&self, task: &Task<'_>) -> Result<String, Attempt> {
        let s = &self.settings;
        let response_format = match s.provider.json_output() {
            JsonOutput::Schema => json!({
                "type": "json_schema",
                "json_schema": { "name": task.name, "strict": true, "schema": task.schema },
            }),
            JsonOutput::Object => json!({ "type": "json_object" }),
        };
        // The schema also goes in the instructions: some servers (versions of Ollama, Gemini's
        // compatibility layer) have been reported to ignore `response_format`, and a model that
        // never sees the shape would never give a usable answer.
        let system = format!(
            "{}\n\nAnswer with a single JSON object that follows this JSON Schema, and nothing \
             else:\n{}",
            task.system, task.schema
        );
        // No temperature or token limit: reasoning models refuse some of them, and the
        // defaults suit short answers.
        let body = json!({
            "model": s.model,
            "stream": false,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": task.input },
            ],
            "response_format": response_format,
        });
        let mut request = self.http.post(s.endpoint());
        if let Some(key) = &s.api_key {
            request = match s.provider.auth() {
                Auth::ApiKeyHeader => request.header("api-key", key),
                Auth::XApiKey => request.header("x-api-key", key),
                Auth::Bearer => request.bearer_auth(key),
            };
        }
        let response = self.send(request.json(&body)).await?;
        chat_text(&response).map_err(Attempt::Failed)
    }
}

/// The text of a chat completions response, after checking why it ended.
fn chat_text(response: &Value) -> Result<String, ModelError> {
    let choice = &response["choices"][0];
    match choice["finish_reason"].as_str() {
        // `model_length` is Mistral's: the model's context ran out.
        Some("length" | "model_length") => return Err(ModelError::Truncated),
        Some("content_filter") => return Err(ModelError::Refused),
        Some("error") => return Err(ModelError::Invalid("the model stopped on an error".into())),
        _ => {}
    }
    let message = &choice["message"];
    // With JSON Schema output, OpenAI reports a declined request in `refusal`.
    if message["refusal"]
        .as_str()
        .is_some_and(|r| !r.trim().is_empty())
    {
        return Err(ModelError::Refused);
    }
    message["content"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| ModelError::Invalid("no message content".into()))
}

enum Attempt {
    Busy {
        why: String,
        retry_after: Option<Duration>,
    },
    Failed(ModelError),
}

/// The text of a Messages API response. Checks the stop reason first; with thinking on, the
/// text is not the first block, and after a server-side fallback only the text that follows the
/// last `fallback` block is the final model's.
fn anthropic_text(response: &Value) -> Result<String, ModelError> {
    match response["stop_reason"].as_str() {
        Some("end_turn" | "stop_sequence") => {}
        Some("refusal") => return Err(ModelError::Refused),
        Some("max_tokens") => return Err(ModelError::Truncated),
        other => {
            return Err(ModelError::Invalid(format!(
                "unexpected stop reason {other:?}"
            )));
        }
    }
    let mut text = String::new();
    for block in response["content"].as_array().into_iter().flatten() {
        match block["type"].as_str() {
            Some("fallback") => text.clear(),
            Some("text") => text += block["text"].as_str().unwrap_or_default(),
            _ => {}
        }
    }
    if text.trim().is_empty() {
        return Err(ModelError::Invalid("no text in the answer".into()));
    }
    Ok(text)
}

/// The error sentence of a model service's error body, or the start of the body.
fn error_message(body: &str) -> String {
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let message = parsed.as_ref().and_then(|v| {
        v["error"]["message"]
            .as_str()
            .or_else(|| v["error"].as_str())
            .map(str::to_owned)
    });
    message.unwrap_or_else(|| body.chars().take(200).collect())
}

/// Parses an answer that should be one JSON object, tolerating a Markdown code fence around it
/// (some local models add one).
fn parse_object(text: &str) -> Result<Value, ModelError> {
    let text = text.trim();
    let unfenced = text
        .strip_prefix("```json")
        .or_else(|| text.strip_prefix("```"))
        .and_then(|t| t.trim_end().strip_suffix("```"))
        .unwrap_or(text);
    match serde_json::from_str::<Value>(unfenced.trim()) {
        Ok(value @ Value::Object(_)) => Ok(value),
        Ok(_) => Err(ModelError::Invalid(
            "the answer is not a JSON object".into(),
        )),
        Err(e) => Err(ModelError::Invalid(format!("the answer is not JSON: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_text_after_thinking_and_fallbacks_is_the_answer() {
        let response = json!({
            "stop_reason": "end_turn",
            "content": [
                { "type": "thinking", "thinking": "", "signature": "x" },
                { "type": "text", "text": "{\"partial\":" },
                { "type": "fallback", "from": { "model": "a" }, "to": { "model": "b" } },
                { "type": "text", "text": "{\"action\":\"none\"}" }
            ]
        });
        assert_eq!(anthropic_text(&response).unwrap(), "{\"action\":\"none\"}");
        assert_eq!(
            anthropic_text(&json!({ "stop_reason": "refusal", "content": [] })),
            Err(ModelError::Refused)
        );
        assert_eq!(
            anthropic_text(&json!({ "stop_reason": "max_tokens", "content": [] })),
            Err(ModelError::Truncated)
        );
    }

    #[test]
    fn chat_completions_report_refusals_and_cut_answers() {
        let answer = |finish: &str, message: Value| {
            chat_text(&json!({ "choices": [{ "finish_reason": finish, "message": message }] }))
        };
        assert_eq!(
            answer("stop", json!({ "content": "{}", "refusal": null })),
            Ok("{}".into())
        );
        assert_eq!(
            answer(
                "stop",
                json!({ "content": null, "refusal": "I can't help with that." })
            ),
            Err(ModelError::Refused)
        );
        assert_eq!(
            answer("content_filter", json!({ "content": "" })),
            Err(ModelError::Refused)
        );
        assert_eq!(
            answer("length", json!({ "content": "{\"a\":" })),
            Err(ModelError::Truncated)
        );
        assert_eq!(
            answer("model_length", json!({ "content": "{\"a\":" })),
            Err(ModelError::Truncated)
        );
    }

    #[test]
    fn answers_must_be_json_objects() {
        assert!(parse_object("```json\n{\"a\": 1}\n```").is_ok());
        assert!(parse_object(" {\"a\": 1} ").is_ok());
        assert!(matches!(parse_object("[1]"), Err(ModelError::Invalid(_))));
        assert!(matches!(
            parse_object("Sure! Here it is"),
            Err(ModelError::Invalid(_))
        ));
    }
}
