//! What agents do with a message waiting in the inbox: open a ticket for it, attach it to an
//! existing ticket, or discard it.

use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::{Error, Result, rule};

/// The message's sender and subject, locked, if it is still waiting in the inbox.
/// A message waiting in the inbox: sender's address and name, and subject.
struct Waiting {
    from: String,
    from_name: Option<String>,
    subject: String,
}

async fn lock_waiting(tx: &mut Transaction<'_, Postgres>, message: Uuid) -> Result<Waiting> {
    let row: Option<(String, Option<String>, String, bool)> = sqlx::query_as(
        "SELECT from_address, from_name, subject, ticket_id IS NULL AND discarded_at IS NULL
         FROM messages WHERE id = $1 AND direction = 'inbound' FOR UPDATE",
    )
    .bind(message)
    .fetch_optional(&mut **tx)
    .await?;
    match row {
        None => Err(Error::NotFound("message")),
        Some((_, _, _, false)) => rule("the message is no longer in the inbox"),
        Some((from, from_name, subject, true)) => Ok(Waiting {
            from,
            from_name: from_name.filter(|n| !n.trim().is_empty()),
            subject,
        }),
    }
}

/// Opens a ticket of `customer` for the message. The sender becomes the ticket's requester,
/// registered with that customer if new; a message without a readable sender gets no requester.
/// A departed requester gets no ticket, but an address of one who departed from another customer
/// passes to a new requester of `customer`. Returns the new ticket's number.
pub async fn open_ticket(db: &PgPool, message: Uuid, customer: Uuid) -> Result<i64> {
    let mut tx = db.begin().await?;
    let Waiting {
        from,
        from_name,
        subject,
    } = lock_waiting(&mut tx, message).await?;

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

    let requester = if from.contains('@') {
        // A known requester learns their name from the message when it was not known yet.
        Some(
            crate::requesters::find_or_create(&mut tx, customer, &from, from_name.as_deref())
                .await?,
        )
    } else {
        None
    };

    let ticket = Uuid::new_v4();
    let title = if subject.trim().is_empty() {
        "(no subject)"
    } else {
        subject.trim()
    };
    let number: i64 = sqlx::query_scalar(
        "INSERT INTO tickets (id, customer_id, requester_id, title) VALUES ($1, $2, $3, $4)
         RETURNING number",
    )
    .bind(ticket)
    .bind(customer)
    .bind(requester)
    .bind(title)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("UPDATE messages SET ticket_id = $2 WHERE id = $1")
        .bind(message)
        .bind(ticket)
        .execute(&mut *tx)
        .await?;
    crate::tickets::emit_opened(&mut tx, number).await?;
    tx.commit().await?;
    Ok(number)
}

