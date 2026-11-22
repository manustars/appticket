//! From an event to proposals: gather what the core knows, ask the model, keep only what is
//! valid and useful, and propose it.

use std::collections::HashSet;
use std::fmt;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use hd_addon_sdk::api::{
    Asset, Customer, Direction, Priority, Service, Ticket, TicketState, TicketSummary,
    TimelineEntry,
};
use hd_addon_sdk::{Client, Content, Envelope, Event, MessageReceived, Rationale, Target, Triage};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use crate::model::{Model, ModelError, Task};
use crate::prompts;

/// The longest rationale shown to an agent, per language; a longer one is cut.
const MAX_RATIONALE_CHARS: usize = 300;
/// How long the agents' languages from `whoami` are trusted before asking again.
const LANGUAGES_TTL: Duration = Duration::from_secs(10 * 60);
/// The longest draft reply proposed; a longer one is dropped as unlikely to be a short reply.
const MAX_REPLY_CHARS: usize = 4_000;

/// A proposal the Assistant made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposed {
    pub target: Target,
    pub content: Content,
    /// One sentence per language of the agents; English is always there.
    pub rationale: Rationale,
}

/// Why an event could not be handled.
#[derive(Debug)]
pub enum Failure {
    /// The core or the model is unavailable for now: worth trying again.
    Transient(String),
    /// Trying again will not help.
    Permanent(String),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Transient(why) | Failure::Permanent(why) => f.write_str(why),
        }
    }
}

impl From<hd_addon_sdk::Error> for Failure {
    fn from(e: hd_addon_sdk::Error) -> Self {
        if e.is_transient() {
            Failure::Transient(e.to_string())
        } else {
            Failure::Permanent(e.to_string())
        }
    }
}

pub struct Assistant {
    core: Client,
    model: Model,
    languages: RwLock<Languages>,
}

/// The languages of the agents' UI, as the core last listed them.
struct Languages {
    tags: Vec<String>,
    fetched: Option<Instant>,
}

/// The languages to write rationales in: valid tags from the core, each once, English first and
/// always present.
fn usable_languages(listed: &[String]) -> Vec<String> {
    let mut tags = vec!["en".to_owned()];
    for tag in listed {
        let tag = tag.trim();
        let valid = (2..=8).contains(&tag.len()) && tag.chars().all(|c| c.is_ascii_lowercase());
        if valid && !tags.iter().any(|t| t == tag) {
            tags.push(tag.to_owned());
        }
    }
    tags
}

/// The model's answer on an inbox message.
#[derive(Debug, Deserialize)]
struct TriageAnswer {
    action: String,
    #[serde(default)]
    customer: Option<String>,
    #[serde(default)]
    ticket_number: Option<i64>,
    #[serde(default)]
    evidence: Option<String>,
    #[serde(default)]
    rationale: Value,
}

/// The model's answer on a ticket.
#[derive(Debug, Deserialize)]
struct TicketAnswer {
    #[serde(default)]
    priority: Option<Priority>,
    #[serde(default)]
    priority_rationale: Value,
    #[serde(default)]
    assets: Vec<String>,
    #[serde(default)]
    assets_rationale: Value,
    #[serde(default)]
    services: Vec<String>,
    #[serde(default)]
    services_rationale: Value,
    #[serde(default)]
    reply: Option<String>,
    #[serde(default)]
    reply_rationale: Value,
}

impl Assistant {
    pub fn new(core: Client, model: Model) -> Assistant {
        Assistant {
            core,
            model,
            languages: RwLock::new(Languages {
                tags: usable_languages(&[]),
                fetched: None,
            }),
        }
    }

    /// The languages rationales are written in.
    pub fn languages(&self) -> Vec<String> {
        self.languages.read().unwrap().tags.clone()
    }

    /// Asks the core for the agents' languages (`whoami`); on failure the previous ones stay.
    pub async fn refresh_languages(&self) -> Result<Vec<String>, hd_addon_sdk::Error> {
        let me = self.core.whoami().await?;
        let tags = usable_languages(&me.languages);
        *self.languages.write().unwrap() = Languages {
            tags: tags.clone(),
            fetched: Some(Instant::now()),
        };
        Ok(tags)
    }

    /// The languages, asked of the core again when never fetched or older than
    /// [`LANGUAGES_TTL`].
    async fn current_languages(&self) -> Vec<String> {
        let stale = self
            .languages
            .read()
            .unwrap()
            .fetched
            .is_none_or(|at| at.elapsed() > LANGUAGES_TTL);
        if stale && let Err(e) = self.refresh_languages().await {
            tracing::warn!(error = %e, "could not read the agents' languages; keeping the previous ones");
        }
        self.languages()
    }

