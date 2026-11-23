//! Parent and child tickets: one failure that affects several customers is tracked by a parent
//! ticket of the help desk's own organization, and each customer's ticket about it is a child.

use serde::Serialize;
use sqlx::{PgPool, Postgres, Transaction};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::{Error, Result, rule};
use crate::outbound::Sender;
use crate::tickets::{self, TicketState};

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct RelatedTicket {
    pub number: i64,
    pub title: String,
    pub state: TicketState,
    pub customer_name: String,
}

struct Row {
    id: Uuid,
    state: TicketState,
    help_desk: bool,
    parent: Option<Uuid>,
}

async fn lock(tx: &mut Transaction<'_, Postgres>, number: i64) -> Result<Row> {
    let row: Option<(Uuid, TicketState, bool, Option<Uuid>)> = sqlx::query_as(
        "SELECT t.id, t.state, c.is_help_desk, t.parent_id
         FROM tickets t JOIN customers c ON c.id = t.customer_id
         WHERE t.number = $1 FOR UPDATE OF t",
    )
    .bind(number)
    .fetch_optional(&mut **tx)
    .await?;
    let (id, state, help_desk, parent) = row.ok_or(Error::NotFound("ticket"))?;
    Ok(Row {
        id,
        state,
        help_desk,
        parent,
    })
}

