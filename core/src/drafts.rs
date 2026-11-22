//! Drafts: what an agent is writing on a ticket and has not sent yet. The core keeps one draft per
//! agent and ticket, so that leaving the page or changing computer loses nothing; sending a reply
//! or a note clears it.

use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::{Error, Result};

/// Whether the draft is a reply to the requester or an internal note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum DraftMode {
    Reply,
    Note,
}

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct Draft {
    pub mode: DraftMode,
    pub body: String,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

async fn ticket_id(db: &PgPool, number: i64) -> Result<Uuid> {
    sqlx::query_scalar("SELECT id FROM tickets WHERE number = $1")
        .bind(number)
        .fetch_optional(db)
        .await?
        .ok_or(Error::NotFound("ticket"))
}

/// The agent's draft on the ticket, if any.
pub async fn get(db: &PgPool, number: i64, agent: Uuid) -> Result<Option<Draft>> {
    let ticket = ticket_id(db, number).await?;
    Ok(sqlx::query_as(
        "SELECT mode, body, updated_at FROM drafts WHERE ticket_id = $1 AND agent_id = $2",
    )
    .bind(ticket)
    .bind(agent)
    .fetch_optional(db)
    .await?)
}

/// Saves the agent's draft; an empty body removes it.
pub async fn save(
    db: &PgPool,
    number: i64,
    agent: Uuid,
    mode: DraftMode,
    body: &str,
) -> Result<()> {
    let ticket = ticket_id(db, number).await?;
    if body.trim().is_empty() {
        sqlx::query("DELETE FROM drafts WHERE ticket_id = $1 AND agent_id = $2")
            .bind(ticket)
            .bind(agent)
            .execute(db)
            .await?;
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO drafts (ticket_id, agent_id, mode, body) VALUES ($1, $2, $3, $4)
         ON CONFLICT (ticket_id, agent_id)
         DO UPDATE SET mode = $3, body = $4, updated_at = now()",
    )
    .bind(ticket)
    .bind(agent)
    .bind(mode)
    .bind(body)
    .execute(db)
    .await?;
    Ok(())
}

/// Clears the agent's draft once they sent a reply or a note on the ticket.
pub async fn clear(tx: &mut Transaction<'_, Postgres>, ticket: Uuid, agent: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM drafts WHERE ticket_id = $1 AND agent_id = $2")
        .bind(ticket)
        .bind(agent)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;
    use crate::tickets::{self, TicketState};

    #[sqlx::test]
    async fn a_draft_is_per_agent_and_cleared_by_sending(db: PgPool) {
        let c = testing::customer(&db, "Brightfield").await;
        let r = testing::requester(&db, c, "ada@brightfield.test").await;
        let marco = testing::agent(&db, "marco@northgate.test").await;
        let sara = testing::agent(&db, "sara@northgate.test").await;
        let n = testing::ticket(&db, c, Some(r), "a@brightfield.test").await;

        save(&db, n, marco, DraftMode::Reply, "Buongiorno Ada,")
            .await
            .unwrap();
        save(&db, n, sara, DraftMode::Note, "Check the PSU")
            .await
            .unwrap();
        assert_eq!(
            get(&db, n, marco).await.unwrap().unwrap().body,
            "Buongiorno Ada,"
        );
        save(&db, n, marco, DraftMode::Reply, "Buongiorno Ada, grazie")
            .await
            .unwrap();
        assert_eq!(
            get(&db, n, marco).await.unwrap().unwrap().body,
            "Buongiorno Ada, grazie"
        );

        tickets::reply(
            &db,
            &testing::sender(),
            n,
            marco,
            "Buongiorno Ada, grazie",
            None,
            TicketState::Waiting,
        )
        .await
        .unwrap();
        assert!(
            get(&db, n, marco).await.unwrap().is_none(),
            "sending clears it"
        );
        assert!(
            get(&db, n, sara).await.unwrap().is_some(),
            "another agent's draft stays"
        );
        tickets::add_note(&db, n, sara, "PSU replaced")
            .await
            .unwrap();
        assert!(get(&db, n, sara).await.unwrap().is_none());

        save(&db, n, marco, DraftMode::Note, "x").await.unwrap();
        save(&db, n, marco, DraftMode::Note, "  ").await.unwrap();
        assert!(
            get(&db, n, marco).await.unwrap().is_none(),
            "an empty draft is removed"
        );
        assert!(matches!(
            get(&db, 9999, marco).await,
            Err(Error::NotFound(_))
        ));
    }
}
