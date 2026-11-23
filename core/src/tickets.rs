//! What agents do on a ticket: read it, write notes, reply to the requester, assign it and move
//! it between states. Rules are in docs/domain-rules.md ("Tickets").

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::{Error, Result, rule};
use crate::outbound::Sender;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum TicketState {
    /// Someone has to act on it.
    Open,
    /// The help desk awaits the requester.
    Waiting,
    /// An agent considers the work done; a reply from the requester reopens it.
    Resolved,
    /// Finished for good.
    Closed,
}

/// How soon a ticket needs attention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum Priority {
    Low,
    Normal,
    High,
    Urgent,
}

pub async fn set_priority(db: &PgPool, number: i64, priority: Priority) -> Result<()> {
    let mut tx = db.begin().await?;
    let (ticket_id, state) = lock(&mut tx, number).await?;
    not_closed(state)?;
    sqlx::query("UPDATE tickets SET priority = $2, updated_at = now() WHERE id = $1")
        .bind(ticket_id)
        .bind(priority)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Records that a ticket was opened, for the add-ons that follow it.
pub async fn emit_opened(tx: &mut Transaction<'_, Postgres>, number: i64) -> Result<()> {
    crate::addons::emit(
        tx,
        crate::addons::TICKET_OPENED,
        serde_json::json!({ "ticket_number": number }),
    )
    .await
}

/// The ticket's id and state, locked for the rest of the transaction.
async fn lock(tx: &mut Transaction<'_, Postgres>, number: i64) -> Result<(Uuid, TicketState)> {
    sqlx::query_as("SELECT id, state FROM tickets WHERE number = $1 FOR UPDATE")
        .bind(number)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(Error::NotFound("ticket"))
}

fn not_closed(state: TicketState) -> Result<()> {
    if state == TicketState::Closed {
        return rule("the ticket is closed; a closed ticket cannot change");
    }
    Ok(())
}

pub async fn add_note(db: &PgPool, number: i64, author: Uuid, body: &str) -> Result<Uuid> {
    let body = body.trim();
    if body.is_empty() {
        return rule("a note cannot be empty");
    }
    let mut tx = db.begin().await?;
    let (ticket_id, _) = lock(&mut tx, number).await?;
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO notes (id, ticket_id, author_id, body) VALUES ($1, $2, $3, $4)")
        .bind(id)
        .bind(ticket_id)
        .bind(author)
        .bind(body)
        .execute(&mut *tx)
        .await?;
    crate::drafts::clear(&mut tx, ticket_id, author).await?;
    touch(&mut tx, ticket_id).await?;
    tx.commit().await?;
    Ok(id)
}

pub async fn set_state(db: &PgPool, number: i64, state: TicketState) -> Result<()> {
    let mut tx = db.begin().await?;
    let (ticket_id, current) = lock(&mut tx, number).await?;
    not_closed(current)?;
    sqlx::query("UPDATE tickets SET state = $2, updated_at = now() WHERE id = $1")
        .bind(ticket_id)
        .bind(state)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Makes `agent` the ticket's assignee, or leaves it unassigned when `None`.
pub async fn assign(db: &PgPool, number: i64, agent: Option<Uuid>) -> Result<()> {
    let mut tx = db.begin().await?;
    let (ticket_id, state) = lock(&mut tx, number).await?;
    not_closed(state)?;
    if let Some(agent) = agent {
        let active: Option<bool> =
            sqlx::query_scalar("SELECT deactivated_at IS NULL FROM agents WHERE id = $1")
                .bind(agent)
                .fetch_optional(&mut *tx)
                .await?;
        match active {
            None => return Err(Error::NotFound("agent")),
            Some(false) => {
                return rule("the agent is deactivated: tickets cannot be assigned to them");
            }
            Some(true) => {}
        }
    }
    sqlx::query("UPDATE tickets SET assignee_id = $2, updated_at = now() WHERE id = $1")
        .bind(ticket_id)
        .bind(agent)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Queues a reply on the ticket and moves the ticket to `then`. It is sent by the outbox and
/// threads with the conversation, so that the answer comes back here.
///
/// On a ticket with a requester it goes to them, at the address they last wrote from on the
/// ticket; on a ticket they have not written on, `to` is the address the agent picked (see
/// [`crate::requesters::Recipient`]). On a ticket without a requester it goes to every key
/// requester of the ticket's customer, each at their contact address in a message of their own,
/// and the ticket stays without a requester; with none, the reply is refused. Returns the queued
/// messages.
pub async fn reply(
    db: &PgPool,
    sender: &Sender,
    number: i64,
    author: Uuid,
    body: &str,
    to: Option<&str>,
    then: TicketState,
) -> Result<Vec<Uuid>> {
    let body = body.trim();
    if body.is_empty() {
        return rule("a reply cannot be empty");
    }
    let mut tx = db.begin().await?;
    // Read before any lock: the key requesters are locked before the ticket.
    let read: Option<(Uuid, Option<Uuid>, TicketState)> =
        sqlx::query_as("SELECT customer_id, requester_id, state FROM tickets WHERE number = $1")
            .bind(number)
            .fetch_optional(&mut *tx)
            .await?;
    let (customer, requester, state) = read.ok_or(Error::NotFound("ticket"))?;
    not_closed(state)?;
    let keys = match requester {
        Some(_) => None,
        None => {
            if to.is_some_and(|to| !to.trim().is_empty()) {
                return rule(
                    "the ticket has no requester: the reply goes to every key requester of its customer, and no address is picked",
                );
            }
            Some(crate::key_requesters::to_reply_to(&mut tx, customer).await?)
        }
    };
    let (ticket_id, state) = lock(&mut tx, number).await?;
    not_closed(state)?;
    let now: (Uuid, Option<Uuid>) =
        sqlx::query_as("SELECT customer_id, requester_id FROM tickets WHERE id = $1")
            .bind(ticket_id)
            .fetch_one(&mut *tx)
            .await?;
    if now != (customer, requester) {
        return rule("the ticket has just changed; reload and try again");
    }
    let recipients = match keys {
        Some(keys) => keys.into_iter().map(|key| key.contact_address).collect(),
        None => {
            let recipient = crate::requesters::recipient(&mut tx, ticket_id)
                .await?
                .ok_or(Error::NotFound("requester"))?;
            vec![recipient.choose(to)?]
        }
    };
    let queued = send(&mut tx, sender, ticket_id, author, body, &recipients, then).await?;
    tx.commit().await?;
    Ok(queued)
}

/// Queues `body` on the ticket to each of `recipients`, in a message of their own threaded with
/// the conversation, moves the ticket to `then` and clears the author's draft there. Returns the
/// queued messages.
pub(crate) async fn send(
    tx: &mut Transaction<'_, Postgres>,
    sender: &Sender,
    ticket: Uuid,
    author: Uuid,
    body: &str,
    recipients: &[String],
    then: TicketState,
) -> Result<Vec<Uuid>> {
    let outgoing = Outgoing::on(tx, ticket, author, body).await?;
    let mut queued = Vec::new();
    for to in recipients {
        queued.push(outgoing.queue(tx, sender, to).await?);
    }
    sqlx::query("UPDATE tickets SET state = $2, updated_at = now() WHERE id = $1")
        .bind(ticket)
        .bind(then)
        .execute(&mut **tx)
        .await?;
    crate::drafts::clear(tx, ticket, author).await?;
    Ok(queued)
}

/// A message an agent sends on a ticket, before it has a recipient: it threads with the
/// conversation so far, answering the last message received (else the last one).
pub(crate) struct Outgoing<'a> {
    ticket: Uuid,
    author: Uuid,
    body: &'a str,
    subject: String,
    in_reply_to: Option<String>,
    references: Vec<String>,
}

impl<'a> Outgoing<'a> {
    pub(crate) async fn on(
        tx: &mut Transaction<'_, Postgres>,
        ticket: Uuid,
        author: Uuid,
        body: &'a str,
    ) -> Result<Outgoing<'a>> {
        let (title, number): (String, i64) =
            sqlx::query_as("SELECT title, number FROM tickets WHERE id = $1")
                .bind(ticket)
                .fetch_one(&mut **tx)
                .await?;
        // The conversation so far, newest last.
        let mut messages: Vec<(String, String)> = sqlx::query_as(
            "SELECT rfc_message_id, direction FROM messages
             WHERE ticket_id = $1 AND rfc_message_id IS NOT NULL
             ORDER BY created_at DESC LIMIT 20",
        )
        .bind(ticket)
        .fetch_all(&mut **tx)
        .await?;
        messages.reverse();
        let in_reply_to = messages
            .iter()
            .rev()
            .find(|(_, direction)| direction == "inbound")
            .or(messages.last())
            .map(|(id, _)| id.clone());
        Ok(Outgoing {
            ticket,
            author,
            body,
            subject: reply_subject(&title, number),
            in_reply_to,
            references: messages.into_iter().map(|(id, _)| id).collect(),
        })
    }

    /// Puts the message to `to` in the outbox, as a message of its own; returns its id. The
    /// database refuses a recipient who is neither the ticket's requester nor a key requester of
    /// its customer at their contact address; one who just stopped being either is a change the
    /// agent reloads for.
    pub(crate) async fn queue(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        sender: &Sender,
        to: &str,
    ) -> Result<Uuid> {
        let id = Uuid::new_v4();
        let queued = sqlx::query(
            "INSERT INTO messages (id, ticket_id, direction, rfc_message_id, in_reply_to,
                                   \"references\", from_address, from_name, to_address, author_id,
                                   subject, body_text)
             VALUES ($1, $2, 'outbound', $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(id)
        .bind(self.ticket)
        .bind(sender.new_message_id())
        .bind(&self.in_reply_to)
        .bind(&self.references)
        .bind(&sender.address)
        .bind(&sender.name)
        .bind(to)
        .bind(self.author)
        .bind(&self.subject)
        .bind(self.body)
        .execute(&mut **tx)
        .await;
        match queued {
            // The check_violation the trigger raises on messages, and no other.
            Err(sqlx::Error::Database(e))
                if e.code().as_deref() == Some("23514")
                    && e.message().contains(
                        "a message goes to the ticket's requester or to a key requester",
                    ) =>
            {
                rule(format!(
                    "{to} is no longer the ticket's requester or a key requester of its customer; reload and try again"
                ))
            }
            Err(e) => Err(e.into()),
            Ok(_) => Ok(id),
        }
    }
}

/// Opens a ticket by hand, for example after a phone call (docs/domain-rules.md, "Incoming
/// messages"). With a requester's address, that requester is used or, if new, registered with
/// the customer; a departed requester, and an address that is a requester of another customer, are
/// refused, unless that requester departed: then the address passes to the new requester. The
/// optional note records what was said. Returns the ticket's number.
pub async fn open(
    db: &PgPool,
    customer: Uuid,
    title: &str,
    requester_email: Option<&str>,
    note: Option<&str>,
    author: Uuid,
) -> Result<i64> {
    let title = title.trim();
    if title.is_empty() {
        return rule("a ticket needs a title");
    }
    let mut tx = db.begin().await?;
    let offboarded: Option<bool> =
        sqlx::query_scalar("SELECT offboarded_at IS NOT NULL FROM customers WHERE id = $1")
            .bind(customer)
            .fetch_optional(&mut *tx)
            .await?;
    match offboarded {
        None => return Err(Error::NotFound("customer")),
        Some(true) => return rule("the customer is offboarded and gets no new tickets"),
        Some(false) => {}
    }
    let requester = match requester_email.map(str::trim).filter(|e| !e.is_empty()) {
        None => None,
        Some(email) => {
            let email = crate::requesters::clean_address(email)?;
            Some(crate::requesters::find_or_create(&mut tx, customer, &email, None).await?)
        }
    };
    let id = Uuid::new_v4();
    let number: i64 = sqlx::query_scalar(
        "INSERT INTO tickets (id, customer_id, requester_id, title) VALUES ($1, $2, $3, $4)
         RETURNING number",
    )
    .bind(id)
    .bind(customer)
    .bind(requester)
    .bind(title)
    .fetch_one(&mut *tx)
    .await?;
    emit_opened(&mut tx, number).await?;
    if let Some(note) = note.map(str::trim).filter(|n| !n.is_empty()) {
        sqlx::query("INSERT INTO notes (id, ticket_id, author_id, body) VALUES ($1, $2, $3, $4)")
            .bind(Uuid::new_v4())
            .bind(id)
            .bind(author)
            .bind(note)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(number)
}

/// What happens to the ticket's requester when the ticket moves to another customer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum RequesterMove {
    /// The requester was identified with the wrong customer: the requester moves too, and all of
    /// the requester's tickets with them.
    Move,
    /// The requester stays with the old customer and leaves this ticket.
    Remove,
}

/// What a move did.
#[derive(Debug, Serialize, ToSchema)]
pub struct Moved {
    /// How many tickets changed customer.
    pub moved: i64,
    /// How many asset and service links were removed because they did not fit the new customer.
    pub unlinked: i64,
}

/// Moves the ticket to another customer. A requester of the old customer either moves along,
/// taking all of their tickets (closed ones too, see [`crate::requesters::move_requester`]), or
/// leaves this ticket. Links that do not fit the new customer are removed.
pub async fn move_to_customer(
    db: &PgPool,
    number: i64,
    customer: Uuid,
    requester: RequesterMove,
) -> Result<Moved> {
    let mut tx = db.begin().await?;
    // Read before any lock: locks are taken customers first, then requesters, then key
    // requesters, then the assets the requester holds (against changes only), then tickets.
    let (ticket_id, state, current, requester_id): (Uuid, TicketState, Uuid, Option<Uuid>) =
        sqlx::query_as(
            "SELECT id, state, customer_id, requester_id FROM tickets WHERE number = $1",
        )
        .bind(number)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(Error::NotFound("ticket"))?;
    not_closed(state)?;
    if current == customer {
        return rule("the ticket already belongs to this customer");
    }
    // The requester who moves along, if any.
    let moving = requester_id.filter(|_| requester == RequesterMove::Move);
    if let Some(moving) = moving {
        crate::requesters::lock_for_move(&mut tx, moving, customer).await?;
    }
    let (_, state) = lock(&mut tx, number).await?;
    not_closed(state)?;
    let now: (Uuid, Option<Uuid>) =
        sqlx::query_as("SELECT customer_id, requester_id FROM tickets WHERE id = $1")
            .bind(ticket_id)
            .fetch_one(&mut *tx)
            .await?;
    if now != (current, requester_id) {
        return rule("the ticket has just changed; reload and try again");
    }
    let moved = match moving {
        Some(moving) => crate::requesters::move_requester(&mut tx, moving, customer).await?,
        None => {
            ensure_ticket_may_move_to(&mut tx, ticket_id, customer).await?;
            let unlinked = crate::catalog::unlink_for_move(&mut tx, &[ticket_id], customer).await?;
            sqlx::query(
                "UPDATE tickets SET customer_id = $2, requester_id = NULL, updated_at = now()
                 WHERE id = $1",
            )
            .bind(ticket_id)
            .bind(customer)
            .execute(&mut *tx)
            .await?;
            Moved { moved: 1, unlinked }
        }
    };
    tx.commit().await?;
    Ok(moved)
}

/// Guards a ticket moving alone: `customer` must exist and not be offboarded, and the ticket must
/// not be a parent ticket, which stays with the help desk's own organization.
async fn ensure_ticket_may_move_to(
    tx: &mut Transaction<'_, Postgres>,
    ticket: Uuid,
    customer: Uuid,
) -> Result<()> {
    let offboarded: Option<bool> =
        sqlx::query_scalar("SELECT offboarded_at IS NOT NULL FROM customers WHERE id = $1")
            .bind(customer)
            .fetch_optional(&mut **tx)
            .await?;
    match offboarded {
        None => return Err(Error::NotFound("customer")),
        Some(true) => return rule("the customer is offboarded and gets no tickets"),
        Some(false) => {}
    }
    let parent: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM tickets WHERE parent_id = $1)")
            .bind(ticket)
            .fetch_one(&mut **tx)
            .await?;
    if parent {
        return rule("a parent ticket stays with the help desk's own organization");
    }
    Ok(())
}

/// The sites each of the tickets numbered `numbers` is at, by number, each list by name.
pub async fn sites<'e>(
    db: impl sqlx::PgExecutor<'e>,
    numbers: &[i64],
) -> Result<HashMap<i64, Vec<crate::requesters::SiteRef>>> {
    let rows: Vec<(i64, Uuid, String)> = sqlx::query_as(
        "SELECT t.number, s.id, s.name
         FROM tickets t
         JOIN ticket_sites ts ON ts.ticket_id = t.id
         JOIN sites s ON s.id = ts.site_id
         WHERE t.number = ANY($1)
         ORDER BY t.number, lower(s.name), s.name, s.id",
    )
    .bind(numbers)
    .fetch_all(db)
    .await?;
    let mut sites: HashMap<i64, Vec<crate::requesters::SiteRef>> = HashMap::new();
    for (number, id, name) in rows {
        sites
            .entry(number)
            .or_default()
            .push(crate::requesters::SiteRef { id, name });
    }
    Ok(sites)
}

