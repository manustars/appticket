//! Announcements from a parent ticket: while a failure is being worked on, the help desk tells the
//! customers it affects how things stand. Rules are in docs/domain-rules.md ("Tickets", "People").

use std::collections::HashMap;

use serde::Serialize;
use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::{Error, Result, rule};
use crate::outbound::Sender;
use crate::tickets::{Outgoing, TicketState};

/// An announcement sent from a parent ticket, with when it was sent and to whom.
#[derive(Debug, Serialize, ToSchema)]
pub struct Announcement {
    pub id: Uuid,
    pub author_name: String,
    pub subject: String,
    pub body: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Who it went to, as they were when it was sent, by customer and address.
    pub recipients: Vec<AnnouncementRecipient>,
}

/// A key requester an announcement went to, each in a message of their own.
#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct AnnouncementRecipient {
    pub requester_id: Uuid,
    pub name: Option<String>,
    /// Their contact address when it was sent.
    pub address: String,
    pub customer_id: Uuid,
    pub customer_name: String,
    /// When their message left; empty while it waits in the outbox.
    #[serde(with = "time::serde::rfc3339::option")]
    pub sent_at: Option<OffsetDateTime>,
    /// The outbox stopped trying: their message will not be sent.
    pub gave_up: bool,
}

/// The customers an announcement from the ticket goes to, each once, by id: those of its children
/// and those that use the services it links, leaving out the offboarded ones, which the help desk
/// no longer serves.
async fn affected_customers(tx: &mut Transaction<'_, Postgres>, ticket: Uuid) -> Result<Vec<Uuid>> {
    Ok(sqlx::query_scalar(
        "SELECT c.id FROM customers c
         WHERE c.offboarded_at IS NULL
           AND (c.id IN (SELECT customer_id FROM tickets WHERE parent_id = $1)
                OR c.id IN (SELECT cs.customer_id FROM ticket_services ts
                            JOIN customer_services cs ON cs.service_id = ts.service_id
                            WHERE ts.ticket_id = $1))
         ORDER BY c.id",
    )
    .bind(ticket)
    .fetch_all(&mut **tx)
    .await?)
}

/// Locks `customers` against being offboarded until the transaction ends, before their key
/// requesters; returns those that still are not.
async fn lock_served(tx: &mut Transaction<'_, Postgres>, customers: &[Uuid]) -> Result<Vec<Uuid>> {
    Ok(sqlx::query_scalar(
        "SELECT id FROM customers WHERE id = ANY($1) AND offboarded_at IS NULL
         ORDER BY id FOR SHARE",
    )
    .bind(customers)
    .fetch_all(&mut **tx)
    .await?)
}

