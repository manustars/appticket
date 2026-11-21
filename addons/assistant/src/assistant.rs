//! From an event to proposals: gather what the core knows, ask the model, keep only what is
//! valid and useful, and propose it.

use std::collections::HashSet;
use std::fmt;

use hd_addon_sdk::api::{
    Asset, Customer, Direction, Priority, Ticket, TicketState, TicketSummary, TimelineEntry,
};
use hd_addon_sdk::{Client, Content, Envelope, Event, MessageReceived, Target, Triage};
use serde::Deserialize;
use serde_json::Value;

use crate::model::{Model, ModelError, Task};
use crate::prompts;

/// The longest rationale shown to an agent; a longer one is cut.
const MAX_RATIONALE_CHARS: usize = 300;
/// The longest draft reply proposed; a longer one is dropped as unlikely to be a short reply.
const MAX_REPLY_CHARS: usize = 4_000;

/// A proposal the Assistant made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposed {
    pub target: Target,
    pub content: Content,
    pub rationale: String,
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
    rationale: String,
}

/// The model's answer on a ticket.
#[derive(Debug, Deserialize)]
struct TicketAnswer {
    #[serde(default)]
    priority: Option<Priority>,
    #[serde(default)]
    priority_rationale: String,
    #[serde(default)]
    assets: Vec<String>,
    #[serde(default)]
    assets_rationale: String,
    #[serde(default)]
    reply: Option<String>,
    #[serde(default)]
    reply_rationale: String,
}

impl Assistant {
    pub fn new(core: Client, model: Model) -> Assistant {
        Assistant { core, model }
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
        let task = Task {
            name: "triage",
            system: prompts::TRIAGE_SYSTEM,
            input: prompts::triage_input(message, &customers, &tickets),
            schema: prompts::triage_schema(),
        };
        let Some(answer) = self.ask(task).await? else {
            return Ok(Vec::new());
        };
        Ok(decide_triage(answer, message, &customers, &tickets)
            .into_iter()
            .collect())
    }

    async fn on_ticket(&self, number: i64) -> Result<Vec<Proposed>, Failure> {
        let ticket = self.core.ticket(number).await?;
        if ticket.state == TicketState::Closed {
            return Ok(Vec::new());
        }
        let assets = self.core.customer_assets(ticket.customer.id).await?;
        let task = Task {
            name: "ticket",
            system: prompts::TICKET_SYSTEM,
            input: prompts::ticket_input(&ticket, &assets),
            schema: prompts::ticket_schema(),
        };
        let Some(answer) = self.ask(task).await? else {
            return Ok(Vec::new());
        };
        Ok(decide_ticket(answer, &ticket, &assets))
    }
}

fn rationale(text: &str) -> String {
    let text = text.trim();
    match text.char_indices().nth(MAX_RATIONALE_CHARS) {
        None => text.to_owned(),
        Some((cut, _)) => format!("{}…", text[..cut].trim_end()),
    }
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
) -> Option<Proposed> {
    let answer: TriageAnswer = match serde_json::from_value(answer) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(error = %e, "no triage proposal: the answer has the wrong shape");
            return None;
        }
    };
    let why = rationale(&answer.rationale);
    if why.is_empty() && answer.action != "none" {
        tracing::warn!("no triage proposal: the answer gives no rationale");
        return None;
    }
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

/// Keeps the parts of a ticket answer that are valid and would change something.
fn decide_ticket(answer: Value, ticket: &Ticket, assets: &[Asset]) -> Vec<Proposed> {
    let answer: TicketAnswer = match serde_json::from_value(answer) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(error = %e, "no ticket proposals: the answer has the wrong shape");
            return Vec::new();
        }
    };
    let target = Target::Ticket(ticket.number);
    let mut out = Vec::new();

    if let Some(priority) = answer.priority
        && priority != ticket.priority
    {
        out.push(Proposed {
            target,
            content: Content::Priority { priority },
            rationale: rationale(&answer.priority_rationale),
        });
    }

    let mut ids = Vec::new();
    let mut unknown = false;
    for reference in &answer.assets {
        let reference = reference.trim().to_ascii_uppercase();
        match (0..assets.len()).find(|&i| prompts::asset_ref(i) == reference) {
            Some(i) => ids.push(assets[i].id),
            None => unknown = true,
        }
    }
    let linked: HashSet<_> = ticket.assets.iter().map(|a| a.id).collect();
    let mut seen = HashSet::new();
    ids.retain(|id| !linked.contains(id) && seen.insert(*id));
    if unknown {
        tracing::warn!("no assets proposal: the answer names an asset that was not listed");
    } else if !ids.is_empty() {
        out.push(Proposed {
            target,
            content: Content::Assets { asset_ids: ids },
            rationale: rationale(&answer.assets_rationale),
        });
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
            out.push(Proposed {
                target,
                content: Content::Reply {
                    body: body.to_owned(),
                },
                rationale: rationale(&answer.reply_rationale),
            });
        }
    }
    out
}