    /// Handles one verified event and returns the proposals the core accepted.
    pub async fn process(&self, envelope: &Envelope) -> Result<Vec<Proposed>, Failure> {
        let event = envelope
            .event()
            .map_err(|e| Failure::Permanent(format!("the event's data is malformed: {e}")))?;
        let candidates = match event {
            Event::MessageReceived(m) => match m.ticket_number {
                None => self.triage(&m).await?,
                Some(number) => self.on_ticket(number).await?,
            },
            Event::TicketOpened(opened) => self.on_ticket(opened.ticket_number).await?,
            Event::Other { kind, .. } => {
                tracing::debug!(%kind, "ignoring an event kind the Assistant does not handle");
                Vec::new()
            }
        };
        let mut made = Vec::new();
        for p in candidates {
            match self.core.propose(p.target, &p.content, &p.rationale).await {
                Ok(_) => made.push(p),
                // The situation changed meanwhile: the message left the inbox, the ticket was
                // closed, an asset was retired. The proposal no longer applies.
                Err(e) if matches!(e.status(), Some(404 | 409)) => {
                    tracing::info!(kind = p.content.kind(), error = %e, "proposal no longer applies");
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(made)
    }

    async fn ask(&self, task: Task<'_>) -> Result<Option<Value>, Failure> {
        match self.model.answer(&task).await {
            Ok(answer) => Ok(Some(answer)),
            Err(ModelError::Unavailable(why)) => Err(Failure::Transient(why)),
            Err(ModelError::Rejected(why)) => Err(Failure::Permanent(why)),
            Err(other) => {
                tracing::warn!(task = task.name, error = %other, "no proposal: the model gave no usable answer");
                Ok(None)
            }
        }
    }

    async fn triage(&self, message: &MessageReceived) -> Result<Vec<Proposed>, Failure> {
        let customers = self.core.customers().await?;
        let tickets = self
            .core
            .open_tickets_of_requester(&message.from_address)
            .await?;
        let languages = self.current_languages().await;
        let system = prompts::system(prompts::TRIAGE_SYSTEM, &languages);
        let task = Task {
            name: "triage",
            system: &system,
            input: prompts::triage_input(message, &customers, &tickets),
            schema: prompts::triage_schema(&languages),
        };
        let Some(answer) = self.ask(task).await? else {
            return Ok(Vec::new());
        };
        Ok(
            decide_triage(answer, message, &customers, &tickets, &languages)
                .into_iter()
                .collect(),
        )
    }

    async fn on_ticket(&self, number: i64) -> Result<Vec<Proposed>, Failure> {
        let ticket = self.core.ticket(number).await?;
        if ticket.state == TicketState::Closed {
            return Ok(Vec::new());
        }
        let assets = self.core.customer_assets(ticket.customer.id).await?;
        let services = self.core.customer_services(ticket.customer.id).await?;
        let languages = self.current_languages().await;
        let system = prompts::system(prompts::TICKET_SYSTEM, &languages);
        let task = Task {
            name: "ticket",
            system: &system,
            input: prompts::ticket_input(&ticket, &assets, &services),
            schema: prompts::ticket_schema(&languages),
        };
        let Some(answer) = self.ask(task).await? else {
            return Ok(Vec::new());
        };
        Ok(decide_ticket(
            answer, &ticket, &assets, &services, &languages,
        ))
    }
}

fn sentence(text: &str) -> String {
    let text = text.trim();
    match text.char_indices().nth(MAX_RATIONALE_CHARS) {
        None => text.to_owned(),
        Some((cut, _)) => format!("{}…", text[..cut].trim_end()),
    }
}

/// The sentences of a rationale in `languages`, dropping missing or empty ones and languages not
/// asked for. `None` without an English sentence: agents whose language is missing read the
/// English one, so every proposal needs it.
fn rationale(value: &Value, languages: &[String]) -> Option<Rationale> {
    let mut out = Rationale::new();
    if let Value::Object(map) = value {
        for language in languages {
            if let Some(text) = map.get(language).and_then(Value::as_str) {
                let text = sentence(text);
                if !text.is_empty() {
                    out.insert(language.clone(), text);
                }
            }
        }
    }
    if !out.contains_key("en") {
        return None;
    }
    let missing: Vec<_> = languages.iter().filter(|l| !out.contains_key(*l)).collect();
    if !missing.is_empty() {
        tracing::info!(
            ?missing,
            "a rationale lacks some languages; those agents read English"
        );
    }
    Some(out)
}

/// Lowercase words, so that quoted evidence matches the message whatever its spacing and case.
fn words(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Keeps a triage answer only when it is grounded in what the model was shown.
fn decide_triage(
    answer: Value,
    message: &MessageReceived,
    customers: &[Customer],
    tickets: &[TicketSummary],
    languages: &[String],
) -> Option<Proposed> {
    let answer: TriageAnswer = match serde_json::from_value(answer) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(error = %e, "no triage proposal: the answer has the wrong shape");
            return None;
        }
    };
    if answer.action == "none" {
        return None;
    }
    let Some(why) = rationale(&answer.rationale, languages) else {
        tracing::warn!("no triage proposal: the answer gives no English rationale");
        return None;
    };
    let triage = match answer.action.as_str() {
        "none" => return None,
        "discard" => Triage::Discard,
        "attach" => {
            let number = answer.ticket_number?;
            if !tickets.iter().any(|t| t.number == number) {
                tracing::warn!(
                    number,
                    "no triage proposal: the ticket is not one of the sender's"
                );
                return None;
            }
            Triage::Attach {
                ticket_number: number,
            }
        }
        "open_ticket" => {
            let reference = answer.customer.as_deref()?.trim().to_ascii_uppercase();
            let Some(customer) = (0..customers.len())
                .find(|&i| prompts::customer_ref(i) == reference)
                .map(|i| &customers[i])
            else {
                tracing::warn!(%reference, "no triage proposal: unknown customer reference");
                return None;
            };
            let evidence = words(answer.evidence.as_deref().unwrap_or_default());
            let shown = words(&format!(
                "{} {} {} {}",
                message.from_name.as_deref().unwrap_or_default(),
                message.from_address,
                message.subject,
                message.body_text
            ));
            if evidence.is_empty() || !shown.contains(&evidence) {
                tracing::warn!(
                    "no triage proposal: the evidence for the customer is not in the message"
                );
                return None;
            }
            // The quote must name this customer, so that a slip of reference (C2 for C1) with
            // a quote about another customer gives no proposal.
            let names_it = std::iter::once(&customer.name)
                .chain(&customer.domains)
                .map(|n| words(n))
                .any(|n| !n.is_empty() && evidence.contains(&n));
            if !names_it {
                tracing::warn!(
                    %reference,
                    "no triage proposal: the evidence does not name the chosen customer"
                );
                return None;
            }
            Triage::OpenTicket {
                customer_id: customer.id,
            }
        }
        other => {
            tracing::warn!(action = other, "no triage proposal: unknown action");
            return None;
        }
    };
    Some(Proposed {
        target: Target::Message(message.message_id),
        content: Content::Triage(triage),
        rationale: why,
    })
}

/// The ids the model picked by reference from `ids` (shown as `reference(0)`, `reference(1)`…),
/// each once and without those already `linked`. `None` when any reference was not shown: the
/// model may have confused the list, so none of its picks is trusted.
fn picked(
    references: &[String],
    ids: &[Uuid],
    reference: fn(usize) -> String,
    linked: &HashSet<Uuid>,
) -> Option<Vec<Uuid>> {
    let mut out = Vec::new();
    for given in references {
        let given = given.trim().to_ascii_uppercase();
        let i = (0..ids.len()).find(|&i| reference(i) == given)?;
        if !linked.contains(&ids[i]) && !out.contains(&ids[i]) {
            out.push(ids[i]);
        }
    }
    Some(out)
}

/// Keeps the parts of a ticket answer that are valid and would change something.
fn decide_ticket(
    answer: Value,
    ticket: &Ticket,
    assets: &[Asset],
    services: &[Service],
    languages: &[String],
) -> Vec<Proposed> {
    let answer: TicketAnswer = match serde_json::from_value(answer) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(error = %e, "no ticket proposals: the answer has the wrong shape");
            return Vec::new();
        }
    };
    let target = Target::Ticket(ticket.number);
    let mut out = Vec::new();
    // Each proposal needs its own English rationale.
    let mut push = |content: Content, why: &Value| match rationale(why, languages) {
        Some(rationale) => out.push(Proposed {
            target,
            content,
            rationale,
        }),
        None => tracing::warn!(
            kind = content.kind(),
            "no proposal of this kind: the answer gives no English rationale"
        ),
    };