/// Sends `body` from the parent ticket `parent` to the key requesters of its children's customers
/// and of the customers that use the services it links, each person once, at their contact address
/// and in a message of their own. Neither the parent nor its children change state. Returns the
/// announcement as it is listed on the parent.
pub async fn send(
    db: &PgPool,
    sender: &Sender,
    parent: i64,
    author: Uuid,
    body: &str,
) -> Result<Announcement> {
    let body = body.trim();
    if body.is_empty() {
        return rule("an announcement cannot be empty");
    }
    let mut tx = db.begin().await?;
    // Read before any lock: customers and their key requesters are locked before the parent, and
    // who the announcement goes to must not change in between.
    let read: Option<(Uuid, String)> =
        sqlx::query_as("SELECT id, title FROM tickets WHERE number = $1")
            .bind(parent)
            .fetch_optional(&mut *tx)
            .await?;
    let (ticket, title) = read.ok_or(Error::NotFound("ticket"))?;
    let customers = affected_customers(&mut tx, ticket).await?;
    if lock_served(&mut tx, &customers).await? != customers {
        return rule("a customer has just been offboarded; reload and try again");
    }
    let keys = crate::key_requesters::locked_of_customers(&mut tx, &customers).await?;
    let (state, help_desk, child): (TicketState, bool, bool) = sqlx::query_as(
        "SELECT t.state, c.is_help_desk, t.parent_id IS NOT NULL
         FROM tickets t JOIN customers c ON c.id = t.customer_id
         WHERE t.id = $1 FOR UPDATE OF t",
    )
    .bind(ticket)
    .fetch_one(&mut *tx)
    .await?;
    if !help_desk {
        return rule(
            "announcements are sent from a parent ticket, which belongs to the help desk's own organization",
        );
    }
    if child {
        return rule("the ticket is a child: announcements are sent from its parent");
    }
    if state == TicketState::Closed {
        return rule("the ticket is closed; a closed ticket sends no announcements");
    }
    if affected_customers(&mut tx, ticket).await? != customers {
        return rule(
            "the customers the announcement goes to have just changed; reload and try again",
        );
    }
    if keys.is_empty() {
        return rule(
            "the customers of the ticket's children and the customers that use its services have no key requesters: there is nobody to announce to",
        );
    }

    let id = Uuid::new_v4();
    let subject = crate::tickets::ticket_subject(&title, parent);
    sqlx::query(
        "INSERT INTO announcements (id, ticket_id, author_id, subject, body)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(ticket)
    .bind(author)
    .bind(&subject)
    .bind(body)
    .execute(&mut *tx)
    .await?;
    let message = Outgoing::announcing(ticket, author, body, subject, id);
    for (customer, key) in &keys {
        let recorded = sqlx::query(
            "INSERT INTO announcement_recipients
                 (announcement_id, requester_id, customer_id, name, address)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(id)
        .bind(key.id)
        .bind(customer)
        .bind(&key.name)
        .bind(&key.contact_address)
        .execute(&mut *tx)
        .await;
        match recorded {
            // The check_violation the trigger raises on recipients, and no other: a child detached
            // or a customer that stopped using a service meanwhile.
            Err(sqlx::Error::Database(e))
                if e.code().as_deref() == Some("23514")
                    && e.message()
                        .contains("an announcement goes to a key requester") =>
            {
                return rule(format!(
                    "{} is no longer a key requester of a customer the failure affects; reload and try again",
                    key.contact_address
                ));
            }
            Err(e) => return Err(e.into()),
            Ok(_) => {}
        }
        message.queue(&mut tx, sender, &key.contact_address).await?;
    }
    tx.commit().await?;
    let sent = load(db, ticket, Some(id)).await?.pop();
    Ok(sent.expect("the announcement just sent"))
}

/// The announcements sent from the ticket, oldest first.
pub async fn of_ticket(db: &PgPool, ticket: Uuid) -> Result<Vec<Announcement>> {
    load(db, ticket, None).await
}

/// The announcements sent from the ticket, oldest first, or only the one with id `only`; each with
/// its recipients, by customer and address.
async fn load(db: &PgPool, ticket: Uuid, only: Option<Uuid>) -> Result<Vec<Announcement>> {
    #[derive(sqlx::FromRow)]
    struct Head {
        id: Uuid,
        author_name: String,
        subject: String,
        body: String,
        created_at: OffsetDateTime,
    }
    #[derive(sqlx::FromRow)]
    struct RecipientOf {
        announcement_id: Uuid,
        #[sqlx(flatten)]
        recipient: AnnouncementRecipient,
    }
    let heads: Vec<Head> = sqlx::query_as(
        "SELECT n.id, a.name AS author_name, n.subject, n.body, n.created_at
         FROM announcements n JOIN agents a ON a.id = n.author_id
         WHERE n.ticket_id = $1 AND ($2::uuid IS NULL OR n.id = $2)
         ORDER BY n.created_at, n.id",
    )
    .bind(ticket)
    .bind(only)
    .fetch_all(db)
    .await?;
    let recipients: Vec<RecipientOf> = sqlx::query_as(
        "SELECT r.announcement_id, r.requester_id, r.name, r.address, r.customer_id,
                c.name AS customer_name, m.sent_at, message_gave_up(m, $3) AS gave_up
         FROM announcements n
         JOIN announcement_recipients r ON r.announcement_id = n.id
         JOIN customers c ON c.id = r.customer_id
         JOIN messages m ON m.announcement_id = r.announcement_id AND m.to_address = r.address
         WHERE n.ticket_id = $1 AND ($2::uuid IS NULL OR n.id = $2)
         ORDER BY c.name, r.address",
    )
    .bind(ticket)
    .bind(only)
    .bind(crate::outbound::MAX_ATTEMPTS)
    .fetch_all(db)
    .await?;
    let mut by_announcement: HashMap<Uuid, Vec<AnnouncementRecipient>> = HashMap::new();
    for row in recipients {
        by_announcement
            .entry(row.announcement_id)
            .or_default()
            .push(row.recipient);
    }
    Ok(heads
        .into_iter()
        .map(|head| Announcement {
            recipients: by_announcement.remove(&head.id).unwrap_or_default(),
            id: head.id,
            author_name: head.author_name,
            subject: head.subject,
            body: head.body,
            created_at: head.created_at,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parents::attach;
    use crate::testing;
    use crate::tickets::{self, TimelineEntry};

    /// The announcements listed on the ticket, oldest first.
    async fn listed(db: &PgPool, number: i64) -> Vec<Announcement> {
        tickets::detail(db, number)
            .await
            .unwrap()
            .timeline
            .into_iter()
            .filter_map(|entry| match entry {
                TimelineEntry::Announcement(a) => Some(a),
                _ => None,
            })
            .collect()
    }

    /// Who an announcement went to: their customer's name and address.
    fn to_whom(announcement: &Announcement) -> Vec<(&str, &str)> {
        announcement
            .recipients
            .iter()
            .map(|r| (r.customer_name.as_str(), r.address.as_str()))
            .collect()
    }

    /// An announcement goes to the key requesters of the parent's children's customers, not to
    /// their other requesters, and is listed on the parent with its recipients.
    #[sqlx::test]
    async fn an_announcement_goes_to_the_key_requesters_of_the_childrens_customers(db: PgPool) {
        let own = testing::help_desk(&db).await;
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let lena = testing::requester(&db, brightfield, "lena@brightfield.test").await;
        let ivo = testing::requester(&db, harbor, "ivo@harbor.test").await;
        for key in [ada, ivo] {
            crate::key_requesters::mark(&db, key, None).await.unwrap();
        }
        let agent = testing::agent(&db, "marco@northgate.test").await;
        let outage = testing::ticket(&db, own, None, "outage@northgate.test").await;
        let a = testing::ticket(&db, brightfield, Some(lena), "a@brightfield.test").await;
        let b = testing::ticket(&db, harbor, None, "b@harbor.test").await;
        for child in [a, b] {
            attach(&db, child, outage).await.unwrap();
        }

        let sent = send(
            &db,
            &testing::sender(),
            outage,
            agent,
            "Mail is down; we are on it.",
        )
        .await
        .unwrap();
        assert_eq!(
            to_whom(&sent),
            [
                ("Brightfield", "ada@brightfield.test"),
                ("Harbor Lane", "ivo@harbor.test")
            ]
        );
        let listed = listed(&db, outage).await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].body, "Mail is down; we are on it.");
        assert_eq!(listed[0].author_name, "marco");
        assert_eq!(to_whom(&listed[0]), to_whom(&sent));
    }

    /// An announcement also goes to the key requesters of the customers that use the services the
    /// parent links, the help desk's own organization among them; each person gets one message,
    /// however many children their customer has and whether it also uses the services.
    #[sqlx::test]
    async fn an_announcement_goes_to_the_customers_that_use_the_parents_services_each_person_once(
        db: PgPool,
    ) {
        let own = testing::help_desk(&db).await;
        let brightfield = testing::customer(&db, "Brightfield").await;
        let delta = testing::customer(&db, "Delta Works").await;
        let echo = testing::customer(&db, "Echo Farms").await;
        let mail = crate::catalog::create_service(&db, "Hosted mail", "")
            .await
            .unwrap();
        let backup = crate::catalog::create_service(&db, "Backup", "")
            .await
            .unwrap();
        for (service, customer) in [(mail, brightfield), (mail, delta), (backup, echo)] {
            crate::catalog::use_service(&db, service, customer)
                .await
                .unwrap();
        }
        let mut people = Vec::new();
        for (customer, address) in [
            (own, "sara@northgate.test"),
            (brightfield, "ada@brightfield.test"),
            (delta, "nora@delta.test"),
            (echo, "eva@echo.test"),
        ] {
            let key = testing::requester(&db, customer, address).await;
            crate::key_requesters::mark(&db, key, None).await.unwrap();
            people.push(key);
        }
        let agent = testing::agent(&db, "marco@northgate.test").await;
        let outage = testing::ticket(&db, own, None, "outage@northgate.test").await;
        crate::catalog::link_service(&db, outage, mail, true)
            .await
            .unwrap();
        for id in ["a1@brightfield.test", "a2@brightfield.test"] {
            let child = testing::ticket(&db, brightfield, None, id).await;
            attach(&db, child, outage).await.unwrap();
        }

        let sent = send(&db, &testing::sender(), outage, agent, "Mail is back.")
            .await
            .unwrap();
        assert_eq!(
            to_whom(&sent),
            [
                ("Brightfield", "ada@brightfield.test"),
                ("Delta Works", "nora@delta.test"),
                ("Northgate", "sara@northgate.test")
            ]
        );
        let queued: Vec<String> = sqlx::query_scalar(
            "SELECT to_address FROM messages WHERE direction = 'outbound' ORDER BY to_address",
        )
        .fetch_all(&db)
        .await
        .unwrap();
        assert_eq!(
            queued,
            [
                "ada@brightfield.test",
                "nora@delta.test",
                "sara@northgate.test"
            ],
            "one message each"
        );
    }

    /// Only a parent ticket that is not closed sends announcements: a closed one, a ticket of
    /// another customer than the help desk's own organization and a child are refused, and nothing
    /// is sent.
    #[sqlx::test]
    async fn announcements_are_sent_only_from_a_parent_that_is_not_closed(db: PgPool) {
        let own = testing::help_desk(&db).await;
        let brightfield = testing::customer(&db, "Brightfield").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        crate::key_requesters::mark(&db, ada, None).await.unwrap();
        let agent = testing::agent(&db, "marco@northgate.test").await;
        let outage = testing::ticket(&db, own, None, "outage@northgate.test").await;
        let child = testing::ticket(&db, brightfield, None, "a@brightfield.test").await;
        attach(&db, child, outage).await.unwrap();
        let other = testing::ticket(&db, brightfield, None, "b@brightfield.test").await;
        let sender = testing::sender();
        let refused = |number| send(&db, &sender, number, agent, "Still down.");

        let Err(Error::Rule(why)) = refused(child).await else {
            panic!("announced from a child");
        };
        assert!(why.contains("parent"), "{why}");
        let Err(Error::Rule(why)) = refused(other).await else {
            panic!("announced from a ticket of a customer");
        };
        assert!(why.contains("help desk's own organization"), "{why}");
        tickets::set_state(&db, outage, tickets::TicketState::Closed)
            .await
            .unwrap();
        let Err(Error::Rule(why)) = refused(outage).await else {
            panic!("announced from a closed parent");
        };
        assert!(why.contains("closed"), "{why}");
        for number in [outage, child, other] {
            assert!(listed(&db, number).await.is_empty());
        }
        let queued: i64 =
            sqlx::query_scalar("SELECT count(*) FROM messages WHERE direction = 'outbound'")
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(queued, 0);
    }

    /// Announcements are sent as often as needed, from a parent in any state but closed, and
    /// change the state of neither the parent nor its children; a closed child still counts. Each
    /// stays listed as it was sent, whatever happens to its recipients afterwards.
    #[sqlx::test]
    async fn announcements_change_no_state_and_stay_as_sent(db: PgPool) {
        let own = testing::help_desk(&db).await;
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let delta = testing::customer(&db, "Delta Works").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        crate::requesters::add_address(&db, ada, "ada@home.test")
            .await
            .unwrap();
        crate::key_requesters::mark(&db, ada, Some("ada@brightfield.test"))
            .await
            .unwrap();
        let ivo = testing::requester(&db, harbor, "ivo@harbor.test").await;
        let nora = testing::requester(&db, delta, "nora@delta.test").await;
        for key in [ivo, nora] {
            crate::key_requesters::mark(&db, key, None).await.unwrap();
        }
        let agent = testing::agent(&db, "marco@northgate.test").await;
        let outage = testing::ticket(&db, own, None, "outage@northgate.test").await;
        let mut children = Vec::new();
        for (customer, id, state) in [
            (brightfield, "a@brightfield.test", TicketState::Waiting),
            (harbor, "b@harbor.test", TicketState::Resolved),
            (delta, "d@delta.test", TicketState::Closed),
        ] {
            let child = testing::ticket(&db, customer, None, id).await;
            attach(&db, child, outage).await.unwrap();
            tickets::set_state(&db, child, state).await.unwrap();
            children.push(child);
        }
        tickets::set_state(&db, outage, TicketState::Resolved)
            .await
            .unwrap();
        let sender = testing::sender();

        send(&db, &sender, outage, agent, "Mail is down.")
            .await
            .unwrap();
        crate::key_requesters::mark(&db, ada, Some("ada@home.test"))
            .await
            .unwrap();
        crate::key_requesters::unmark(&db, ivo).await.unwrap();
        send(&db, &sender, outage, agent, "Mail is back.")
            .await
            .unwrap();

        let listed = listed(&db, outage).await;
        let bodies: Vec<&str> = listed.iter().map(|a| a.body.as_str()).collect();
        assert_eq!(bodies, ["Mail is down.", "Mail is back."]);
        assert_eq!(
            to_whom(&listed[0]),
            [
                ("Brightfield", "ada@brightfield.test"),
                ("Delta Works", "nora@delta.test"),
                ("Harbor Lane", "ivo@harbor.test")
            ]
        );
        assert_eq!(
            to_whom(&listed[1]),
            [
                ("Brightfield", "ada@home.test"),
                ("Delta Works", "nora@delta.test")
            ]
        );
        assert_eq!(testing::state(&db, outage).await, "resolved");
        let states = [
            testing::state(&db, children[0]).await,
            testing::state(&db, children[1]).await,
            testing::state(&db, children[2]).await,
        ];
        assert_eq!(states, ["waiting", "resolved", "closed"]);
    }

    /// Announcements stay out of the parent's own conversation with its requester: sending one to
    /// the requester's contact address as a key requester picks no address for replies there, and
    /// a reply there neither answers nor refers to an announcement.
    #[sqlx::test]
    async fn announcements_stay_out_of_the_parents_conversation(db: PgPool) {
        let own = testing::help_desk(&db).await;
        let sara = testing::requester(&db, own, "sara@northgate.test").await;
        crate::requesters::add_address(&db, sara, "sara@home.test")
            .await
            .unwrap();
        crate::key_requesters::mark(&db, sara, Some("sara@home.test"))
            .await
            .unwrap();
        let mail = crate::catalog::create_service(&db, "Hosted mail", "")
            .await
            .unwrap();
        let agent = testing::agent(&db, "marco@northgate.test").await;
        let outage = testing::ticket(&db, own, Some(sara), "outage@northgate.test").await;
        crate::catalog::link_service(&db, outage, mail, true)
            .await
            .unwrap();
        let sender = testing::sender();

        send(&db, &sender, outage, agent, "Mail is down.")
            .await
            .unwrap();
        let ticket = tickets::detail(&db, outage).await.unwrap();
        assert_eq!(ticket.requester.unwrap().last_picked, None);
        let Err(Error::Rule(_)) = tickets::reply(
            &db,
            &sender,
            outage,
            agent,
            "Which address?",
            None,
            TicketState::Waiting,
        )
        .await
        else {
            panic!("the announcement picked the address of a reply");
        };
        tickets::reply(
            &db,
            &sender,
            outage,
            agent,
            "We are on it.",
            Some("sara@northgate.test"),
            TicketState::Waiting,
        )
        .await
        .unwrap();
        let (in_reply_to, references): (Option<String>, Vec<String>) = sqlx::query_as(
            r#"SELECT in_reply_to, "references" FROM messages
               WHERE direction = 'outbound' AND announcement_id IS NULL"#,
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(in_reply_to.as_deref(), Some("outage@northgate.test"));
        assert_eq!(references, ["outage@northgate.test"]);
    }

    /// A key requester of another customer who answers an announcement writes back to the parent,
    /// as any reply comes back to its ticket, and reopens it when resolved. They are not the
    /// parent's requester nor a key requester of its customer, so agents do not reply to them from
    /// the parent: a reply there goes to the help desk's own key requesters.
    #[sqlx::test]
    async fn an_answer_to_an_announcement_comes_back_to_the_parent(db: PgPool) {
        let own = testing::help_desk(&db).await;
        let brightfield = testing::customer(&db, "Brightfield").await;
        let sara = testing::requester(&db, own, "sara@northgate.test").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        for key in [sara, ada] {
            crate::key_requesters::mark(&db, key, None).await.unwrap();
        }
        let agent = testing::agent(&db, "marco@northgate.test").await;
        let outage = testing::ticket(&db, own, None, "outage@northgate.test").await;
        let child = testing::ticket(&db, brightfield, None, "a@brightfield.test").await;
        attach(&db, child, outage).await.unwrap();
        let sender = testing::sender();
        let sent = send(&db, &sender, outage, agent, "Mail is down.")
            .await
            .unwrap();
        tickets::set_state(&db, outage, TicketState::Resolved)
            .await
            .unwrap();
        let announced: String = sqlx::query_scalar(
            "SELECT rfc_message_id FROM messages WHERE to_address = 'ada@brightfield.test'",
        )
        .fetch_one(&db)
        .await
        .unwrap();

        let answer = crate::mail::Incoming {
            rfc_message_id: Some("answer@brightfield.test".into()),
            in_reply_to: vec![announced],
            references: Vec::new(),
            from_address: "ada@brightfield.test".into(),
            from_name: Some("Ada Ferrini".into()),
            subject: format!("Re: {}", sent.subject),
            body_text: "Our office has no mail either.".into(),
            raw: Vec::new(),
        };
        let mut tx = db.begin().await.unwrap();
        let routed = crate::mail::route(&mut tx, &answer).await.unwrap();
        tx.commit().await.unwrap();
        let ticket = tickets::detail(&db, outage).await.unwrap();
        assert_eq!(
            routed,
            crate::mail::Routed::Reply {
                ticket_id: ticket.id
            }
        );
        assert_eq!(ticket.state, TicketState::Open, "the answer reopens it");
        assert!(
            ticket.requester.is_none(),
            "she does not become its requester"
        );

        tickets::reply(
            &db,
            &sender,
            outage,
            agent,
            "Brightfield is affected too.",
            None,
            TicketState::Waiting,
        )
        .await
        .unwrap();
        let replied: Vec<String> = sqlx::query_scalar(
            "SELECT to_address FROM messages
             WHERE direction = 'outbound' AND announcement_id IS NULL",
        )
        .fetch_all(&db)
        .await
        .unwrap();
        assert_eq!(replied, ["sara@northgate.test"]);
    }

    /// An offboarded customer is no longer served: an announcement does not go to its key
    /// requesters, whether it has a child of the parent or uses one of its services.
    #[sqlx::test]
    async fn an_announcement_skips_offboarded_customers(db: PgPool) {
        let own = testing::help_desk(&db).await;
        let brightfield = testing::customer(&db, "Brightfield").await;
        let delta = testing::customer(&db, "Delta Works").await;
        let echo = testing::customer(&db, "Echo Farms").await;
        let mail = crate::catalog::create_service(&db, "Hosted mail", "")
            .await
            .unwrap();
        for customer in [brightfield, echo] {
            crate::catalog::use_service(&db, mail, customer)
                .await
                .unwrap();
        }
        for (customer, address) in [
            (brightfield, "ada@brightfield.test"),
            (delta, "nora@delta.test"),
            (echo, "eva@echo.test"),
        ] {
            let key = testing::requester(&db, customer, address).await;
            crate::key_requesters::mark(&db, key, None).await.unwrap();
        }
        let agent = testing::agent(&db, "marco@northgate.test").await;
        let outage = testing::ticket(&db, own, None, "outage@northgate.test").await;
        crate::catalog::link_service(&db, outage, mail, true)
            .await
            .unwrap();
        let child = testing::ticket(&db, delta, None, "d@delta.test").await;
        attach(&db, child, outage).await.unwrap();
        for customer in [delta, echo] {
            crate::customers::offboard(&db, customer).await.unwrap();
        }

        let sent = send(&db, &testing::sender(), outage, agent, "Mail is down.")
            .await
            .unwrap();
        assert_eq!(to_whom(&sent), [("Brightfield", "ada@brightfield.test")]);
    }

    /// An announcement needs something to say and someone to say it to: an empty one is refused,
    /// and so is one from a parent whose children's customers and the customers that use its
    /// services have no key requesters, or that has neither children nor services.
    #[sqlx::test]
    async fn an_announcement_to_nobody_is_refused(db: PgPool) {
        let own = testing::help_desk(&db).await;
        let brightfield = testing::customer(&db, "Brightfield").await;
        let agent = testing::agent(&db, "marco@northgate.test").await;
        let alone = testing::ticket(&db, own, None, "alone@northgate.test").await;
        let outage = testing::ticket(&db, own, None, "outage@northgate.test").await;
        let child = testing::ticket(&db, brightfield, None, "a@brightfield.test").await;
        attach(&db, child, outage).await.unwrap();
        let sender = testing::sender();

        for number in [alone, outage] {
            let Err(Error::Rule(why)) = send(&db, &sender, number, agent, "Still down.").await
            else {
                panic!("announced to nobody");
            };
            assert!(why.contains("no key requesters"), "{why}");
            assert!(listed(&db, number).await.is_empty());
        }
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        crate::key_requesters::mark(&db, ada, None).await.unwrap();
        let Err(Error::Rule(_)) = send(&db, &sender, outage, agent, "  ").await else {
            panic!("an empty announcement");
        };
        send(&db, &sender, outage, agent, "Still down.")
            .await
            .unwrap();
    }

    /// Writes an announcement from ticket `number` straight to the database, to each requester of
    /// `to` at the address given; returns its id, or the database's refusal.
    async fn write(
        db: &PgPool,
        number: i64,
        to: &[(Uuid, &str)],
    ) -> std::result::Result<Uuid, String> {
        let mut tx = db.begin().await.unwrap();
        let id = Uuid::new_v4();
        let wrote = async {
            sqlx::query(
                "INSERT INTO announcements (id, ticket_id, author_id, subject, body)
                 SELECT $1, t.id, (SELECT id FROM agents LIMIT 1), 'Outage', 'Still down.'
                 FROM tickets t WHERE t.number = $2",
            )
            .bind(id)
            .bind(number)
            .execute(&mut *tx)
            .await?;
            for (requester, address) in to {
                sqlx::query(
                    "INSERT INTO announcement_recipients
                         (announcement_id, requester_id, customer_id, address)
                     SELECT $1, id, customer_id, $3 FROM requesters WHERE id = $2",
                )
                .bind(id)
                .bind(requester)
                .bind(address)
                .execute(&mut *tx)
                .await?;
            }
            tx.commit().await.map(|_| id)
        };
        wrote.await.map_err(|e| e.to_string())
    }

    /// The database refuses an announcement from a closed ticket, from a child and from a
    /// ticket of a customer, also when the code is bypassed.
    #[sqlx::test]
    async fn the_database_keeps_announcements_from_parents_that_are_not_closed(db: PgPool) {
        let own = testing::help_desk(&db).await;
        let brightfield = testing::customer(&db, "Brightfield").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        crate::key_requesters::mark(&db, ada, None).await.unwrap();
        testing::agent(&db, "marco@northgate.test").await;
        let outage = testing::ticket(&db, own, None, "outage@northgate.test").await;
        let child = testing::ticket(&db, brightfield, None, "a@brightfield.test").await;
        attach(&db, child, outage).await.unwrap();
        let other = testing::ticket(&db, brightfield, None, "b@brightfield.test").await;
        let to = &[(ada, "ada@brightfield.test")];

        let why = write(&db, child, to).await.unwrap_err();
        assert!(why.contains("from a parent ticket"), "{why}");
        let why = write(&db, other, to).await.unwrap_err();
        assert!(why.contains("from a parent ticket"), "{why}");
        write(&db, outage, to).await.unwrap();
        tickets::set_state(&db, outage, tickets::TicketState::Closed)
            .await
            .unwrap();
        let why = write(&db, outage, to).await.unwrap_err();
        assert!(why.contains("closed"), "{why}");
    }

    /// The database keeps an announcement going to someone, each person once, and only to key
    /// requesters at their contact address, of the customers of the parent's children and of the
    /// customers that use the parent's services, also when the code is bypassed.
    #[sqlx::test]
    async fn the_database_keeps_who_an_announcement_goes_to(db: PgPool) {
        let own = testing::help_desk(&db).await;
        let brightfield = testing::customer(&db, "Brightfield").await;
        let delta = testing::customer(&db, "Delta Works").await;
        let echo = testing::customer(&db, "Echo Farms").await;
        let mail = crate::catalog::create_service(&db, "Hosted mail", "")
            .await
            .unwrap();
        let foxglen = testing::customer(&db, "Foxglen").await;
        for customer in [delta, foxglen] {
            crate::catalog::use_service(&db, mail, customer)
                .await
                .unwrap();
        }
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        crate::requesters::add_address(&db, ada, "ada@home.test")
            .await
            .unwrap();
        let lena = testing::requester(&db, brightfield, "lena@brightfield.test").await;
        let nora = testing::requester(&db, delta, "nora@delta.test").await;
        let eva = testing::requester(&db, echo, "eva@echo.test").await;
        let finn = testing::requester(&db, foxglen, "finn@foxglen.test").await;
        crate::key_requesters::mark(&db, ada, Some("ada@brightfield.test"))
            .await
            .unwrap();
        for key in [nora, eva, finn] {
            crate::key_requesters::mark(&db, key, None).await.unwrap();
        }
        crate::customers::offboard(&db, foxglen).await.unwrap();
        testing::agent(&db, "marco@northgate.test").await;
        let outage = testing::ticket(&db, own, None, "outage@northgate.test").await;
        crate::catalog::link_service(&db, outage, mail, true)
            .await
            .unwrap();
        let child = testing::ticket(&db, brightfield, None, "a@brightfield.test").await;
        attach(&db, child, outage).await.unwrap();

        let why = write(&db, outage, &[]).await.unwrap_err();
        assert!(why.contains("goes to someone"), "{why}");
        for (to, what) in [
            ((lena, "lena@brightfield.test"), "not a key requester"),
            ((ada, "ada@home.test"), "not their contact address"),
            (
                (eva, "eva@echo.test"),
                "a customer the failure does not affect",
            ),
            ((finn, "finn@foxglen.test"), "an offboarded customer"),
        ] {
            let why = write(&db, outage, &[to]).await.unwrap_err();
            assert!(why.contains("a key requester"), "{what}: {why}");
        }
        let why = write(
            &db,
            outage,
            &[(ada, "ada@brightfield.test"), (ada, "ada@brightfield.test")],
        )
        .await
        .unwrap_err();
        assert!(why.contains("announcement_recipients_pkey"), "{why}");
        let sent = write(
            &db,
            outage,
            &[(ada, "ada@brightfield.test"), (nora, "nora@delta.test")],
        )
        .await
        .unwrap();

        // Its messages go to its recipients, on the parent it was sent from.
        let message = |number: i64, to: &'static str| {
            sqlx::query(
                "INSERT INTO messages (id, ticket_id, direction, from_address, to_address,
                                       author_id, subject, announcement_id)
                 SELECT $1, t.id, 'outbound', 'helpdesk@helpdesk.test', $3,
                        (SELECT id FROM agents LIMIT 1), 'Outage', $4
                 FROM tickets t WHERE t.number = $2",
            )
            .bind(Uuid::new_v4())
            .bind(number)
            .bind(to)
            .bind(sent)
            .execute(&db)
        };
        let why = message(outage, "lena@brightfield.test")
            .await
            .unwrap_err()
            .to_string();
        assert!(
            why.contains("messages_to_an_announcement_recipient"),
            "{why}"
        );
        let why = message(child, "ada@brightfield.test")
            .await
            .unwrap_err()
            .to_string();
        assert!(why.contains("on the ticket it was sent from"), "{why}");
        message(outage, "ada@brightfield.test").await.unwrap();
        let why = message(outage, "ada@brightfield.test")
            .await
            .unwrap_err()
            .to_string();
        assert!(
            why.contains("messages_one_per_announcement_recipient"),
            "{why}"
        );
    }

    /// An announcement stays as it was sent, also when the code is bypassed: neither it nor its
    /// recipients are changed or removed afterwards, a recipient's merge or departure included.
    #[sqlx::test]
    async fn the_database_keeps_an_announcement_as_it_was_sent(db: PgPool) {
        let own = testing::help_desk(&db).await;
        let brightfield = testing::customer(&db, "Brightfield").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let lena = testing::requester(&db, brightfield, "lena@brightfield.test").await;
        crate::key_requesters::mark(&db, ada, None).await.unwrap();
        let agent = testing::agent(&db, "marco@northgate.test").await;
        let outage = testing::ticket(&db, own, None, "outage@northgate.test").await;
        let child = testing::ticket(&db, brightfield, None, "a@brightfield.test").await;
        attach(&db, child, outage).await.unwrap();
        let sent = send(&db, &testing::sender(), outage, agent, "Mail is down.")
            .await
            .unwrap();

        for change in [
            "UPDATE announcements SET body = 'Changed.' WHERE id = $1",
            "UPDATE announcements SET ticket_id = (SELECT id FROM tickets WHERE number = 2)
             WHERE id = $1",
            "UPDATE announcement_recipients SET name = 'Someone' WHERE announcement_id = $1",
            "UPDATE announcement_recipients SET address = 'lena@brightfield.test'
             WHERE announcement_id = $1",
            "DELETE FROM announcement_recipients WHERE announcement_id = $1",
            "DELETE FROM announcements WHERE id = $1",
        ] {
            let why = sqlx::query(change)
                .bind(sent.id)
                .execute(&db)
                .await
                .expect_err(change)
                .to_string();
            assert!(why.contains("stays as it was sent"), "{change}: {why}");
        }

        // The recipient is merged away and the one that remains departs: the list stays.
        crate::key_requesters::unmark(&db, ada).await.unwrap();
        crate::requesters::merge(&db, lena, ada).await.unwrap();
        crate::requesters::depart(&db, lena).await.unwrap();
        let listed = listed(&db, outage).await;
        assert_eq!(
            to_whom(&listed[0]),
            [("Brightfield", "ada@brightfield.test")]
        );
        assert_eq!(listed[0].recipients[0].requester_id, ada);
    }
}