/// "Re: <title> [#<number>]", without stacking "Re:".
pub fn reply_subject(title: &str, number: i64) -> String {
    let bare = title
        .trim()
        .strip_prefix("Re:")
        .or_else(|| title.trim().strip_prefix("RE:"))
        .map(str::trim)
        .unwrap_or(title.trim());
    format!("Re: {bare} [#{number}]")
}

async fn touch(tx: &mut Transaction<'_, Postgres>, ticket_id: Uuid) -> Result<()> {
    sqlx::query("UPDATE tickets SET updated_at = now() WHERE id = $1")
        .bind(ticket_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// A ticket with everything written on it.
#[derive(Serialize, ToSchema)]
pub struct TicketDetail {
    pub id: Uuid,
    pub number: i64,
    pub title: String,
    pub state: TicketState,
    pub priority: Priority,
    pub customer: TicketCustomer,
    pub requester: Option<Requester>,
    /// The key requesters of the ticket's customer. Without a requester, a reply on the ticket goes
    /// to each of them, at their contact address; with none either, it cannot be replied to. Given
    /// to agents only: add-ons never get it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_requesters: Option<Vec<crate::key_requesters::KeyRequester>>,
    pub assignee: Option<Party>,
    /// Assets of the ticket's customer the ticket is about.
    pub assets: Vec<crate::catalog::LinkedAsset>,
    /// Services of the help desk the ticket is about.
    pub services: Vec<crate::catalog::LinkedService>,
    /// The parent ticket tracking the failure this ticket is about, if any.
    pub parent: Option<crate::parents::RelatedTicket>,
    /// The customers' tickets about the failure this ticket tracks.
    pub children: Vec<crate::parents::RelatedTicket>,
    /// Open proposals of add-ons such as the Assistant.
    pub proposals: Vec<crate::proposals::ProposalView>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    /// Messages and notes, oldest first.
    pub timeline: Vec<TimelineEntry>,
}

#[derive(Serialize, ToSchema)]
pub struct Party {
    pub id: Uuid,
    pub name: String,
}

#[derive(Serialize, ToSchema)]
pub struct TicketCustomer {
    pub id: Uuid,
    pub name: String,
    /// Whether this is the help desk's own organization.
    pub is_help_desk: bool,
}

#[derive(Serialize, ToSchema)]
pub struct Requester {
    pub id: Uuid,
    /// The address that stands for the requester on this ticket, where a reply goes: the one they
    /// last wrote from on it, else the one an agent last picked there, else the first they
    /// registered. Empty for a departed requester whose addresses have all passed to others.
    pub email: String,
    /// All the requester's addresses, the first registered first.
    pub addresses: Vec<String>,
    /// The address the requester last wrote from on this ticket, where a reply goes. Empty when
    /// they have not written on it: a reply then goes to the address the agent picks.
    pub reply_to: Option<String>,
    /// On a ticket the requester has not written on, the address an agent picked for the last
    /// reply there: the next reply goes there too unless the agent picks another.
    pub last_picked: Option<String>,
    pub name: Option<String>,
    /// How many tickets the requester has, this one included.
    pub ticket_count: i64,
    /// The requester has left their customer: their messages wait in the inbox.
    pub departed: bool,
}

#[derive(Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum TimelineEntry {
    /// An e-mail received from, or sent to, the requester.
    Message(MessageEntry),
    /// An internal remark, seen only by agents.
    Note(NoteEntry),
}

/// Whether the help desk received a message or sent it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum Direction {
    Inbound,
    Outbound,
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct MessageEntry {
    pub id: Uuid,
    pub direction: Direction,
    pub from_address: String,
    pub from_name: Option<String>,
    pub to_address: Option<String>,
    /// The agent who wrote an outbound message.
    pub author_name: Option<String>,
    pub subject: String,
    pub body_text: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// When an outbound message left; empty while it waits in the outbox.
    #[serde(with = "time::serde::rfc3339::option")]
    pub sent_at: Option<OffsetDateTime>,
    /// Why the last attempt to send it failed.
    pub last_error: Option<String>,
    /// The outbox stopped trying: the message will not be sent.
    pub gave_up: bool,
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct NoteEntry {
    pub id: Uuid,
    pub author_name: String,
    pub body: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

pub async fn detail(db: &PgPool, number: i64) -> Result<TicketDetail> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: Uuid,
        title: String,
        state: TicketState,
        priority: Priority,
        customer_id: Uuid,
        customer_name: String,
        customer_is_help_desk: bool,
        requester_id: Option<Uuid>,
        requester_address: Option<String>,
        requester_name: Option<String>,
        requester_tickets: Option<i64>,
        requester_departed: Option<bool>,
        assignee_id: Option<Uuid>,
        assignee_name: Option<String>,
        created_at: OffsetDateTime,
        updated_at: OffsetDateTime,
    }
    let row: Row = sqlx::query_as(
        "SELECT t.id, t.title, t.state, t.priority, c.id AS customer_id, c.name AS customer_name,
                c.is_help_desk AS customer_is_help_desk,
                r.id AS requester_id, ticket_requester_address(t.id) AS requester_address,
                r.name AS requester_name,
                (SELECT count(*) FROM tickets o WHERE o.requester_id = r.id) AS requester_tickets,
                r.departed_at IS NOT NULL AS requester_departed,
                a.id AS assignee_id, a.name AS assignee_name, t.created_at, t.updated_at
         FROM tickets t
         JOIN customers c ON c.id = t.customer_id
         LEFT JOIN requesters r ON r.id = t.requester_id
         LEFT JOIN agents a ON a.id = t.assignee_id
         WHERE t.number = $1",
    )
    .bind(number)
    .fetch_optional(db)
    .await?
    .ok_or(Error::NotFound("ticket"))?;

    let messages: Vec<MessageEntry> = sqlx::query_as(
        "SELECT m.id, m.direction, m.from_address, m.from_name, m.to_address,
                a.name AS author_name, m.subject, m.body_text, m.created_at, m.sent_at, m.last_error,
                (m.direction = 'outbound' AND m.sent_at IS NULL AND m.send_attempts >= $2) AS gave_up
         FROM messages m LEFT JOIN agents a ON a.id = m.author_id
         WHERE m.ticket_id = $1",
    )
    .bind(row.id)
    .bind(crate::outbound::MAX_ATTEMPTS)
    .fetch_all(db)
    .await?;
    let notes: Vec<NoteEntry> = sqlx::query_as(
        "SELECT n.id, a.name AS author_name, n.body, n.created_at
         FROM notes n JOIN agents a ON a.id = n.author_id
         WHERE n.ticket_id = $1",
    )
    .bind(row.id)
    .fetch_all(db)
    .await?;
    let mut timeline: Vec<(OffsetDateTime, TimelineEntry)> = messages
        .into_iter()
        .map(|m| (m.created_at, TimelineEntry::Message(m)))
        .chain(
            notes
                .into_iter()
                .map(|n| (n.created_at, TimelineEntry::Note(n))),
        )
        .collect();
    timeline.sort_by_key(|(at, _)| *at);
    let (assets, services) = crate::catalog::ticket_links(db, row.id).await?;
    let (parent, children) = crate::parents::related(db, row.id).await?;
    let proposals = crate::proposals::open_on_ticket(db, row.id).await?;
    let recipient = crate::requesters::recipient(&mut *db.acquire().await?, row.id).await?;
    let key_requesters = crate::key_requesters::of_customer(db, row.customer_id).await?;

    Ok(TicketDetail {
        id: row.id,
        number,
        title: row.title,
        state: row.state,
        priority: row.priority,
        customer: TicketCustomer {
            id: row.customer_id,
            name: row.customer_name,
            is_help_desk: row.customer_is_help_desk,
        },
        requester: row
            .requester_id
            .zip(recipient)
            .map(|(id, recipient)| Requester {
                id,
                email: row.requester_address.unwrap_or_default(),
                addresses: recipient.addresses,
                last_picked: recipient
                    .last_picked
                    .filter(|_| recipient.wrote_from.is_none()),
                reply_to: recipient.wrote_from,
                name: row.requester_name,
                ticket_count: row.requester_tickets.unwrap_or(1),
                departed: row.requester_departed.unwrap_or(false),
            }),
        key_requesters: Some(key_requesters),
        assignee: row.assignee_id.map(|id| Party {
            id,
            name: row.assignee_name.unwrap_or_default(),
        }),
        assets,
        services,
        parent,
        children,
        proposals,
        created_at: row.created_at,
        updated_at: row.updated_at,
        timeline: timeline.into_iter().map(|(_, entry)| entry).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::error::Error;
    use crate::testing::{self, sender, state};

    #[sqlx::test]
    async fn a_reply_is_queued_threaded_and_moves_the_ticket(db: PgPool) {
        let c = testing::customer(&db, "Brightfield").await;
        let r = testing::requester(&db, c, "ada@brightfield.test").await;
        let a = testing::agent(&db, "marco@northgate.test").await;
        let n = testing::ticket(&db, c, Some(r), "first@brightfield.test").await;

        reply(
            &db,
            &sender(),
            n,
            a,
            "Please restart it.",
            None,
            TicketState::Waiting,
        )
        .await
        .unwrap();

        type Row = (
            String,
            String,
            Option<String>,
            Vec<String>,
            Option<OffsetDateTime>,
        );
        let (to, subject, in_reply_to, refs, sent): Row = sqlx::query_as(
            r#"SELECT to_address, subject, in_reply_to, "references", sent_at FROM messages
               WHERE direction = 'outbound'"#,
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(to, "ada@brightfield.test");
        assert_eq!(subject, format!("Re: Printer jams [#{n}]"));
        assert_eq!(in_reply_to.as_deref(), Some("first@brightfield.test"));
        assert_eq!(refs, vec!["first@brightfield.test"]);
        assert!(sent.is_none(), "queued, not sent");
        assert_eq!(state(&db, n).await, "waiting");
    }

    /// The outbound messages on the ticket: to whom, oldest first.
    async fn sent_to(db: &PgPool, number: i64) -> Vec<String> {
        detail(db, number)
            .await
            .unwrap()
            .timeline
            .into_iter()
            .filter_map(|entry| match entry {
                TimelineEntry::Message(m) if m.direction == Direction::Outbound => m.to_address,
                _ => None,
            })
            .collect()
    }

    #[sqlx::test]
    async fn a_ticket_with_neither_requester_nor_key_requesters_cannot_be_replied_to(db: PgPool) {
        let c = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let ivo = testing::requester(&db, harbor, "ivo@harbor.test").await;
        crate::key_requesters::mark(&db, ivo, None).await.unwrap();
        let a = testing::agent(&db, "marco@northgate.test").await;
        let n = testing::ticket(&db, c, None, "first@brightfield.test").await;
        let Err(Error::Rule(why)) =
            reply(&db, &sender(), n, a, "Hello", None, TicketState::Waiting).await
        else {
            panic!("a reply to nobody");
        };
        assert!(why.contains("key requester"), "{why}");
        assert_eq!(sent_to(&db, n).await, Vec::<String>::new());
    }

    /// On a ticket without a requester, a reply goes to every key requester of the ticket's
    /// customer, each at their contact address and in a message of their own; the ticket stays
    /// without a requester.
    #[sqlx::test]
    async fn a_reply_without_a_requester_goes_to_every_key_requester(db: PgPool) {
        let c = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let ada = testing::requester(&db, c, "ada@brightfield.test").await;
        crate::requesters::add_address(&db, ada, "ada@home.test")
            .await
            .unwrap();
        let lena = testing::requester(&db, c, "lena@brightfield.test").await;
        testing::requester(&db, c, "nora@brightfield.test").await;
        let ivo = testing::requester(&db, harbor, "ivo@harbor.test").await;
        for (key, contact) in [(ada, Some("ada@home.test")), (lena, None), (ivo, None)] {
            crate::key_requesters::mark(&db, key, contact)
                .await
                .unwrap();
        }
        let a = testing::agent(&db, "marco@northgate.test").await;
        let n = testing::ticket(&db, c, None, "first@brightfield.test").await;

        let Err(Error::Rule(_)) = reply(
            &db,
            &sender(),
            n,
            a,
            "Hello",
            Some("ada@brightfield.test"),
            TicketState::Waiting,
        )
        .await
        else {
            panic!("an address was picked among the key requesters");
        };
        reply(
            &db,
            &sender(),
            n,
            a,
            "We are on it.",
            None,
            TicketState::Waiting,
        )
        .await
        .unwrap();
        let mut to = sent_to(&db, n).await;
        to.sort();
        assert_eq!(to, ["ada@home.test", "lena@brightfield.test"]);
        let ticket = detail(&db, n).await.unwrap();
        assert!(ticket.requester.is_none());
        assert_eq!(ticket.state, TicketState::Waiting);
    }

    #[sqlx::test]
    async fn a_closed_ticket_never_changes(db: PgPool) {
        let c = testing::customer(&db, "Brightfield").await;
        let r = testing::requester(&db, c, "ada@brightfield.test").await;
        let a = testing::agent(&db, "marco@northgate.test").await;
        let n = testing::ticket(&db, c, Some(r), "first@brightfield.test").await;
        set_state(&db, n, TicketState::Closed).await.unwrap();

        assert!(matches!(
            set_state(&db, n, TicketState::Open).await,
            Err(Error::Rule(_))
        ));
        assert!(matches!(assign(&db, n, Some(a)).await, Err(Error::Rule(_))));
        assert!(matches!(
            reply(&db, &sender(), n, a, "Hi", None, TicketState::Waiting).await,
            Err(Error::Rule(_))
        ));
        assert_eq!(state(&db, n).await, "closed");
    }

    #[sqlx::test]
    async fn notes_and_messages_come_back_in_order(db: PgPool) {
        let c = testing::customer(&db, "Brightfield").await;
        let r = testing::requester(&db, c, "ada@brightfield.test").await;
        let a = testing::agent(&db, "marco@northgate.test").await;
        let n = testing::ticket(&db, c, Some(r), "first@brightfield.test").await;
        add_note(&db, n, a, "Probably the fuser.").await.unwrap();
        reply(
            &db,
            &sender(),
            n,
            a,
            "We will look at it.",
            None,
            TicketState::Open,
        )
        .await
        .unwrap();
        assign(&db, n, Some(a)).await.unwrap();

        let ticket = detail(&db, n).await.unwrap();
        let kinds: Vec<&str> = ticket
            .timeline
            .iter()
            .map(|e| match e {
                TimelineEntry::Message(m) if m.direction == Direction::Inbound => "in",
                TimelineEntry::Message(_) => "out",
                TimelineEntry::Note(_) => "note",
            })
            .collect();
        assert_eq!(kinds, ["in", "note", "out"]);
        assert_eq!(ticket.assignee.map(|p| p.id), Some(a));
        assert_eq!(
            ticket.requester.map(|r| r.email).as_deref(),
            Some("ada@brightfield.test")
        );
        assert!(matches!(detail(&db, 9999).await, Err(Error::NotFound(_))));
    }

    #[sqlx::test]
    async fn moving_the_requester_takes_all_their_tickets(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let r = testing::requester(&db, brightfield, "luca.bassi@mail.test").await;
        let first = testing::ticket(&db, brightfield, Some(r), "l1@mail.test").await;
        let second = testing::ticket(&db, brightfield, Some(r), "l2@mail.test").await;
        set_state(&db, second, TicketState::Closed).await.unwrap();
        let other = testing::ticket(&db, brightfield, None, "o1@brightfield.test").await;

        let moved = move_to_customer(&db, first, harbor, RequesterMove::Move)
            .await
            .unwrap();
        assert_eq!(moved.moved, 2);
        let customer_of = |n: i64| {
            let db = db.clone();
            async move {
                sqlx::query_scalar::<_, Uuid>("SELECT customer_id FROM tickets WHERE number = $1")
                    .bind(n)
                    .fetch_one(&db)
                    .await
                    .unwrap()
            }
        };
        assert_eq!(customer_of(first).await, harbor);
        assert_eq!(
            customer_of(second).await,
            harbor,
            "closed tickets follow too"
        );
        assert_eq!(
            customer_of(other).await,
            brightfield,
            "other requesters' tickets stay"
        );
    }

    #[sqlx::test]
    async fn removing_the_requester_moves_only_this_ticket(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let r = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let first = testing::ticket(&db, brightfield, Some(r), "a1@brightfield.test").await;
        let second = testing::ticket(&db, brightfield, Some(r), "a2@brightfield.test").await;

        assert_eq!(
            move_to_customer(&db, first, harbor, RequesterMove::Remove)
                .await
                .unwrap()
                .moved,
            1
        );
        let ticket = detail(&db, first).await.unwrap();
        assert_eq!(ticket.customer.id, harbor);
        assert!(ticket.requester.is_none());
        assert_eq!(detail(&db, second).await.unwrap().customer.id, brightfield);
        assert!(matches!(
            move_to_customer(&db, first, harbor, RequesterMove::Remove).await,
            Err(Error::Rule(_))
        ));
    }

    #[sqlx::test]
    async fn an_agent_opens_a_ticket_by_hand(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let a = testing::agent(&db, "marco@northgate.test").await;
        testing::requester(&db, harbor, "ivo@harbor.test").await;

        let n = open(
            &db,
            brightfield,
            "Phone: VPN down",
            Some("Ada@Brightfield.test"),
            Some("Called at 9."),
            a,
        )
        .await
        .unwrap();
        let ticket = detail(&db, n).await.unwrap();
        assert_eq!(
            ticket.requester.map(|r| r.email).as_deref(),
            Some("ada@brightfield.test")
        );
        assert!(
            matches!(&ticket.timeline[..], [TimelineEntry::Note(note)] if note.body == "Called at 9.")
        );
        assert!(!ticket.customer.is_help_desk);

        let planned = open(&db, brightfield, "Renew the certificate", None, None, a)
            .await
            .unwrap();
        assert!(detail(&db, planned).await.unwrap().requester.is_none());
        assert!(matches!(
            open(&db, brightfield, "x", Some("ivo@harbor.test"), None, a).await,
            Err(Error::Rule(_))
        ));
        assert!(matches!(
            open(&db, brightfield, "  ", None, None, a).await,
            Err(Error::Rule(_))
        ));
    }

    #[sqlx::test]
    async fn no_ticket_is_opened_by_hand_for_a_departed_requester(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let a = testing::agent(&db, "marco@northgate.test").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        crate::requesters::depart(&db, ada).await.unwrap();

        let Err(Error::Rule(why)) = open(
            &db,
            brightfield,
            "Phone: VPN down",
            Some("ada@brightfield.test"),
            None,
            a,
        )
        .await
        else {
            panic!("a ticket opened for a departed requester");
        };
        assert!(why.contains("departed"), "{why}");

        let n = open(
            &db,
            harbor,
            "Phone: VPN down",
            Some("ada@brightfield.test"),
            None,
            a,
        )
        .await
        .unwrap();
        let requester = detail(&db, n).await.unwrap().requester.unwrap();
        assert_ne!(requester.id, ada, "the address passed to a new requester");
    }

    #[sqlx::test]
    async fn an_empty_note_is_refused(db: PgPool) {
        let c = testing::customer(&db, "Brightfield").await;
        let a = testing::agent(&db, "marco@northgate.test").await;
        let n = testing::ticket(&db, c, None, "first@brightfield.test").await;
        assert!(matches!(
            add_note(&db, n, a, "   ").await,
            Err(Error::Rule(_))
        ));
    }

    /// A key requester unmarked after the recipients were read, by code that does not hold their
    /// lock, is refused as a change to reload, not as a failure of the help desk.
    #[sqlx::test]
    async fn a_recipient_who_stopped_being_key_is_a_conflict(db: PgPool) {
        let c = testing::customer(&db, "Brightfield").await;
        let lena = testing::requester(&db, c, "lena@brightfield.test").await;
        crate::key_requesters::mark(&db, lena, None).await.unwrap();
        let a = testing::agent(&db, "marco@northgate.test").await;
        let n = testing::ticket(&db, c, None, "n1@brightfield.test").await;

        let mut tx = db.begin().await.unwrap();
        let (ticket, _) = lock(&mut tx, n).await.unwrap();
        let outgoing = Outgoing::on(&mut tx, ticket, a, "We are on it.")
            .await
            .unwrap();
        crate::key_requesters::unmark(&db, lena).await.unwrap();
        let Err(Error::Rule(why)) = outgoing
            .queue(&mut tx, &sender(), "lena@brightfield.test")
            .await
        else {
            panic!("queued to someone who is no longer a key requester");
        };
        assert!(why.contains("reload"), "{why}");
    }

    /// The database sends a message on a ticket only to its requester or to a key requester of its
    /// customer, at their contact address, also when the code is bypassed; a ticket with neither
    /// gets none.
    #[sqlx::test]
    async fn the_database_writes_only_to_the_requester_or_key_requesters(db: PgPool) {
        let c = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let ada = testing::requester(&db, c, "ada@brightfield.test").await;
        let lena = testing::requester(&db, c, "lena@brightfield.test").await;
        crate::requesters::add_address(&db, lena, "lena@home.test")
            .await
            .unwrap();
        crate::key_requesters::mark(&db, lena, Some("lena@home.test"))
            .await
            .unwrap();
        let ivo = testing::requester(&db, harbor, "ivo@harbor.test").await;
        crate::key_requesters::mark(&db, ivo, None).await.unwrap();
        let a = testing::agent(&db, "marco@northgate.test").await;
        let hers = testing::ticket(&db, c, Some(ada), "a1@brightfield.test").await;
        let none = testing::ticket(&db, c, None, "n1@brightfield.test").await;
        let send = |number: i64, to: &'static str| {
            let db = db.clone();
            async move {
                sqlx::query(
                    "INSERT INTO messages (id, ticket_id, direction, from_address, to_address,
                                           author_id, subject, body_text)
                     SELECT $1, id, 'outbound', 'helpdesk@helpdesk.test', $3, $4, 'Re: x', 'Hi.'
                     FROM tickets WHERE number = $2",
                )
                .bind(Uuid::new_v4())
                .bind(number)
                .bind(to)
                .bind(a)
                .execute(&db)
                .await
                .is_ok()
            }
        };

        assert!(send(hers, "ada@brightfield.test").await);
        assert!(send(hers, "lena@home.test").await, "a key requester");
        assert!(send(none, "lena@home.test").await);
        assert!(
            !send(none, "lena@brightfield.test").await,
            "not the contact address"
        );
        assert!(
            !send(none, "ivo@harbor.test").await,
            "another customer's key requester"
        );
        assert!(
            !send(none, "someone@elsewhere.test").await,
            "an address of nobody"
        );
    }

    /// The names of the sites the ticket is at, in order.
    async fn at(db: &PgPool, number: i64) -> Vec<String> {
        sites(db, &[number])
            .await
            .unwrap()
            .remove(&number)
            .unwrap_or_default()
            .into_iter()
            .map(|s| s.name)
            .collect()
    }

    /// A site of the customer, by name.
    async fn site(db: &PgPool, customer: Uuid, name: &str) -> Uuid {
        crate::sites::create(db, customer, name, "Via dei Tigli 1")
            .await
            .unwrap()
    }

    /// Puts the requester at `site`, or at none.
    async fn put_at(db: &PgPool, requester: Uuid, site: Option<Uuid>) {
        let details = crate::requesters::RequesterDetails {
            site_id: site,
            ..Default::default()
        };
        crate::requesters::update(db, requester, &details)
            .await
            .unwrap();
    }

    /// A ticket that links assets is at the sites where they are: a laptop at its holder's site,
    /// a printer at its own, whatever its holder; its requester's site does not count.
    #[sqlx::test]
    async fn a_ticket_is_at_the_sites_of_its_assets(db: PgPool) {
        let c = testing::customer(&db, "Brightfield").await;
        let head = site(&db, c, "Head office").await;
        let lab = site(&db, c, "Lab").await;
        let warehouse = site(&db, c, "Warehouse").await;
        let depot = site(&db, c, "Depot").await;
        let ada = testing::requester(&db, c, "ada@brightfield.test").await;
        put_at(&db, ada, Some(lab)).await;
        let lena = testing::requester(&db, c, "lena@brightfield.test").await;
        put_at(&db, lena, Some(head)).await;
        let laptop = crate::asset_kinds::create(&db, "Laptop", true)
            .await
            .unwrap();
        let printer = crate::asset_kinds::create(&db, "Printer", false)
            .await
            .unwrap();
        let pc = crate::catalog::create_asset(
            &db,
            c,
            &testing::asset_details(laptop, "PC-0042", None, Some(depot), Some(ada)),
        )
        .await
        .unwrap();
        let printer_2f = crate::catalog::create_asset(
            &db,
            c,
            &testing::asset_details(printer, "Printer 2F", None, Some(warehouse), Some(ada)),
        )
        .await
        .unwrap();
        let n = testing::ticket(&db, c, Some(lena), "a1@brightfield.test").await;
        for asset in [printer_2f, pc] {
            crate::catalog::link_asset(&db, n, asset, true)
                .await
                .unwrap();
        }

        assert_eq!(at(&db, n).await, ["Lab", "Warehouse"]);
    }

    /// A ticket that links no asset is at its requester's site; without a requester it is at none,
    /// and so is one whose assets are at no site, whatever its requester.
    #[sqlx::test]
    async fn a_ticket_without_assets_is_at_its_requester_s_site(db: PgPool) {
        let c = testing::customer(&db, "Brightfield").await;
        let head = site(&db, c, "Head office").await;
        let lena = testing::requester(&db, c, "lena@brightfield.test").await;
        put_at(&db, lena, Some(head)).await;
        let hers = testing::ticket(&db, c, Some(lena), "a1@brightfield.test").await;
        let nobody_s = testing::ticket(&db, c, None, "a2@brightfield.test").await;
        let with_asset = testing::ticket(&db, c, Some(lena), "a3@brightfield.test").await;
        let pc = testing::asset(&db, c, "Laptop", "PC-0042").await;
        crate::catalog::link_asset(&db, with_asset, pc, true)
            .await
            .unwrap();

        assert_eq!(at(&db, hers).await, ["Head office"]);
        assert_eq!(at(&db, nobody_s).await, Vec::<String>::new());
        assert_eq!(
            at(&db, with_asset).await,
            Vec::<String>::new(),
            "its asset is at no site"
        );
    }

    /// A closed ticket stays at the sites it was at when it was closed, whatever happens to its
    /// assets and its requester afterwards, while one still open follows them.
    #[sqlx::test]
    async fn a_closed_ticket_keeps_its_sites(db: PgPool) {
        let c = testing::customer(&db, "Brightfield").await;
        let head = site(&db, c, "Head office").await;
        let lab = site(&db, c, "Lab").await;
        let ada = testing::requester(&db, c, "ada@brightfield.test").await;
        put_at(&db, ada, Some(head)).await;
        let laptop = crate::asset_kinds::create(&db, "Laptop", true)
            .await
            .unwrap();
        let pc = crate::catalog::create_asset(
            &db,
            c,
            &testing::asset_details(laptop, "PC-0042", None, None, Some(ada)),
        )
        .await
        .unwrap();
        let by_asset = testing::ticket(&db, c, None, "a1@brightfield.test").await;
        crate::catalog::link_asset(&db, by_asset, pc, true)
            .await
            .unwrap();
        let by_requester = testing::ticket(&db, c, Some(ada), "a2@brightfield.test").await;
        let open = testing::ticket(&db, c, Some(ada), "a3@brightfield.test").await;
        for n in [by_asset, by_requester] {
            set_state(&db, n, TicketState::Closed).await.unwrap();
        }

        put_at(&db, ada, Some(lab)).await;
        assert_eq!(at(&db, open).await, ["Lab"], "an open ticket follows");
        crate::catalog::update_asset(
            &db,
            pc,
            &testing::asset_details(laptop, "PC-0042", None, Some(lab), None),
        )
        .await
        .unwrap();
        crate::sites::retire(&db, head).await.unwrap();
        for n in [by_asset, by_requester] {
            assert_eq!(at(&db, n).await, ["Head office"], "ticket {n}");
        }
    }

    #[test]
    fn reply_subjects_do_not_stack() {
        assert_eq!(
            reply_subject("PC will not start", 7),
            "Re: PC will not start [#7]"
        );
        assert_eq!(
            reply_subject("Re: PC will not start", 7),
            "Re: PC will not start [#7]"
        );
    }
}
