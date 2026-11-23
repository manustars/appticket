//! Parent and child tickets: one failure that affects several customers is tracked by a parent
//! ticket of the help desk's own organization, and each customer's ticket about it is a child.

use std::collections::{BTreeMap, HashSet};

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
    /// How many people the message was queued to, one message each: the children's requesters and
    /// the key requesters of the children's customers.
    pub people_written_to: i64,
}

/// A child being resolved.
#[derive(Debug, PartialEq, Eq, sqlx::FromRow)]
struct Child {
    number: i64,
    id: Uuid,
    customer_id: Uuid,
    requester_id: Option<Uuid>,
}

/// The parent's children that are open or waiting, by number; locked when `lock` is set.
async fn open_children(
    tx: &mut Transaction<'_, Postgres>,
    parent: i64,
    lock: bool,
) -> Result<Vec<Child>> {
    let sql = if lock {
        "SELECT t.number, t.id, t.customer_id, t.requester_id
         FROM tickets t
         JOIN tickets p ON p.id = t.parent_id
         WHERE p.number = $1 AND t.state IN ('open', 'waiting')
         ORDER BY t.number
         FOR UPDATE OF t"
    } else {
        "SELECT t.number, t.id, t.customer_id, t.requester_id
         FROM tickets t
         JOIN tickets p ON p.id = t.parent_id
         WHERE p.number = $1 AND t.state IN ('open', 'waiting')
         ORDER BY t.number"
    };
    Ok(sqlx::query_as(sql)
        .bind(parent)
        .fetch_all(&mut **tx)
        .await?)
}

