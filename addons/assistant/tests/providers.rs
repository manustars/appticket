//! Each provider, configured as an installer would through `HD_ASSISTANT_*` variables, against a
//! fake server on a local port: the request goes to the provider's path with its auth header and
//! JSON output setting, and the answer comes back parsed.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, Uri},
};
use hd_assistant::config::Config;
use hd_assistant::model::{Model, Task};
use hd_assistant::prompts;
use serde_json::{Value, json};

#[derive(Clone, Debug)]
struct Seen {
    uri: String,
    headers: HeaderMap,
    body: Value,
}

type Log = Arc<Mutex<Vec<Seen>>>;

const ANSWER: &str = r#"{"action":"none","customer":null,"ticket_number":null,"evidence":null,"rationale":"Unsure."}"#;

/// Answers any POST like the provider would: the Messages API shape on `/v1/messages`, the chat
/// completions shape anywhere else.
async fn fake(log: Log) -> String {
    async fn answer(
        State(log): State<Log>,
        uri: Uri,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        log.lock().unwrap().push(Seen {
            uri: uri.to_string(),
            headers,
            body,
        });
        if uri.path() == "/v1/messages" {
            return Json(json!({
                "type": "message", "role": "assistant", "stop_reason": "end_turn",
                "content": [{"type": "thinking", "thinking": "", "signature": "x"},
                            {"type": "text", "text": ANSWER}]
            }));
        }
        Json(json!({
            "id": "chatcmpl-test", "object": "chat.completion",
            "choices": [{"index": 0, "finish_reason": "stop",
                         "message": {"role": "assistant", "content": ANSWER, "refusal": null}}]
        }))
    }
    let app = Router::new().fallback(answer).with_state(log);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

/// Configures `provider` from variables, with the model URL on the fake server, asks one
/// triage question and returns what the server saw.
async fn ask(provider: &str, model: &str, url_path: &str, extra: &[(&str, &str)]) -> Seen {
    let log: Log = Arc::default();
    let server = fake(log.clone()).await;
    let mut vars: HashMap<String, String> = [
        ("HD_ASSISTANT_CORE_URL", "http://127.0.0.1:8080"),
        ("HD_ASSISTANT_TOKEN", "t"),
        ("HD_ASSISTANT_EVENT_SECRET", "s"),
        ("HD_ASSISTANT_MODEL_PROVIDER", provider),
        ("HD_ASSISTANT_MODEL", model),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    vars.insert(
        "HD_ASSISTANT_MODEL_URL".into(),
        format!("{server}{url_path}"),
    );
    for (k, v) in extra {
        vars.insert(k.to_string(), v.to_string());
    }
    let config = Config::from_vars(|name| vars.get(name).cloned()).unwrap();
    let model = Model::new(config.model).unwrap();
    let answer = model
        .answer(&Task {
            name: "triage",
            system: prompts::TRIAGE_SYSTEM,
            input: "<message>Hello</message>".into(),
            schema: prompts::triage_schema(),
        })
        .await
        .unwrap();
    assert_eq!(answer["action"], "none", "{provider}: the answer is parsed");
    let seen = log.lock().unwrap();
    assert_eq!(seen.len(), 1);
    seen[0].clone()
}

fn header<'a>(seen: &'a Seen, name: &str) -> Option<&'a str> {
    seen.headers.get(name).and_then(|v| v.to_str().ok())
}

/// JSON Schema output in the chat completions shape, strict, with the triage schema.
fn assert_schema_output(seen: &Seen) {
    let format = &seen.body["response_format"];
    assert_eq!(format["type"], "json_schema", "{}", seen.body);
    assert_eq!(format["json_schema"]["name"], "triage");
    assert_eq!(format["json_schema"]["strict"], true);
    assert_eq!(format["json_schema"]["schema"], prompts::triage_schema());
    assert_eq!(seen.body["messages"][0]["role"], "system");
    assert_eq!(seen.body["messages"][1]["role"], "user");
    // Also in the instructions, for servers that ignore `response_format`.
    let system = seen.body["messages"][0]["content"].as_str().unwrap();
    assert!(system.starts_with(prompts::TRIAGE_SYSTEM));
    assert!(system.contains("follows this JSON Schema") && system.contains("\"open_ticket\""));
    for absent in ["temperature", "max_tokens"] {
        assert!(seen.body.get(absent).is_none(), "{absent} is not sent");
    }
}