/// Attaches the message to an existing, not closed ticket, which reopens if it was waiting or
/// resolved, as a reply from the requester would.
pub async fn attach(db: &PgPool, message: Uuid, number: i64) -> Result<()> {
    let mut tx = db.begin().await?;
    lock_waiting(&mut tx, message).await?;
    let ticket: Option<(Uuid, String)> =
        sqlx::query_as("SELECT id, state FROM tickets WHERE number = $1 FOR UPDATE")
            .bind(number)
            .fetch_optional(&mut *tx)
            .await?;
    let Some((ticket, state)) = ticket else {
        return Err(Error::NotFound("ticket"));
    };
    if state == "closed" {
        return rule("the ticket is closed; open a new ticket instead");
    }
    sqlx::query("UPDATE messages SET ticket_id = $2 WHERE id = $1")
        .bind(message)
        .bind(ticket)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE tickets SET updated_at = now(),
                state = CASE WHEN state IN ('waiting', 'resolved') THEN 'open' ELSE state END
         WHERE id = $1",
    )
    .bind(ticket)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn discard(db: &PgPool, message: Uuid) -> Result<()> {
    let mut tx = db.begin().await?;
    lock_waiting(&mut tx, message).await?;
    sqlx::query("UPDATE messages SET discarded_at = now() WHERE id = $1")
        .bind(message)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use crate::testing::{self, state};

    #[sqlx::test]
    async fn opening_a_ticket_registers_the_sender_with_the_customer(db: PgPool) {
        let c = testing::customer(&db, "Brightfield").await;
        let m = testing::inbox_message(&db, "new.person@mail.test").await;
        let n = open_ticket(&db, m, c).await.unwrap();
        let (customer, email): (Uuid, String) = sqlx::query_as(
            "SELECT t.customer_id, a.address FROM tickets t
             JOIN requester_addresses a ON a.requester_id = t.requester_id
             WHERE t.number = $1",
        )
        .bind(n)
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!((customer, email.as_str()), (c, "new.person@mail.test"));
        // The message has left the inbox.
        assert!(matches!(discard(&db, m).await, Err(Error::Rule(_))));
    }

    #[sqlx::test]
    async fn the_sender_s_name_is_kept(db: PgPool) {
        let c = testing::customer(&db, "Brightfield").await;
        let m = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO messages (id, direction, from_address, from_name, subject, body_text)
             VALUES ($1, 'inbound', 'lena.moro@mail.test', 'Lena Moro', 'VPN', '...')",
        )
        .bind(m)
        .execute(&db)
        .await
        .unwrap();
        let n = open_ticket(&db, m, c).await.unwrap();
        let ticket = crate::tickets::detail(&db, n).await.unwrap();
        assert_eq!(ticket.requester.unwrap().name.as_deref(), Some("Lena Moro"));
    }

    #[sqlx::test]
    async fn a_requester_of_another_customer_is_refused(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let m = testing::inbox_message(&db, "ada@brightfield.test").await;
        assert!(matches!(
            open_ticket(&db, m, harbor).await,
            Err(Error::Rule(_))
        ));
        assert!(open_ticket(&db, m, brightfield).await.is_ok());
    }

    /// No ticket is opened for a departed requester; for another customer, the address passes to a
    /// new requester of that customer.
    #[sqlx::test]
    async fn a_departed_requester_gets_no_ticket(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        crate::requesters::depart(&db, ada).await.unwrap();
        let m = testing::inbox_message(&db, "ada@brightfield.test").await;

        let Err(Error::Rule(why)) = open_ticket(&db, m, brightfield).await else {
            panic!("a ticket opened for a departed requester");
        };
        assert!(why.contains("departed"), "{why}");

        let n = open_ticket(&db, m, harbor).await.unwrap();
        let requester = crate::tickets::detail(&db, n)
            .await
            .unwrap()
            .requester
            .unwrap();
        assert_ne!(requester.id, ada);
        assert_eq!(requester.addresses, ["ada@brightfield.test"]);
        assert!(!requester.departed);
    }

    #[sqlx::test]
    async fn an_unreadable_sender_gets_a_ticket_without_requester(db: PgPool) {
        let c = testing::customer(&db, "Brightfield").await;
        let m = testing::inbox_message(&db, "(unreadable)").await;
        let n = open_ticket(&db, m, c).await.unwrap();
        let requester: Option<Uuid> =
            sqlx::query_scalar("SELECT requester_id FROM tickets WHERE number = $1")
                .bind(n)
                .fetch_one(&db)
                .await
                .unwrap();
        assert!(requester.is_none());
    }

    #[sqlx::test]
    async fn offboarded_customers_get_no_new_tickets(db: PgPool) {
        let c = testing::customer(&db, "Brightfield").await;
        sqlx::query("UPDATE customers SET offboarded_at = now() WHERE id = $1")
            .bind(c)
            .execute(&db)
            .await
            .unwrap();
        let m = testing::inbox_message(&db, "ada@brightfield.test").await;
        assert!(matches!(open_ticket(&db, m, c).await, Err(Error::Rule(_))));
    }

    #[sqlx::test]
    async fn attaching_reopens_a_resolved_ticket_but_not_a_closed_one(db: PgPool) {
        let c = testing::customer(&db, "Brightfield").await;
        let n = testing::ticket(&db, c, None, "first@brightfield.test").await;
        sqlx::query("UPDATE tickets SET state = 'resolved' WHERE number = $1")
            .bind(n)
            .execute(&db)
            .await
            .unwrap();
        let m = testing::inbox_message(&db, "ada@brightfield.test").await;
        attach(&db, m, n).await.unwrap();
        assert_eq!(state(&db, n).await, "open");

        sqlx::query("UPDATE tickets SET state = 'closed' WHERE number = $1")
            .bind(n)
            .execute(&db)
            .await
            .unwrap();
        let m2 = testing::inbox_message(&db, "ada@brightfield.test").await;
        assert!(matches!(attach(&db, m2, n).await, Err(Error::Rule(_))));
        assert!(matches!(
            attach(&db, m2, 9999).await,
            Err(Error::NotFound(_))
        ));
    }

    #[sqlx::test]
    async fn a_discarded_message_leaves_the_inbox(db: PgPool) {
        let m = testing::inbox_message(&db, "someone@elsewhere.test").await;
        discard(&db, m).await.unwrap();
        let waiting: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM messages WHERE ticket_id IS NULL AND discarded_at IS NULL",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(waiting, 0);
    }
}
