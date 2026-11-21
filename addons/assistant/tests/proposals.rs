//! Each event against a fake core and a fake model on local ports: the right proposals reach
//! the core, and an unusable answer from the model reaches it as no proposal at all.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use hd_addon_sdk::api::Priority;
use hd_addon_sdk::events::{HEADER_SIGNATURE, HEADER_TIMESTAMP};
use hd_addon_sdk::{Client, Content, Envelope, Target, Triage, Verifier, signature};
use hd_assistant::config::{ModelSettings, Provider};
use hd_assistant::model::Model;
use hd_assistant::{Assistant, Proposed, server};
use serde_json::{Value, json};
use uuid::Uuid;

const TOKEN: &str = "assistant-test-token";
const BRIGHTFIELD: &str = "0e7b6a52-5d0f-4d5e-8a8e-3f2b1c0d9e8f";
const NORTHGATE: &str = "1f8c7b63-6e10-4e6f-9b9f-4a3c2d1e0f90";
const PC: &str = "5a1d0c3e-7b2f-4e6a-9c8d-1f0e2d3c4b5a";
const PRINTER: &str = "6b2e1d4f-8c30-4f7b-8d9e-2a1f3e4d5c6b";
const MESSAGE: &str = "7b8c9d0e-1f2a-4b3c-8d5e-6f7a8b9c0d1e";

fn uuid(s: &str) -> Uuid {
    Uuid::parse_str(s).unwrap()
}

// ----- the fake core -----

#[derive(Default)]
struct Core {
    tickets: HashMap<i64, Value>,
    /// Open tickets by requester address.
    of_requester: HashMap<String, Value>,
    /// Ticket numbers on which every proposal is refused, as if just closed.
    refuse_on: Vec<i64>,
    proposals: Vec<Value>,
    calls: Vec<String>,
}

type SharedCore = Arc<Mutex<Core>>;

fn authorized(headers: &HeaderMap) -> bool {
    headers.get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {TOKEN}"))
}

fn problem(status: StatusCode, sentence: &str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "error": sentence })))
}