/// Resolves every child that is still open or waiting, sending `body` to each child's requester
/// and to the key requesters of each child's customer. Each person gets one message of their own:
/// a requester on the first of their children where their address is known, at the address a reply
/// there goes to; a key requester who is not one of those requesters on a child of their customer,
/// one without a requester first, at their contact address. A child with neither, or whose
/// departed requester has no address left, is only resolved. The parent itself is not changed.
pub async fn resolve_children(
    db: &PgPool,
    sender: &Sender,
    parent: i64,
    author: Uuid,
    body: &str,
) -> Result<ResolvedChildren> {
    let body = body.trim();
    if body.is_empty() {
        return rule("the message to the requesters cannot be empty");
    }
    let mut tx = db.begin().await?;
    // Read before any lock: the key requesters of the children's customers are locked before the
    // children, and the children must not change in between.
    let children = open_children(&mut tx, parent, false).await?;
    if children.is_empty() {
        return rule("the ticket has no open or waiting children");
    }
    let mut customers: Vec<Uuid> = children.iter().map(|c| c.customer_id).collect();
    let mut seen = HashSet::new();
    customers.retain(|c| seen.insert(*c));
    let keys = crate::key_requesters::locked_of_customers(&mut tx, &customers).await?;
    if open_children(&mut tx, parent, true).await? != children {
        return rule("the children have just changed; reload and try again");
    }

    // Who is written to, and on which child.
    let mut written: HashSet<Uuid> = HashSet::new();
    let mut messages: Vec<(Uuid, String)> = Vec::new();
    // A reply goes where its requester last wrote from on the child, else where an agent's last
    // reply there went. A requester with several addresses and neither on all their children needs
    // an agent to pick one: refuse before changing anything.
    let mut undecided: BTreeMap<Uuid, Vec<i64>> = BTreeMap::new();
    for child in &children {
        let Some(requester) = child.requester_id else {
            continue;
        };
        if written.contains(&requester) {
            continue;
        }
        let Some(recipient) = crate::requesters::recipient(&mut tx, child.id).await? else {
            continue;
        };
        if recipient.addresses.is_empty() {
            continue;
        }
        match recipient.choose(None) {
            Ok(to) => {
                written.insert(requester);
                undecided.remove(&requester);
                messages.push((child.id, to));
            }
            Err(_) => undecided.entry(requester).or_default().push(child.number),
        }
    }
    if !undecided.is_empty() {
        let numbers: Vec<String> = undecided
            .into_values()
            .flatten()
            .map(|n| format!("#{n}"))
            .collect();
        return rule(format!(
            "the requesters of {} have several addresses and have not written on their ticket: reply to them there first, picking the address",
            numbers.join(", ")
        ));
    }
    for customer in customers {
        let on = children
            .iter()
            .filter(|c| c.customer_id == customer)
            .min_by_key(|c| (c.requester_id.is_some(), c.number))
            .map(|c| c.id)
            .expect("a child of the customer");
        for (_, key) in keys.iter().filter(|(c, _)| *c == customer) {
            if written.insert(key.id) {
                messages.push((on, key.contact_address.clone()));
            }
        }
    }

    // The children written on, in order, each with its recipients.
    let mut on_child: Vec<(Uuid, Vec<String>)> = Vec::new();
    for (ticket, to) in &messages {
        match on_child.iter_mut().find(|(t, _)| t == ticket) {
            Some((_, recipients)) => recipients.push(to.clone()),
            None => on_child.push((*ticket, vec![to.clone()])),
        }
    }
    for (ticket, recipients) in &on_child {
        tickets::send(
            &mut tx,
            sender,
            *ticket,
            author,
            body,
            recipients,
            TicketState::Resolved,
        )
        .await?;
    }
    let ids: Vec<Uuid> = children.iter().map(|c| c.id).collect();
    sqlx::query(
        "UPDATE tickets SET state = $2, updated_at = now() WHERE id = ANY($1) AND state <> $2",
    )
    .bind(&ids)
    .bind(TicketState::Resolved)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(ResolvedChildren {
        resolved: children.len() as i64,
        people_written_to: messages.len() as i64,
    })
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
        assert_eq!((done.resolved, done.people_written_to), (3, 2));
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

    /// The messages queued on the parent's children: on which child, to whom, in that order.
    async fn sent(db: &PgPool, children: &[i64]) -> Vec<(i64, String)> {
        let mut sent = Vec::new();
        for &child in children {
            for entry in tickets::detail(db, child).await.unwrap().timeline {
                if let tickets::TimelineEntry::Message(m) = entry
                    && m.direction == tickets::Direction::Outbound
                {
                    sent.push((child, m.to_address.unwrap()));
                }
            }
        }
        sent.sort();
        sent
    }

    /// Resolving the children writes to each child's requester and to the key requesters of each
    /// child's customer, each person once and in a message of their own: on their own ticket for a
    /// requester, else on a ticket of their customer, one without a requester first. A child with
    /// neither is only resolved.
    #[sqlx::test]
    async fn resolving_children_writes_to_each_person_once(db: PgPool) {
        let own = help_desk(&db).await;
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let delta = testing::customer(&db, "Delta Works").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let lena = testing::requester(&db, brightfield, "lena@brightfield.test").await;
        let ivo = testing::requester(&db, harbor, "ivo@harbor.test").await;
        let nora = testing::requester(&db, harbor, "nora@harbor.test").await;
        for key in [ada, lena, nora] {
            crate::key_requesters::mark(&db, key, None).await.unwrap();
        }
        let agent = testing::agent(&db, "marco@northgate.test").await;
        let outage = testing::ticket(&db, own, None, "outage@northgate.test").await;
        let a1 = testing::ticket(&db, brightfield, Some(ada), "a1@brightfield.test").await;
        let a2 = testing::ticket(&db, brightfield, Some(ada), "a2@brightfield.test").await;
        let b = testing::ticket(&db, harbor, Some(ivo), "b@harbor.test").await;
        let c = testing::ticket(&db, harbor, None, "c@harbor.test").await;
        let d = testing::ticket(&db, delta, None, "d@delta.test").await;
        let children = [a1, a2, b, c, d];
        for child in children {
            attach(&db, child, outage).await.unwrap();
        }

        let done = resolve_children(&db, &testing::sender(), outage, agent, "Mail works again.")
            .await
            .unwrap();
        assert_eq!((done.resolved, done.people_written_to), (5, 4));
        assert_eq!(
            sent(&db, &children).await,
            [
                (a1, "ada@brightfield.test".to_owned()),
                (a1, "lena@brightfield.test".to_owned()),
                (b, "ivo@harbor.test".to_owned()),
                (c, "nora@harbor.test".to_owned()),
            ]
        );
        for child in children {
            assert_eq!(testing::state(&db, child).await, "resolved");
        }
        assert_eq!(testing::state(&db, outage).await, "open");
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
        assert_eq!((done.resolved, done.people_written_to), (1, 0));
        assert_eq!(testing::state(&db, child).await, "resolved");
    }

    /// A requester is written to once, on the first of their children where their address is
    /// known. One whose address is known on none of them, having several, needs an agent to pick
    /// it: resolving is refused, naming their children, and nothing changes.
    #[sqlx::test]
    async fn children_are_answered_where_their_requesters_wrote_from(db: PgPool) {
        let own = help_desk(&db).await;
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let ivo = testing::requester(&db, harbor, "ivo@harbor.test").await;
        for (requester, address) in [(ada, "ada@home.test"), (ivo, "ivo@home.test")] {
            crate::requesters::add_address(&db, requester, address)
                .await
                .unwrap();
        }
        let agent = testing::agent(&db, "marco@northgate.test").await;
        let outage = testing::ticket(&db, own, None, "outage@northgate.test").await;
        let silent = testing::ticket(&db, brightfield, Some(ada), "a@brightfield.test").await;
        let wrote = testing::ticket(&db, brightfield, Some(ada), "b@brightfield.test").await;
        testing::message_from(&db, wrote, "ada@home.test").await;
        let his = testing::ticket(&db, harbor, Some(ivo), "c@harbor.test").await;
        let children = [silent, wrote, his];
        for child in children {
            attach(&db, child, outage).await.unwrap();
        }

        let Err(Error::Rule(why)) =
            resolve_children(&db, &testing::sender(), outage, agent, "Fixed.").await
        else {
            panic!("a requester with several addresses was answered without a choice");
        };
        assert!(why.contains(&format!("#{his}")), "{why}");
        assert!(!why.contains(&format!("#{silent}")), "{why}");
        for child in children {
            assert_eq!(testing::state(&db, child).await, "open", "nothing changed");
        }

        // Replying on his ticket, the agent picks the address; resolving then uses it too.
        tickets::reply(
            &db,
            &testing::sender(),
            his,
            agent,
            "Which address?",
            Some("ivo@harbor.test"),
            TicketState::Waiting,
        )
        .await
        .unwrap();
        resolve_children(&db, &testing::sender(), outage, agent, "Fixed.")
            .await
            .unwrap();
        assert_eq!(
            sent(&db, &children).await,
            [
                (wrote, "ada@home.test".to_owned()),
                (his, "ivo@harbor.test".to_owned()),
                (his, "ivo@harbor.test".to_owned())
            ],
            "the agent's question and the message"
        );
    }
}