    if let Some(priority) = answer.priority
        && priority != ticket.priority
    {
        push(Content::Priority { priority }, &answer.priority_rationale);
    }

    let asset_ids: Vec<Uuid> = assets.iter().map(|a| a.id).collect();
    let linked: HashSet<Uuid> = ticket.assets.iter().map(|a| a.id).collect();
    match picked(&answer.assets, &asset_ids, prompts::asset_ref, &linked) {
        None => tracing::warn!("no assets proposal: the answer names an asset that was not listed"),
        Some(ids) if !ids.is_empty() => {
            push(Content::Assets { asset_ids: ids }, &answer.assets_rationale)
        }
        Some(_) => {}
    }

    let service_ids: Vec<Uuid> = services.iter().map(|s| s.id).collect();
    let linked: HashSet<Uuid> = ticket.services.iter().map(|s| s.id).collect();
    match picked(
        &answer.services,
        &service_ids,
        prompts::service_ref,
        &linked,
    ) {
        None => {
            tracing::warn!("no services proposal: the answer names a service that was not listed")
        }
        Some(ids) if !ids.is_empty() => push(
            Content::Services { service_ids: ids },
            &answer.services_rationale,
        ),
        Some(_) => {}
    }

    // A draft only answers a requester who wrote last; an agent may already have replied.
    let requester_wrote_last = ticket
        .timeline
        .iter()
        .rev()
        .find_map(|e| match e {
            TimelineEntry::Message(m) => Some(m.direction == Direction::Inbound),
            _ => None,
        })
        .unwrap_or(false);
    if let Some(body) = answer.reply.as_deref().map(str::trim)
        && !body.is_empty()
        && ticket.requester.is_some()
        && requester_wrote_last
    {
        if body.chars().count() > MAX_REPLY_CHARS {
            tracing::warn!("no reply proposal: the draft is too long");
        } else {
            push(
                Content::Reply {
                    body: letter_layout(body),
                },
                &answer.reply_rationale,
            );
        }
    }
    out
}