/// Makes ticket `child` a child of ticket `parent`.
pub async fn attach(db: &PgPool, child: i64, parent: i64) -> Result<()> {
    if child == parent {
        return rule("a ticket cannot be its own parent");
    }
    let mut tx = db.begin().await?;
    let p = lock(&mut tx, parent).await?;
    let c = lock(&mut tx, child).await?;
    if !p.help_desk {
        return rule("a parent ticket belongs to the help desk's own organization");
    }
    if p.parent.is_some() {
        return rule("the parent ticket is itself a child; parents are one level deep");
    }
    let has_children: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM tickets WHERE parent_id = $1)")
            .bind(c.id)
            .fetch_one(&mut *tx)
            .await?;
    if has_children {
        return rule("the ticket is a parent itself and cannot become a child");
    }
    for (state, which) in [(p.state, "parent"), (c.state, "child")] {
        if state == TicketState::Closed {
            return rule(format!("the {which} ticket is closed"));
        }
    }
    sqlx::query("UPDATE tickets SET parent_id = $2, updated_at = now() WHERE id = $1")
        .bind(c.id)
        .bind(p.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn detach(db: &PgPool, child: i64) -> Result<()> {
    let mut tx = db.begin().await?;
    let c = lock(&mut tx, child).await?;
    if c.state == TicketState::Closed {
        return rule("the ticket is closed; a closed ticket cannot change");
    }
    if c.parent.is_none() {
        return rule("the ticket has no parent");
    }
    sqlx::query("UPDATE tickets SET parent_id = NULL, updated_at = now() WHERE id = $1")
        .bind(c.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// The ticket's parent, if any, and its children.
pub async fn related(
    db: &PgPool,
    ticket: Uuid,
) -> Result<(Option<RelatedTicket>, Vec<RelatedTicket>)> {
    let parent = sqlx::query_as(
        "SELECT p.number, p.title, p.state, c.name AS customer_name
         FROM tickets t JOIN tickets p ON p.id = t.parent_id JOIN customers c ON c.id = p.customer_id
         WHERE t.id = $1",
    )
    .bind(ticket)
    .fetch_optional(db)
    .await?;
    let children = sqlx::query_as(
        "SELECT t.number, t.title, t.state, c.name AS customer_name
         FROM tickets t JOIN customers c ON c.id = t.customer_id
         WHERE t.parent_id = $1 ORDER BY c.name, t.number",
    )
    .bind(ticket)
    .fetch_all(db)
    .await?;
    Ok((parent, children))
}

/// What resolving the children did.
#[derive(Debug, Serialize, ToSchema)]
pub struct ResolvedChildren {
    /// Children moved to resolved.
    pub resolved: i64,
    /// Replies queued to their requesters.
    pub replied: i64,
}

/// Resolves every child that is still open or waiting, queuing `body` as a reply to each child's
/// requester (children without a requester, or whose departed requester has no address left, are
/// only resolved). The parent itself is not changed.
pub async fn resolve_children(
    db: &PgPool,
    sender: &Sender,
    parent: i64,
    author: Uuid,
    body: &str,
) -> Result<ResolvedChildren> {
    if body.trim().is_empty() {
        return rule("the message to the requesters cannot be empty");
    }
    let children: Vec<(i64, Uuid, bool)> = sqlx::query_as(
        "SELECT t.number, t.id,
                EXISTS (SELECT 1 FROM requester_addresses a WHERE a.requester_id = t.requester_id)
         FROM tickets t
         JOIN tickets p ON p.id = t.parent_id
         WHERE p.number = $1 AND t.state IN ('open', 'waiting')
         ORDER BY t.number",
    )
    .bind(parent)
    .fetch_all(db)
    .await?;
    if children.is_empty() {
        return rule("the ticket has no open or waiting children");
    }
    // Each reply goes where its requester last wrote from on the child, else where an agent's
    // last reply there went. A requester with several addresses and neither needs an agent to pick
    // one: refuse before changing anything, so that no child is left half done.
    let mut conn = db.acquire().await?;
    let mut undecided = Vec::new();
    for (number, id, has_address) in &children {
        if *has_address
            && let Some(recipient) = crate::requesters::recipient(&mut conn, *id).await?
            && recipient.choose(None).is_err()
        {
            undecided.push(format!("#{number}"));
        }
    }
    drop(conn);
    if !undecided.is_empty() {
        return rule(format!(
            "the requesters of {} have several addresses and have not written on their ticket: reply to them there first, picking the address",
            undecided.join(", ")
        ));
    }
    let mut done = ResolvedChildren {
        resolved: 0,
        replied: 0,
    };
    for (number, _, has_address) in children {
        if has_address {
            tickets::reply(
                db,
                sender,
                number,
                author,
                body,
                None,
                TicketState::Resolved,
            )
            .await?;
            done.replied += 1;
        } else {
            tickets::set_state(db, number, TicketState::Resolved).await?;
        }
        done.resolved += 1;
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;

    async fn help_desk(db: &PgPool) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO customers (id, name, is_help_desk) VALUES ($1, 'Northgate', true)",
        )
        .bind(id)
        .execute(db)
        .await
        .unwrap();
        id
    }

    #[sqlx::test]
    async fn only_a_help_desk_ticket_can_be_a_parent(db: PgPool) {
        let own = help_desk(&db).await;
        let brightfield = testing::customer(&db, "Brightfield").await;
        let outage = testing::ticket(&db, own, None, "outage@northgate.test").await;
        let a = testing::ticket(&db, brightfield, None, "a@brightfield.test").await;
        let b = testing::ticket(&db, brightfield, None, "b@brightfield.test").await;

        assert!(
            matches!(attach(&db, a, b).await, Err(Error::Rule(_))),
            "not the help desk's"
        );
        attach(&db, a, outage).await.unwrap();
        attach(&db, b, outage).await.unwrap();
        assert!(matches!(
            attach(&db, outage, outage).await,
            Err(Error::Rule(_))
        ));
        let other = testing::ticket(&db, own, None, "other@northgate.test").await;
        assert!(
            matches!(attach(&db, outage, other).await, Err(Error::Rule(_))),
            "one level"
        );

        let id: Uuid = sqlx::query_scalar("SELECT id FROM tickets WHERE number = $1")
            .bind(outage)
            .fetch_one(&db)
            .await
            .unwrap();
        let (parent, children) = related(&db, id).await.unwrap();
        assert!(parent.is_none());
        assert_eq!(
            children.iter().map(|c| c.number).collect::<Vec<_>>(),
            [a, b]
        );
        detach(&db, b).await.unwrap();
        assert_eq!(related(&db, id).await.unwrap().1.len(), 1);
    }

    #[sqlx::test]
    async fn resolving_children_writes_to_each_requester(db: PgPool) {
        let own = help_desk(&db).await;
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let ivo = testing::requester(&db, harbor, "ivo@harbor.test").await;
        let agent = testing::agent(&db, "marco@northgate.test").await;
        let outage = testing::ticket(&db, own, None, "outage@northgate.test").await;
        let a = testing::ticket(&db, brightfield, Some(ada), "a@brightfield.test").await;
        let b = testing::ticket(&db, harbor, Some(ivo), "b@harbor.test").await;
        let c = testing::ticket(&db, harbor, None, "c@harbor.test").await;
        for child in [a, b, c] {
            attach(&db, child, outage).await.unwrap();
        }

        let done = resolve_children(&db, &testing::sender(), outage, agent, "Mail works again.")
            .await
            .unwrap();
        assert_eq!((done.resolved, done.replied), (3, 2));
        for child in [a, b, c] {
            assert_eq!(testing::state(&db, child).await, "resolved");
        }
        assert_eq!(
            testing::state(&db, outage).await,
            "open",
            "the parent is untouched"
        );
        let queued: Vec<String> = sqlx::query_scalar(
            "SELECT to_address FROM messages WHERE direction = 'outbound' ORDER BY to_address",
        )
        .fetch_all(&db)
        .await
        .unwrap();
        assert_eq!(queued, ["ada@brightfield.test", "ivo@harbor.test"]);
        assert!(matches!(
            resolve_children(&db, &testing::sender(), outage, agent, "Again").await,
            Err(Error::Rule(_))
        ));
    }

    /// A departed requester whose addresses all passed to another requester cannot be written to:
    /// their child is resolved without a reply, as one without a requester is.
    #[sqlx::test]
    async fn a_child_whose_requester_has_no_address_left_is_only_resolved(db: PgPool) {
        let own = help_desk(&db).await;
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let agent = testing::agent(&db, "marco@northgate.test").await;
        let outage = testing::ticket(&db, own, None, "outage@northgate.test").await;
        let child = testing::ticket(&db, brightfield, Some(ada), "a@brightfield.test").await;
        attach(&db, child, outage).await.unwrap();
        crate::requesters::depart(&db, ada).await.unwrap();
        crate::customers::add_requester(&db, harbor, "ada@brightfield.test", None)
            .await
            .unwrap();

        let done = resolve_children(&db, &testing::sender(), outage, agent, "Fixed.")
            .await
            .unwrap();
        assert_eq!((done.resolved, done.replied), (1, 0));
        assert_eq!(testing::state(&db, child).await, "resolved");
    }

    #[sqlx::test]
    async fn children_are_answered_where_their_requesters_wrote_from(db: PgPool) {
        let own = help_desk(&db).await;
        let brightfield = testing::customer(&db, "Brightfield").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        crate::requesters::add_address(&db, ada, "ada@home.test")
            .await
            .unwrap();
        let agent = testing::agent(&db, "marco@northgate.test").await;
        let outage = testing::ticket(&db, own, None, "outage@northgate.test").await;
        let wrote = testing::ticket(&db, brightfield, Some(ada), "a@brightfield.test").await;
        testing::message_from(&db, wrote, "ada@home.test").await;
        let silent = testing::ticket(&db, brightfield, Some(ada), "b@brightfield.test").await;
        for child in [wrote, silent] {
            attach(&db, child, outage).await.unwrap();
        }

        let Err(Error::Rule(why)) =
            resolve_children(&db, &testing::sender(), outage, agent, "Fixed.").await
        else {
            panic!("a requester with several addresses was answered without a choice");
        };
        assert!(why.contains(&format!("#{silent}")), "{why}");
        assert!(!why.contains(&format!("#{wrote}")), "{why}");
        for child in [wrote, silent] {
            assert_eq!(testing::state(&db, child).await, "open", "nothing changed");
        }

        // Replying on the silent child, the agent picks the address; resolving then uses it.
        tickets::reply(
            &db,
            &testing::sender(),
            silent,
            agent,
            "Which address?",
            Some("ada@brightfield.test"),
            TicketState::Waiting,
        )
        .await
        .unwrap();
        resolve_children(&db, &testing::sender(), outage, agent, "Fixed.")
            .await
            .unwrap();
        let sent: Vec<(i64, String)> = sqlx::query_as(
            "SELECT t.number, m.to_address FROM messages m JOIN tickets t ON t.id = m.ticket_id
             WHERE m.direction = 'outbound' AND m.body_text = 'Fixed.' ORDER BY t.number",
        )
        .fetch_all(&db)
        .await
        .unwrap();
        assert_eq!(
            sent,
            [
                (wrote, "ada@home.test".to_owned()),
                (silent, "ada@brightfield.test".to_owned())
            ]
        );
    }
}
