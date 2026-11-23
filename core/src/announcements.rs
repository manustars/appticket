//! Announcements from a parent ticket: while a failure is being worked on, the help desk tells the
//! customers it affects how things stand. Rules are in docs/domain-rules.md ("Tickets", "People").

use serde::Serialize;
use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::{Error, Result, rule};
use crate::outbound::Sender;
use crate::tickets::TicketState;

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
            "the customers of the ticket's children and the users of its services have no key requesters: there is nobody to announce to",
        );
    }

    let id = Uuid::new_v4();
    let subject = format!("{} [#{parent}]", title.trim());
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
    for (customer, key) in &keys {
        sqlx::query(
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
        .await?;
        sqlx::query(
            "INSERT INTO messages (id, ticket_id, direction, rfc_message_id, from_address,
                                   from_name, to_address, author_id, subject, body_text,
                                   announcement_id)
             VALUES ($1, $2, 'outbound', $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(Uuid::new_v4())
        .bind(ticket)
        .bind(sender.new_message_id())
        .bind(&sender.address)
        .bind(&sender.name)
        .bind(&key.contact_address)
        .bind(author)
        .bind(&subject)
        .bind(body)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    let mut sent = of_ticket(db, ticket).await?;
    sent.retain(|a| a.id == id);
    Ok(sent.pop().expect("the announcement just sent"))
}

/// The announcements sent from the ticket, oldest first.
pub async fn of_ticket(db: &PgPool, ticket: Uuid) -> Result<Vec<Announcement>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: Uuid,
        author_name: String,
        subject: String,
        body: String,
        created_at: OffsetDateTime,
    }
    #[derive(sqlx::FromRow)]
    struct To {
        announcement_id: Uuid,
        #[sqlx(flatten)]
        recipient: AnnouncementRecipient,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT n.id, a.name AS author_name, n.subject, n.body, n.created_at
         FROM announcements n JOIN agents a ON a.id = n.author_id
         WHERE n.ticket_id = $1
         ORDER BY n.created_at, n.id",
    )
    .bind(ticket)
    .fetch_all(db)
    .await?;
    let mut to: Vec<To> = sqlx::query_as(
        "SELECT r.announcement_id, r.requester_id, r.name, r.address, r.customer_id,
                c.name AS customer_name, m.sent_at,
                (m.sent_at IS NULL AND m.send_attempts >= $2) AS gave_up
         FROM announcements n
         JOIN announcement_recipients r ON r.announcement_id = n.id
         JOIN customers c ON c.id = r.customer_id
         JOIN messages m ON m.announcement_id = r.announcement_id AND m.to_address = r.address
         WHERE n.ticket_id = $1
         ORDER BY c.name, r.address",
    )
    .bind(ticket)
    .bind(crate::outbound::MAX_ATTEMPTS)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let (mine, rest) = to.drain(..).partition(|t| t.announcement_id == row.id);
            to = rest;
            Announcement {
                id: row.id,
                author_name: row.author_name,
                subject: row.subject,
                body: row.body,
                created_at: row.created_at,
                recipients: mine.into_iter().map(|t: To| t.recipient).collect(),
            }
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
    async fn an_announcement_goes_to_the_users_of_the_parents_services_each_person_once(
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
    /// and so is one from a parent whose children's customers and service users have no key
    /// requesters, or that has neither children nor services.
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
    /// users of its services, also when the code is bypassed.
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
}