const KEY: (&str, &str) = ("HD_ASSISTANT_API_KEY", "provider-test-key");

#[tokio::test]
async fn anthropic() {
    let seen = ask("anthropic", "claude-opus-5-5", "", &[KEY]).await;
    assert_eq!(seen.uri, "/v1/messages");
    assert_eq!(header(&seen, "x-api-key"), Some("provider-test-key"));
    assert_eq!(header(&seen, "anthropic-version"), Some("2026-10-21"));
    assert_eq!(header(&seen, "authorization"), None);
    assert_eq!(seen.body["model"], "claude-opus-5-5");
    assert_eq!(seen.body["output_config"]["format"]["type"], "json_schema");
    assert_eq!(seen.body["output_config"]["effort"], "medium");
    assert_eq!(seen.body["system"], prompts::TRIAGE_SYSTEM);
}

#[tokio::test]
async fn openai() {
    let seen = ask("openai", "gpt-test", "/v1", &[KEY]).await;
    assert_eq!(seen.uri, "/v1/chat/completions");
    assert_eq!(
        header(&seen, "authorization"),
        Some("Bearer provider-test-key")
    );
    assert_eq!(seen.body["model"], "gpt-test");
    assert_schema_output(&seen);
}

#[tokio::test]
async fn mistral() {
    let seen = ask("mistral", "mistral-test", "/v1", &[KEY]).await;
    assert_eq!(seen.uri, "/v1/chat/completions");
    assert_eq!(
        header(&seen, "authorization"),
        Some("Bearer provider-test-key")
    );
    assert_schema_output(&seen);
}

#[tokio::test]
async fn gemini() {
    let seen = ask("gemini", "gemini-test", "/v1beta/openai", &[KEY]).await;
    assert_eq!(seen.uri, "/v1beta/openai/chat/completions");
    assert_eq!(
        header(&seen, "authorization"),
        Some("Bearer provider-test-key")
    );
    assert_schema_output(&seen);
}

#[tokio::test]
async fn azure_openai() {
    let seen = ask(
        "azure-openai",
        "triage-deployment",
        "",
        &[KEY, ("HD_ASSISTANT_AZURE_API_VERSION", "2024-10-21")],
    )
    .await;
    assert_eq!(
        seen.uri,
        "/openai/deployments/triage-deployment/chat/completions?api-version=2024-10-21"
    );
    assert_eq!(header(&seen, "api-key"), Some("provider-test-key"));
    assert_eq!(header(&seen, "authorization"), None);
    assert_schema_output(&seen);

    let v1 = ask(
        "azure-openai",
        "triage-deployment",
        "",
        &[KEY, ("HD_ASSISTANT_AZURE_API_VERSION", "v1")],
    )
    .await;
    assert_eq!(v1.uri, "/openai/v1/chat/completions");
    assert_eq!(v1.body["model"], "triage-deployment");
    assert_eq!(header(&v1, "api-key"), Some("provider-test-key"));
}

#[tokio::test]
async fn ollama() {
    let seen = ask("ollama", "local-test", "", &[]).await;
    assert_eq!(seen.uri, "/v1/chat/completions");
    assert_eq!(header(&seen, "authorization"), None, "no key, no header");
    assert_eq!(seen.body["model"], "local-test");
    assert_schema_output(&seen);
}

#[tokio::test]
async fn openai_compatible() {
    let seen = ask("openai-compatible", "served-test", "", &[KEY]).await;
    assert_eq!(seen.uri, "/v1/chat/completions", "a bare address means /v1");
    assert_eq!(
        header(&seen, "authorization"),
        Some("Bearer provider-test-key")
    );
    assert_eq!(seen.body["response_format"], json!({"type": "json_object"}));
    let system = seen.body["messages"][0]["content"].as_str().unwrap();
    assert!(system.starts_with(prompts::TRIAGE_SYSTEM));
    assert!(system.contains("follows this JSON Schema"));
    assert!(
        system.contains("\"open_ticket\""),
        "the schema is in the instructions"
    );

    let keyless = ask("openai-compatible", "served-test", "/api/v1", &[]).await;
    assert_eq!(keyless.uri, "/api/v1/chat/completions");
    assert_eq!(header(&keyless, "authorization"), None);
}
