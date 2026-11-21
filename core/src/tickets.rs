//! What agents do on a ticket: read it, write notes, reply to the requester, assign it and move
//! it between states. Rules are in docs/domain-rules.md ("Tickets").

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
        let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM agents WHERE id = $1)")
            .bind(agent)
            .fetch_one(&mut *tx)
            .await?;
        if !exists {
            return Err(Error::NotFound("agent"));
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

/// Queues a message to the ticket's requester and moves the ticket to `then`. The message is
/// sent by the outbox; it threads with the conversation so that the answer comes back here.
pub async fn reply(
    db: &PgPool,
    sender: &Sender,
    number: i64,
    author: Uuid,
    body: &str,
    then: TicketState,
) -> Result<Uuid> {
    let body = body.trim();
    if body.is_empty() {
        return rule("a reply cannot be empty");
    }
    let mut tx = db.begin().await?;
    let (ticket_id, state) = lock(&mut tx, number).await?;
    not_closed(state)?;
    let (title, requester): (String, Option<String>) = sqlx::query_as(
        "SELECT t.title, r.email FROM tickets t LEFT JOIN requesters r ON r.id = t.requester_id
         WHERE t.id = $1",
    )
    .bind(ticket_id)
    .fetch_one(&mut *tx)
    .await?;
    let Some(to) = requester else {
        return rule("the ticket has no requester to reply to");
    };

    // The conversation so far, newest last, to thread the reply.
    let mut thread: Vec<(String, String)> = sqlx::query_as(
        "SELECT rfc_message_id, direction FROM messages
         WHERE ticket_id = $1 AND rfc_message_id IS NOT NULL
         ORDER BY created_at DESC LIMIT 20",
    )
    .bind(ticket_id)
    .fetch_all(&mut *tx)
    .await?;
    thread.reverse();
    let in_reply_to = thread
        .iter()
        .rev()
        .find(|(_, direction)| direction == "inbound")
        .or(thread.last())
        .map(|(id, _)| id.clone());
    let references: Vec<String> = thread.into_iter().map(|(id, _)| id).collect();

    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO messages (id, ticket_id, direction, rfc_message_id, in_reply_to, \"references\",
                               from_address, from_name, to_address, author_id, subject, body_text)
         VALUES ($1, $2, 'outbound', $3, $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(id)
    .bind(ticket_id)
    .bind(sender.new_message_id())
    .bind(in_reply_to)
    .bind(&references)
    .bind(&sender.address)
    .bind(&sender.name)
    .bind(&to)
    .bind(author)
    .bind(reply_subject(&title, number))
    .bind(body)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE tickets SET state = $2, updated_at = now() WHERE id = $1")
        .bind(ticket_id)
        .bind(then)
        .execute(&mut *tx)
        .await?;
    crate::drafts::clear(&mut tx, ticket_id, author).await?;
    tx.commit().await?;
    Ok(id)
}

/// Opens a ticket by hand, for example after a phone call (docs/domain-rules.md, "Incoming
/// messages"). With a requester's address, that requester is used or, if new, registered with
/// the customer; an address that is a requester of another customer is refused. The optional note
/// records what was said. Returns the ticket's number.
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
    let requester = match requester_email.map(|e| e.trim().to_lowercase()) {
        None => None,
        Some(email) if email.is_empty() => None,
        Some(email) => {
            match email.rsplit_once('@') {
                Some((local, domain)) if !local.is_empty() && domain.contains('.') => {}
                _ => return rule(format!("{email:?} is not an e-mail address")),
            }
            let existing: Option<(Uuid, Uuid, String)> = sqlx::query_as(
                "SELECT r.id, r.customer_id, c.name FROM requesters r
                 JOIN customers c ON c.id = r.customer_id WHERE r.email = $1",
            )
            .bind(&email)
            .fetch_optional(&mut *tx)
            .await?;
            match existing {
                Some((id, of, _)) if of == customer => Some(id),
                Some((_, _, other)) => {
                    return rule(format!(
                        "this address is already a requester of {other}, and a requester belongs to one customer"
                    ));
                }
                None => {
                    let id = Uuid::new_v4();
                    sqlx::query(
                        "INSERT INTO requesters (id, customer_id, email) VALUES ($1, $2, $3)",
                    )
                    .bind(id)
                    .bind(customer)
                    .bind(&email)
                    .execute(&mut *tx)
                    .await?;
                    Some(id)
                }
            }
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
/// taking all of their tickets (closed ones too), or leaves this ticket. Links that do not fit the
/// new customer are removed.
pub async fn move_to_customer(
    db: &PgPool,
    number: i64,
    customer: Uuid,
    requester: RequesterMove,
) -> Result<Moved> {
    let mut tx = db.begin().await?;
    let (ticket_id, state) = lock(&mut tx, number).await?;
    not_closed(state)?;
    let (current, requester_id): (Uuid, Option<Uuid>) =
        sqlx::query_as("SELECT customer_id, requester_id FROM tickets WHERE id = $1")
            .bind(ticket_id)
            .fetch_one(&mut *tx)
            .await?;
    if current == customer {
        return rule("the ticket already belongs to this customer");
    }
    let is_parent: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM tickets WHERE parent_id = $1)")
            .bind(ticket_id)
            .fetch_one(&mut *tx)
            .await?;
    if is_parent {
        return rule("a parent ticket stays with the help desk's own organization");
    }
    let offboarded: Option<bool> =
        sqlx::query_scalar("SELECT offboarded_at IS NOT NULL FROM customers WHERE id = $1")
            .bind(customer)
            .fetch_optional(&mut *tx)
            .await?;
    match offboarded {
        None => return Err(Error::NotFound("customer")),
        Some(true) => return rule("the customer is offboarded and gets no tickets"),
        Some(false) => {}
    }

    let affected: Vec<Uuid> = match (requester_id, requester) {
        (Some(requester_id), RequesterMove::Move) => {
            sqlx::query_scalar("SELECT id FROM tickets WHERE requester_id = $1")
                .bind(requester_id)
                .fetch_all(&mut *tx)
                .await?
        }
        _ => vec![ticket_id],
    };
    let unlinked = crate::catalog::unlink_for_move(&mut tx, &affected, customer).await?;

    let moved = match (requester_id, requester) {
        (Some(requester_id), RequesterMove::Move) => {
            // The foreign key cascades the new customer to every ticket of the requester.
            sqlx::query("UPDATE requesters SET customer_id = $2 WHERE id = $1")
                .bind(requester_id)
                .bind(customer)
                .execute(&mut *tx)
                .await?;
            let moved: i64 =
                sqlx::query_scalar("SELECT count(*) FROM tickets WHERE requester_id = $1")
                    .bind(requester_id)
                    .fetch_one(&mut *tx)
                    .await?;
            sqlx::query("UPDATE tickets SET updated_at = now() WHERE requester_id = $1")
                .bind(requester_id)
                .execute(&mut *tx)
                .await?;
            moved
        }
        (Some(_), RequesterMove::Remove) | (None, _) => {
            sqlx::query(
                "UPDATE tickets SET customer_id = $2, requester_id = NULL, updated_at = now()
                 WHERE id = $1",
            )
            .bind(ticket_id)
            .bind(customer)
            .execute(&mut *tx)
            .await?;
            1
        }
    };
    tx.commit().await?;
    Ok(Moved { moved, unlinked })
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
    pub email: String,
    pub name: Option<String>,
    /// How many tickets the requester has, this one included.
    pub ticket_count: i64,
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
        requester_email: Option<String>,
        requester_name: Option<String>,
        requester_tickets: Option<i64>,
        assignee_id: Option<Uuid>,
        assignee_name: Option<String>,
        created_at: OffsetDateTime,
        updated_at: OffsetDateTime,
    }
    let row: Row = sqlx::query_as(
        "SELECT t.id, t.title, t.state, t.priority, c.id AS customer_id, c.name AS customer_name,
                c.is_help_desk AS customer_is_help_desk,
                r.id AS requester_id, r.email AS requester_email, r.name AS requester_name,
                (SELECT count(*) FROM tickets o WHERE o.requester_id = r.id) AS requester_tickets,
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
        requester: row.requester_id.map(|id| Requester {
            id,
            email: row.requester_email.unwrap_or_default(),
            name: row.requester_name,
            ticket_count: row.requester_tickets.unwrap_or(1),
        }),
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

    #[sqlx::test]
    async fn a_ticket_without_requester_cannot_be_replied_to(db: PgPool) {
        let c = testing::customer(&db, "Brightfield").await;
        let a = testing::agent(&db, "marco@northgate.test").await;
        let n = testing::ticket(&db, c, None, "first@brightfield.test").await;
        let result = reply(&db, &sender(), n, a, "Hello", TicketState::Waiting).await;
        assert!(matches!(result, Err(Error::Rule(_))));
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
            reply(&db, &sender(), n, a, "Hi", TicketState::Waiting).await,
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
    async fn an_empty_note_is_refused(db: PgPool) {
        let c = testing::customer(&db, "Brightfield").await;
        let a = testing::agent(&db, "marco@northgate.test").await;
        let n = testing::ticket(&db, c, None, "first@brightfield.test").await;
        assert!(matches!(
            add_note(&db, n, a, "   ").await,
            Err(Error::Rule(_))
        ));
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