/// Puts a draft in the layout of a letter, whatever the model did with blank lines: the greeting
/// on its own line, a blank line, the paragraphs separated by one blank line, a blank line, and the
/// closing formula. The greeting is a first line ending with a comma; the closing is a short last
/// line without final punctuation (such as "Cordiali saluti"). Text without them only gets its
/// blank lines tidied.
pub fn letter_layout(draft: &str) -> String {
    // Paragraphs as the model wrote them: runs of non-empty lines.
    let mut paragraphs: Vec<Vec<&str>> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in draft.lines().map(str::trim_end) {
        if line.trim().is_empty() {
            if !current.is_empty() {
                paragraphs.push(std::mem::take(&mut current));
            }
        } else {
            current.push(line);
        }
    }
    if !current.is_empty() {
        paragraphs.push(current);
    }

    // Split a greeting off the first paragraph and a closing off the last.
    let greeting = match paragraphs.first() {
        Some(first) if first.len() > 1 && first[0].trim_end().ends_with(',') => {
            Some(paragraphs[0].remove(0))
        }
        _ => None,
    };
    let is_closing = |line: &str| {
        let line = line.trim();
        line.split_whitespace().count() <= 4
            && !line.ends_with(['.', '?', '!', ':', ';'])
            && line.chars().next().is_some_and(char::is_uppercase)
    };
    let closing = match paragraphs.last() {
        Some(last) if last.len() > 1 && is_closing(last[last.len() - 1]) => {
            let last = paragraphs.len() - 1;
            paragraphs[last].pop()
        }
        _ => None,
    };
    paragraphs.retain(|p| !p.is_empty());

    let mut blocks: Vec<String> = Vec::new();
    blocks.extend(greeting.map(str::to_owned));
    blocks.extend(paragraphs.iter().map(|p| p.join("\n")));
    blocks.extend(closing.map(str::to_owned));
    blocks.join("\n\n")
}

#[cfg(test)]
mod layout_tests {
    use super::letter_layout;

    #[test]
    fn a_letter_without_blank_lines_gets_them() {
        let draft = "Buongiorno Nora,\ngrazie per la segnalazione.\nCi indichi l'ultimo messaggio ricevuto.\nCordiali saluti";
        assert_eq!(
            letter_layout(draft),
            "Buongiorno Nora,\n\ngrazie per la segnalazione.\nCi indichi l'ultimo messaggio ricevuto.\n\nCordiali saluti"
        );
    }

    #[test]
    fn a_well_laid_out_letter_is_unchanged() {
        let draft = "Buongiorno Ada,\n\ngrazie per la segnalazione.\n\nVerificheremo il PC-0042.\n\nCordiali saluti";
        assert_eq!(letter_layout(draft), draft);
    }

    #[test]
    fn extra_blank_lines_and_spaces_are_tidied() {
        let draft = "Buongiorno Ivo,   \n\n\n\ngrazie.  \n\n\nCordiali saluti\n\n";
        assert_eq!(
            letter_layout(draft),
            "Buongiorno Ivo,\n\ngrazie.\n\nCordiali saluti"
        );
    }

    #[test]
    fn text_without_greeting_or_closing_is_only_tidied() {
        let draft = "Grazie per la segnalazione. Verificheremo.\n\n\nCi indichi l'orario.";
        assert_eq!(
            letter_layout(draft),
            "Grazie per la segnalazione. Verificheremo.\n\nCi indichi l'orario."
        );
    }

    #[test]
    fn a_last_sentence_is_not_mistaken_for_a_closing() {
        let draft = "Buongiorno Ada,\ngrazie.\nCi indichi se la ventola gira?";
        assert_eq!(
            letter_layout(draft),
            "Buongiorno Ada,\n\ngrazie.\nCi indichi se la ventola gira?"
        );
    }
}