async fn start(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

async fn fake_core(core: SharedCore) -> String {
    async fn customers(
        State(core): State<SharedCore>,
        headers: HeaderMap,
    ) -> (StatusCode, Json<Value>) {
        if !authorized(&headers) {
            return problem(StatusCode::UNAUTHORIZED, "Not signed in.");
        }
        core.lock().unwrap().calls.push("customers".into());
        (
            StatusCode::OK,
            Json(json!([
                {"id": BRIGHTFIELD, "name": "Brightfield", "is_help_desk": false,
                 "domains": ["brightfield.test"]},
                {"id": NORTHGATE, "name": "Northgate Studio", "is_help_desk": true,
                 "domains": ["northgate.test"]}
            ])),
        )
    }
    async fn assets(State(core): State<SharedCore>, Path(id): Path<Uuid>) -> Json<Value> {
        core.lock().unwrap().calls.push(format!("assets {id}"));
        if id != uuid(BRIGHTFIELD) {
            return Json(json!([]));
        }
        Json(json!([
            {"id": PC, "customer_id": BRIGHTFIELD, "customer_name": "Brightfield",
             "kind": "computer", "name": "PC-0042", "source_addon": null, "retired_at": null},
            {"id": PRINTER, "customer_id": BRIGHTFIELD, "customer_name": "Brightfield",
             "kind": "printer", "name": "Printer on the second floor", "source_addon": null,
             "retired_at": null}
        ]))
    }
    async fn tickets(
        State(core): State<SharedCore>,
        Query(q): Query<HashMap<String, String>>,
    ) -> Json<Value> {
        let core = core.lock().unwrap();
        let email = q.get("requester_email").cloned().unwrap_or_default();
        Json(core.of_requester.get(&email).cloned().unwrap_or(json!([])))
    }
    async fn ticket(
        State(core): State<SharedCore>,
        Path(number): Path<i64>,
    ) -> (StatusCode, Json<Value>) {
        let mut core = core.lock().unwrap();
        core.calls.push(format!("ticket {number}"));
        match core.tickets.get(&number) {
            Some(t) => (StatusCode::OK, Json(t.clone())),
            None => problem(StatusCode::NOT_FOUND, "Ticket not found."),
        }
    }
    async fn propose(
        State(core): State<SharedCore>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        if !authorized(&headers) {
            return problem(StatusCode::UNAUTHORIZED, "Not signed in.");
        }
        let mut core = core.lock().unwrap();
        if let Some(n) = body["ticket_number"].as_i64()
            && core.refuse_on.contains(&n)
        {
            return problem(StatusCode::CONFLICT, "The ticket is closed.");
        }
        core.proposals.push(body);
        (StatusCode::CREATED, Json(json!({"id": Uuid::new_v4()})))
    }
    let app = Router::new()
        .route("/api/addon/customers", get(customers))
        .route("/api/addon/customers/{id}/assets", get(assets))
        .route("/api/addon/tickets", get(tickets))
        .route("/api/addon/tickets/{number}", get(ticket))
        .route("/api/addon/proposals", post(propose))
        .with_state(core);
    start(app).await
}

fn ticket_json(
    number: i64,
    state: &str,
    requester: bool,
    linked: &[&str],
    last_from: &str,
) -> Value {
    let linked: Vec<Value> = linked
        .iter()
        .map(|id| json!({"id": id, "kind": "computer", "name": "PC-0042"}))
        .collect();
    let mut timeline = vec![
        json!({
            "kind": "message", "id": Uuid::new_v4(), "direction": "inbound",
            "from_address": "ada@brightfield.test", "from_name": "Ada Ferrini", "to_address": null,
            "author_name": null, "subject": "PC will not start",
            "body_text": "Buongiorno, il PC-0042 mostra solo uno schermo nero e non posso lavorare.",
            "created_at": "2026-11-21T09:00:00Z", "sent_at": null, "last_error": null,
            "gave_up": false
        }),
        json!({
            "kind": "note", "id": Uuid::new_v4(), "author_name": "Marco Lenti",
            "body": "Internal: the customer is late with payments.", "created_at": "2026-11-21T09:01:00Z"
        }),
    ];
    if last_from == "help desk" {
        timeline.push(json!({
            "kind": "message", "id": Uuid::new_v4(), "direction": "outbound",
            "from_address": "helpdesk@helpdesk.test", "from_name": null,
            "to_address": "ada@brightfield.test", "author_name": "Marco Lenti",
            "subject": "Re: PC will not start", "body_text": "We are looking into it.",
            "created_at": "2026-11-21T09:02:00Z", "sent_at": null, "last_error": null,
            "gave_up": false
        }));
    }
    json!({
        "id": Uuid::new_v4(), "number": number, "title": "PC will not start", "state": state,
        "priority": "normal",
        "customer": {"id": BRIGHTFIELD, "name": "Brightfield", "is_help_desk": false},
        "requester": if requester {
            json!({"id": Uuid::new_v4(), "email": "ada@brightfield.test", "name": "Ada Ferrini",
                   "ticket_count": 1})
        } else { Value::Null },
        "assignee": null, "assets": linked, "services": [], "parent": null, "children": [],
        "proposals": [], "created_at": "2026-11-21T09:00:00Z",
        "updated_at": "2026-11-21T09:00:00Z", "timeline": timeline
    })
}

// ----- the fake model -----

#[derive(Default)]
struct ModelServer {
    /// Answers given in order: a status and a body.
    answers: VecDeque<(u16, Value)>,
    /// What each request carried.
    requests: Vec<(HeaderMap, Value)>,
}

type SharedModel = Arc<Mutex<ModelServer>>;

async fn fake_model(model: SharedModel) -> String {
    async fn answer(
        State(model): State<SharedModel>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        let mut model = model.lock().unwrap();
        model.requests.push((headers, body));
        let (status, body) = model.answers.pop_front().unwrap_or((
            500,
            json!({"type": "error", "error": {"type": "api_error",
                "message": "no answer queued"}}),
        ));
        (StatusCode::from_u16(status).unwrap(), Json(body))
    }
    let app = Router::new()
        .route("/v1/messages", post(answer))
        .route("/v1/chat/completions", post(answer))
        .with_state(model);
    start(app).await
}

/// A Messages API response whose text is `text`, after an (omitted) thinking block.
fn claude_text(text: &str) -> (u16, Value) {
    (
        200,
        json!({
            "id": "msg_test", "type": "message", "role": "assistant", "model": "claude-opus-5-5",
            "stop_reason": "end_turn", "stop_details": null,
            "content": [
                {"type": "thinking", "thinking": "", "signature": "c2lnbmF0dXJl"},
                {"type": "text", "text": text}
            ],
            "usage": {"input_tokens": 900, "output_tokens": 120}
        }),
    )
}

fn claude(answer: Value) -> (u16, Value) {
    claude_text(&answer.to_string())
}

// ----- the setup -----

struct World {
    core: SharedCore,
    model: SharedModel,
    assistant: Assistant,
}

async fn world(provider: Provider) -> World {
    let core: SharedCore = Arc::default();
    let model: SharedModel = Arc::default();
    let core_url = fake_core(core.clone()).await;
    let model_url = fake_model(model.clone()).await;
    let settings = ModelSettings {
        provider,
        url: model_url,
        model: match provider {
            Provider::Anthropic => "claude-opus-5-5".into(),
            Provider::OpenAiCompatible => "a-local-model".into(),
        },
        api_key: Some("model-test-key".into()),
        effort: (provider == Provider::Anthropic).then(|| "medium".into()),
    };
    let assistant = Assistant::new(
        Client::new(&core_url, TOKEN).unwrap(),
        Model::new(settings)
            .unwrap()
            .with_retry_delays(vec![Duration::ZERO]),
    );
    World {
        core,
        model,
        assistant,
    }
}

impl World {
    fn answer(&self, answer: (u16, Value)) -> &Self {
        self.model.lock().unwrap().answers.push_back(answer);
        self
    }

    fn requests(&self) -> Vec<(HeaderMap, Value)> {
        self.model.lock().unwrap().requests.clone()
    }

    fn posted(&self) -> Vec<Value> {
        self.core.lock().unwrap().proposals.clone()
    }

    async fn process(&self, kind: &str, data: Value) -> Vec<Proposed> {
        self.assistant.process(&envelope(kind, data)).await.unwrap()
    }
}

fn envelope(kind: &str, data: Value) -> Envelope {
    serde_json::from_value(json!({
        "id": Uuid::new_v4(), "kind": kind, "created_at": "2026-11-21T09:00:00Z", "data": data
    }))
    .unwrap()
}

fn inbox_message(from: &str, subject: &str, body: &str) -> Value {
    json!({
        "message_id": MESSAGE, "ticket_number": null, "from_address": from,
        "from_name": "Ada Ferrini", "subject": subject, "body_text": body
    })
}

fn triage(
    action: &str,
    customer: Option<&str>,
    ticket: Option<i64>,
    evidence: Option<&str>,
) -> Value {
    json!({
        "action": action, "customer": customer, "ticket_number": ticket, "evidence": evidence,
        "rationale": "A test rationale."
    })
}

const SIGNED: &str =
    "My laptop will not connect to the VPN.\n\nAda Ferrini\nBrightfield, accounting";

// ----- inbox messages -----

#[tokio::test]
async fn a_message_tied_to_a_customer_gets_an_open_ticket_proposal() {
    let w = world(Provider::Anthropic).await;
    w.answer(claude(triage(
        "open_ticket",
        Some("C1"),
        None,
        Some("Brightfield,  Accounting"),
    )));
    let made = w
        .process(
            "message.received",
            inbox_message("ada@home-mail.test", "VPN", SIGNED),
        )
        .await;
    assert_eq!(
        made,
        [Proposed {
            target: Target::Message(uuid(MESSAGE)),
            content: Content::Triage(Triage::OpenTicket {
                customer_id: uuid(BRIGHTFIELD)
            }),
            rationale: "A test rationale.".into(),
        }]
    );
    assert_eq!(
        w.posted(),
        [
            json!({"message_id": MESSAGE, "content": {"kind": "triage", "action": "open_ticket",
            "customer_id": BRIGHTFIELD}, "rationale": "A test rationale."})
        ]
    );

    // The request follows the Messages API: structured output, explicit effort, fallbacks.
    let (headers, body) = &w.requests()[0];
    assert_eq!(headers["x-api-key"], "model-test-key");
    assert_eq!(headers["anthropic-version"], "2026-10-21");
    assert_eq!(headers["anthropic-beta"], "server-side-fallback-2026-07-01");
    assert_eq!(body["model"], "claude-opus-5-5");
    assert_eq!(body["fallbacks"], "default");
    assert_eq!(body["output_config"]["effort"], "medium");
    assert_eq!(body["output_config"]["format"]["type"], "json_schema");
    assert_eq!(
        body["output_config"]["format"]["schema"]["properties"]["action"]["enum"],
        json!(["open_ticket", "attach", "discard", "none"])
    );
    for absent in ["temperature", "thinking", "tool_choice"] {
        assert!(body.get(absent).is_none(), "{absent} is not sent");
    }
    let input = body["messages"][0]["content"].as_str().unwrap();
    assert!(input.contains("C1: Brightfield; mail domains: brightfield.test"));
    assert!(input.contains("C2: Northgate Studio (the help desk's own organization)"));
    assert!(input.contains("Sender's domain: home-mail.test"));
}

#[tokio::test]
async fn a_follow_up_is_attached_to_the_senders_ticket() {
    let w = world(Provider::Anthropic).await;
    w.core.lock().unwrap().of_requester.insert(
        "ada@home-mail.test".into(),
        json!([{"number": 41, "title": "VPN drops", "state": "waiting", "priority": "normal",
                "customer_id": BRIGHTFIELD, "requester_email": "ada@home-mail.test",
                "updated_at": "2026-11-20T09:00:00Z"}]),
    );
    w.answer(claude(triage("attach", None, Some(41), None)));
    let made = w
        .process(
            "message.received",
            inbox_message("ada@home-mail.test", "VPN", "Still broken."),
        )
        .await;
    assert_eq!(
        made[0].content,
        Content::Triage(Triage::Attach { ticket_number: 41 })
    );
    let input = w.requests()[0].1["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(input.contains("#41: \"VPN drops\" (waiting, customer C1)"));

    // A ticket that is not one of the sender's is not proposed.
    w.answer(claude(triage("attach", None, Some(40), None)));
    assert!(
        w.process(
            "message.received",
            inbox_message("ada@home-mail.test", "VPN", "Again.")
        )
        .await
        .is_empty()
    );
}

#[tokio::test]
async fn newsletters_are_proposed_for_discarding_and_doubt_gives_nothing() {
    let w = world(Provider::Anthropic).await;
    w.answer(claude(triage("discard", None, None, None)));
    let made = w
        .process(
            "message.received",
            inbox_message("news@offers.test", "Our autumn deals", "Unsubscribe here."),
        )
        .await;
    assert_eq!(made[0].content, Content::Triage(Triage::Discard));

    w.answer(claude(triage("none", None, None, None)));
    assert!(
        w.process(
            "message.received",
            inbox_message("x@unknown.test", "Hello", "Call me.")
        )
        .await
        .is_empty()
    );
    assert_eq!(w.posted().len(), 1);
}

#[tokio::test]
async fn an_unjustified_customer_is_not_proposed() {
    let w = world(Provider::Anthropic).await;
    let message = || inbox_message("ada@home-mail.test", "VPN", SIGNED);
    // A customer reference that was not in the list.
    w.answer(claude(triage(
        "open_ticket",
        Some("C9"),
        None,
        Some("Brightfield"),
    )));
    // Evidence that the message does not contain.
    w.answer(claude(triage(
        "open_ticket",
        Some("C1"),
        None,
        Some("Harbor Lane"),
    )));
    // No evidence at all.
    w.answer(claude(triage("open_ticket", Some("C1"), None, None)));
    // An id in place of a reference.
    w.answer(claude(triage(
        "open_ticket",
        Some(BRIGHTFIELD),
        None,
        Some("Brightfield"),
    )));
    // The right quote with the wrong reference: C2 is the help desk's own organization.
    w.answer(claude(triage(
        "open_ticket",
        Some("C2"),
        None,
        Some("Brightfield, accounting"),
    )));
    // A quote from the message that does not name the customer.
    w.answer(claude(triage(
        "open_ticket",
        Some("C1"),
        None,
        Some("connect to the VPN"),
    )));
    for _ in 0..6 {
        assert!(w.process("message.received", message()).await.is_empty());
    }
    assert_eq!(w.requests().len(), 6, "the model was asked each time");
    assert!(w.posted().is_empty());
}

#[tokio::test]
async fn unusable_answers_give_no_proposal() {
    let w = world(Provider::Anthropic).await;
    let message = || inbox_message("ada@home-mail.test", "VPN", SIGNED);
    w.answer(claude_text("I think this is Brightfield."));
    w.answer(claude(
        json!({"action": "teleport", "rationale": "Why not."}),
    ));
    w.answer(claude(json!({"verdict": "open"})));
    w.answer((
        200,
        json!({"type": "message", "stop_reason": "refusal",
        "stop_details": {"type": "refusal", "category": null, "explanation": null},
        "content": []}),
    ));
    w.answer((
        200,
        json!({"type": "message", "stop_reason": "max_tokens",
        "content": [{"type": "text", "text": "{\"action\": \"open_"}]}),
    ));
    for _ in 0..5 {
        assert!(w.process("message.received", message()).await.is_empty());
    }
    assert_eq!(w.requests().len(), 5, "the model was asked each time");
    assert!(w.posted().is_empty());

    // A model service that refuses the request is a failure to report, not a proposal.
    w.answer((
        400,
        json!({"type": "error", "error": {"type": "invalid_request_error",
        "message": "model: not found"}}),
    ));
    let failed = w
        .assistant
        .process(&envelope("message.received", message()))
        .await;
    assert!(
        matches!(failed, Err(hd_assistant::Failure::Permanent(why)) if why.contains("model: not found"))
    );
}

#[tokio::test]
async fn a_busy_model_is_asked_again() {
    let w = world(Provider::Anthropic).await;
    w.answer((
        529,
        json!({"type": "error", "error": {"type": "overloaded_error",
        "message": "Overloaded"}}),
    ));
    w.answer(claude(triage("discard", None, None, None)));
    let made = w
        .process(
            "message.received",
            inbox_message("news@offers.test", "Deals", "Buy."),
        )
        .await;
    assert_eq!(made.len(), 1);
    assert_eq!(w.requests().len(), 2);

    // Still busy after the retries: the event is worth trying again later.
    w.answer((529, json!({})));
    w.answer((503, json!({})));
    let failed = w
        .assistant
        .process(&envelope(
            "message.received",
            inbox_message("a@b.test", "x", "y"),
        ))
        .await;
    assert!(matches!(failed, Err(hd_assistant::Failure::Transient(_))));
}

// ----- tickets -----

fn ticket_answer(priority: Option<&str>, assets: &[&str], reply: Option<&str>) -> Value {
    json!({
        "priority": priority, "priority_rationale": "The requester cannot work.",
        "assets": assets, "assets_rationale": "The message names PC-0042.",
        "reply": reply, "reply_rationale": "Asks for the missing details."
    })
}

const DRAFT: &str = "Buongiorno Ada,\n\ngrazie per la segnalazione. Il PC si accende? Vede delle luci?\n\nCordiali saluti";

#[tokio::test]
async fn a_new_ticket_gets_priority_assets_and_a_draft_reply() {
    let w = world(Provider::Anthropic).await;
    w.core
        .lock()
        .unwrap()
        .tickets
        .insert(7, ticket_json(7, "open", true, &[], "requester"));
    w.answer(claude(ticket_answer(Some("high"), &["a1"], Some(DRAFT))));
    let made = w
        .process("ticket.opened", json!({"ticket_number": 7}))
        .await;
    let contents: Vec<_> = made.iter().map(|p| p.content.clone()).collect();
    assert_eq!(
        contents,
        [
            Content::Priority {
                priority: Priority::High
            },
            Content::Assets {
                asset_ids: vec![uuid(PC)]
            },
            Content::Reply { body: DRAFT.into() },
        ]
    );
    assert!(made.iter().all(|p| p.target == Target::Ticket(7)));
    assert_eq!(w.posted().len(), 3);

    let body = &w.requests()[0].1;
    let input = body["messages"][0]["content"].as_str().unwrap();
    assert!(input.contains("A1: computer \"PC-0042\""));
    assert!(input.contains("A2: printer \"Printer on the second floor\""));
    assert!(input.contains("Current priority: normal"));
    assert!(input.contains("schermo nero"));
    assert!(
        !input.contains("late with payments"),
        "internal notes stay out"
    );
    assert_eq!(
        body["output_config"]["format"]["schema"]["required"][0],
        "priority"
    );
}

#[tokio::test]
async fn a_message_on_a_ticket_leads_to_ticket_proposals() {
    let w = world(Provider::Anthropic).await;
    w.core
        .lock()
        .unwrap()
        .tickets
        .insert(8, ticket_json(8, "waiting", true, &[], "requester"));
    w.answer(claude(ticket_answer(Some("urgent"), &[], None)));
    let mut data = inbox_message("ada@brightfield.test", "Re: PC", "Now nobody can work.");
    data["ticket_number"] = json!(8);
    let made = w.process("message.received", data).await;
    assert_eq!(
        made.iter().map(|p| p.content.clone()).collect::<Vec<_>>(),
        [Content::Priority {
            priority: Priority::Urgent
        }]
    );
    assert_eq!(w.core.lock().unwrap().calls[0], "ticket 8");
}

#[tokio::test]
async fn only_valid_and_useful_parts_are_proposed() {
    let w = world(Provider::Anthropic).await;
    {
        let mut core = w.core.lock().unwrap();
        core.tickets
            .insert(9, ticket_json(9, "open", true, &[PC], "requester"));
        core.tickets
            .insert(10, ticket_json(10, "open", false, &[], "requester"));
        core.tickets
            .insert(11, ticket_json(11, "open", true, &[], "help desk"));
    }
    // The priority it already has, an asset already linked, and an asset that was not listed.
    w.answer(claude(ticket_answer(Some("normal"), &["A1"], None)));
    w.answer(claude(ticket_answer(None, &["A2", "A7"], None)));
    assert!(
        w.process("ticket.opened", json!({"ticket_number": 9}))
            .await
            .is_empty()
    );
    assert!(
        w.process("ticket.opened", json!({"ticket_number": 9}))
            .await
            .is_empty()
    );

    // No requester to reply to; or the help desk wrote last.
    w.answer(claude(ticket_answer(None, &[], Some(DRAFT))));
    w.answer(claude(ticket_answer(None, &[], Some(DRAFT))));
    assert!(
        w.process("ticket.opened", json!({"ticket_number": 10}))
            .await
            .is_empty()
    );
    assert!(
        w.process("ticket.opened", json!({"ticket_number": 11}))
            .await
            .is_empty()
    );

    // Bad JSON and a priority that does not exist.
    w.answer(claude_text("{\"priority\": \"high\", "));
    w.answer(claude(ticket_answer(Some("whenever"), &[], None)));
    assert!(
        w.process("ticket.opened", json!({"ticket_number": 9}))
            .await
            .is_empty()
    );
    assert!(
        w.process("ticket.opened", json!({"ticket_number": 9}))
            .await
            .is_empty()
    );
    assert!(w.posted().is_empty());
}

#[tokio::test]
async fn a_closed_ticket_is_left_alone_and_late_refusals_are_not_errors() {
    let w = world(Provider::Anthropic).await;
    {
        let mut core = w.core.lock().unwrap();
        core.tickets
            .insert(12, ticket_json(12, "closed", true, &[], "requester"));
        core.tickets
            .insert(13, ticket_json(13, "open", true, &[], "requester"));
        core.refuse_on.push(13);
    }
    assert!(
        w.process("ticket.opened", json!({"ticket_number": 12}))
            .await
            .is_empty()
    );
    assert!(w.requests().is_empty(), "the model is not asked");

    // Closed between reading and proposing: the core refuses with 409.
    w.answer(claude(ticket_answer(Some("high"), &[], None)));
    assert!(
        w.process("ticket.opened", json!({"ticket_number": 13}))
            .await
            .is_empty()
    );
}

// ----- a local model -----

#[tokio::test]
async fn an_openai_compatible_server_works_the_same() {
    let w = world(Provider::OpenAiCompatible).await;
    let fenced = format!(
        "```json\n{}\n```",
        triage(
            "open_ticket",
            Some("C1"),
            None,
            Some("Brightfield, accounting")
        )
    );
    w.answer((
        200,
        json!({
            "id": "chatcmpl-test", "object": "chat.completion",
            "choices": [{"index": 0, "finish_reason": "stop",
                         "message": {"role": "assistant", "content": fenced}}]
        }),
    ));
    let made = w
        .process(
            "message.received",
            inbox_message("ada@home-mail.test", "VPN", SIGNED),
        )
        .await;
    assert_eq!(
        made[0].content,
        Content::Triage(Triage::OpenTicket {
            customer_id: uuid(BRIGHTFIELD)
        })
    );
    let (headers, body) = &w.requests()[0];
    assert_eq!(headers["authorization"], "Bearer model-test-key");
    assert_eq!(body["model"], "a-local-model");
    assert_eq!(body["response_format"]["type"], "json_object");
    assert!(
        body["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("JSON Schema")
    );

    w.answer((
        200,
        json!({"choices": [{"finish_reason": "stop",
        "message": {"content": "Sorry, I cannot help with that."}}]}),
    ));
    assert!(
        w.process(
            "message.received",
            inbox_message("ada@home-mail.test", "VPN", SIGNED)
        )
        .await
        .is_empty()
    );
}

// ----- the endpoint -----

#[tokio::test]
async fn the_endpoint_queues_each_verified_event_once() {
    const SECRET: &str = "event-secret-for-tests";
    let (sender, mut receiver) = server::queue();
    let url = start(server::events(Verifier::new(SECRET), sender)).await;
    let http = hd_addon_sdk::http_client(Duration::from_secs(5)).unwrap();
    let event = envelope("ticket.opened", json!({"ticket_number": 7}));
    let body = serde_json::to_vec(&event).unwrap();
    let deliver = |secret: &'static str| {
        let http = http.clone();
        let url = url.clone();
        let body = body.clone();
        async move {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
            http.post(format!("{url}/events"))
                .header("content-type", "application/json")
                .header(HEADER_TIMESTAMP, now.to_string())
                .header(HEADER_SIGNATURE, signature::sign(secret, now, &body))
                .body(body)
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        }
    };
    assert_eq!(deliver(SECRET).await, 204);
    assert_eq!(deliver(SECRET).await, 204, "a retry is acknowledged");
    assert_eq!(deliver("not the secret").await, 401);

    assert_eq!(receiver.recv().await.unwrap(), event);
    assert!(receiver.try_recv().is_err(), "queued once");
}
