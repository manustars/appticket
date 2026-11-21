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
    tx.commit().await?;
    Ok(id)
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
    pub customer: Party,
    pub requester: Option<Requester>,
    pub assignee: Option<Party>,
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
pub struct Requester {
    pub id: Uuid,
    pub email: String,
    pub name: Option<String>,
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
        customer_id: Uuid,
        customer_name: String,
        requester_id: Option<Uuid>,
        requester_email: Option<String>,
        requester_name: Option<String>,
        assignee_id: Option<Uuid>,
        assignee_name: Option<String>,
        created_at: OffsetDateTime,
        updated_at: OffsetDateTime,
    }
    let row: Row = sqlx::query_as(
        "SELECT t.id, t.title, t.state, c.id AS customer_id, c.name AS customer_name,
                r.id AS requester_id, r.email AS requester_email, r.name AS requester_name,
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
                a.name AS author_name, m.subject, m.body_text, m.created_at, m.sent_at, m.last_error
         FROM messages m LEFT JOIN agents a ON a.id = m.author_id
         WHERE m.ticket_id = $1",
    )
    .bind(row.id)
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

    Ok(TicketDetail {
        id: row.id,
        number,
        title: row.title,
        state: row.state,
        customer: Party {
            id: row.customer_id,
            name: row.customer_name,
        },
        requester: row.requester_id.map(|id| Requester {
            id,
            email: row.requester_email.unwrap_or_default(),
            name: row.requester_name,
        }),
        assignee: row.assignee_id.map(|id| Party {
            id,
            name: row.assignee_name.unwrap_or_default(),
        }),
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
