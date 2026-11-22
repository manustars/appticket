//! Requesters and their e-mail addresses: a requester has one or more, each address belongs to
//! exactly one requester, and a reply goes to the address the requester last wrote from on the
//! ticket. Rules are in docs/domain-rules.md ("People").

use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::{Error, Result, rule};

/// Lowercases and checks an e-mail address such as `ada@brightfield.test`.
pub fn clean_address(address: &str) -> Result<String> {
    let address = address.trim().to_lowercase();
    match address.rsplit_once('@') {
        Some((local, domain))
            if !local.is_empty()
                && domain.contains('.')
                && !address.chars().any(char::is_whitespace) =>
        {
            Ok(address)
        }
        _ => rule(format!("{address:?} is not an e-mail address")),
    }
}

/// The requester an address belongs to.
#[derive(Debug, sqlx::FromRow)]
pub struct Owner {
    pub requester_id: Uuid,
    pub customer_id: Uuid,
    pub customer_name: String,
}

/// Who the address belongs to, if anyone.
pub async fn owner_of(tx: &mut Transaction<'_, Postgres>, address: &str) -> Result<Option<Owner>> {
    Ok(sqlx::query_as(
        "SELECT r.id AS requester_id, r.customer_id, c.name AS customer_name
         FROM requester_addresses a
         JOIN requesters r ON r.id = a.requester_id
         JOIN customers c ON c.id = r.customer_id
         WHERE a.address = $1",
    )
    .bind(address)
    .fetch_optional(&mut **tx)
    .await?)
}

/// Adds a requester of `customer` with its first address, which must be clean and belong to
/// nobody yet.
pub async fn create(
    tx: &mut Transaction<'_, Postgres>,
    customer: Uuid,
    address: &str,
    name: Option<&str>,
) -> Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO requesters (id, customer_id, name) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(customer)
        .bind(name)
        .execute(&mut **tx)
        .await?;
    let inserted =
        sqlx::query("INSERT INTO requester_addresses (address, requester_id) VALUES ($1, $2)")
            .bind(address)
            .bind(id)
            .execute(&mut **tx)
            .await;
    match inserted {
        Err(e) if taken(&e) => rule(format!(
            "the address {address} already belongs to a requester"
        )),
        Err(e) => Err(e.into()),
        Ok(_) => Ok(id),
    }
}

/// The requester of `customer` the address belongs to, registered with `name` if new; an address
/// of another customer's requester is refused. A known requester without a name takes `name`.
pub async fn find_or_create(
    tx: &mut Transaction<'_, Postgres>,
    customer: Uuid,
    address: &str,
    name: Option<&str>,
) -> Result<Uuid> {
    match owner_of(tx, address).await? {
        Some(owner) if owner.customer_id == customer => {
            sqlx::query("UPDATE requesters SET name = $2 WHERE id = $1 AND name IS NULL")
                .bind(owner.requester_id)
                .bind(name)
                .execute(&mut **tx)
                .await?;
            Ok(owner.requester_id)
        }
        Some(owner) => rule(format!(
            "the address {address} is already a requester of {}, and a requester belongs to one customer",
            owner.customer_name
        )),
        None => create(tx, customer, address, name).await,
    }
}

/// The requester's addresses, the first registered first.
pub async fn addresses<'e>(db: impl sqlx::PgExecutor<'e>, requester: Uuid) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT address FROM requester_addresses WHERE requester_id = $1
         ORDER BY created_at, address",
    )
    .bind(requester)
    .fetch_all(db)
    .await?)
}

/// Locks the requester for a change of addresses; their customer must not be offboarded.
async fn lock(tx: &mut Transaction<'_, Postgres>, requester: Uuid) -> Result<()> {
    let offboarded: Option<bool> = sqlx::query_scalar(
        "SELECT c.offboarded_at IS NOT NULL FROM requesters r
         JOIN customers c ON c.id = r.customer_id
         WHERE r.id = $1 FOR UPDATE OF r",
    )
    .bind(requester)
    .fetch_optional(&mut **tx)
    .await?;
    match offboarded {
        None => Err(Error::NotFound("requester")),
        Some(true) => rule("the customer is offboarded"),
        Some(false) => Ok(()),
    }
}

/// Gives the requester one more address; an address already belonging to a requester is refused.
pub async fn add_address(db: &PgPool, requester: Uuid, address: &str) -> Result<()> {
    let address = clean_address(address)?;
    let mut tx = db.begin().await?;
    lock(&mut tx, requester).await?;
    match owner_of(&mut tx, &address).await? {
        Some(owner) if owner.requester_id == requester => {
            return rule(format!(
                "the address {address} already belongs to this requester"
            ));
        }
        Some(owner) => {
            return rule(format!(
                "the address {address} already belongs to a requester of {}, and an address belongs to one requester",
                owner.customer_name
            ));
        }
        None => {}
    }
    let inserted =
        sqlx::query("INSERT INTO requester_addresses (address, requester_id) VALUES ($1, $2)")
            .bind(&address)
            .bind(requester)
            .execute(&mut *tx)
            .await;
    match inserted {
        Err(e) if taken(&e) => {
            return rule(format!(
                "the address {address} already belongs to a requester"
            ));
        }
        Err(e) => return Err(e.into()),
        Ok(_) => {}
    }
    tx.commit().await?;
    Ok(())
}

/// Takes an address from the requester, who keeps at least one.
pub async fn remove_address(db: &PgPool, requester: Uuid, address: &str) -> Result<()> {
    let address = address.trim().to_lowercase();
    let mut tx = db.begin().await?;
    lock(&mut tx, requester).await?;
    let addresses = addresses(&mut *tx, requester).await?;
    if !addresses.contains(&address) {
        return Err(Error::NotFound("address"));
    }
    if addresses.len() == 1 {
        return rule("a requester keeps at least one address");
    }
    sqlx::query("DELETE FROM requester_addresses WHERE address = $1")
        .bind(&address)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Where a reply on a ticket can go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipient {
    /// The address the requester last wrote from on the ticket, while it is still theirs.
    pub wrote_from: Option<String>,
    /// The address an agent picked for the last reply on the ticket, while it is still theirs.
    pub last_picked: Option<String>,
    /// All the requester's addresses, the first registered first.
    pub addresses: Vec<String>,
}

impl Recipient {
    /// The address a reply goes to. The requester's last address on the ticket wins; on a ticket
    /// they have not written on, `to` is the one the agent picks, needed when they have several,
    /// unless an agent already picked one for an earlier reply there.
    pub fn choose(&self, to: Option<&str>) -> Result<String> {
        let to = to
            .map(|t| t.trim().to_lowercase())
            .filter(|t| !t.is_empty());
        if let Some(address) = &self.wrote_from {
            return match to {
                Some(to) if &to != address => rule(format!(
                    "the requester last wrote from {address} on this ticket, so the reply goes there"
                )),
                _ => Ok(address.clone()),
            };
        }
        match (to, self.addresses.as_slice()) {
            (Some(to), addresses) if addresses.contains(&to) => Ok(to),
            (Some(to), _) => rule(format!(
                "the address {to} does not belong to the ticket's requester"
            )),
            (None, [only]) => Ok(only.clone()),
            (None, _) => match &self.last_picked {
                Some(picked) => Ok(picked.clone()),
                None => rule(
                    "the requester has several addresses and has not written on this ticket: pick the one to reply to",
                ),
            },
        }
    }
}

/// The ticket requester's addresses, the one they last wrote from on it and the one the last
/// reply went to; `None` for a ticket without a requester.
pub async fn recipient(conn: &mut sqlx::PgConnection, ticket: Uuid) -> Result<Option<Recipient>> {
    type Row = (Option<Uuid>, Option<String>, Option<String>);
    let row: Option<Row> = sqlx::query_as(
        "SELECT requester_id, ticket_reply_address(id), ticket_picked_address(id)
         FROM tickets WHERE id = $1",
    )
    .bind(ticket)
    .fetch_optional(&mut *conn)
    .await?;
    let (requester, wrote_from, last_picked) = row.ok_or(Error::NotFound("ticket"))?;
    let Some(requester) = requester else {
        return Ok(None);
    };
    Ok(Some(Recipient {
        wrote_from,
        last_picked,
        addresses: addresses(&mut *conn, requester).await?,
    }))
}

/// Whether a database error is a unique violation: someone else registered the address meanwhile.
fn taken(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(e) if e.is_unique_violation())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;

    fn recipient(wrote_from: Option<&str>, addresses: &[&str]) -> Recipient {
        Recipient {
            wrote_from: wrote_from.map(str::to_owned),
            last_picked: None,
            addresses: addresses.iter().map(|a| (*a).to_owned()).collect(),
        }
    }

    #[test]
    fn a_reply_goes_where_the_requester_last_wrote_from() {
        let r = recipient(
            Some("ada@home.test"),
            &["ada@brightfield.test", "ada@home.test"],
        );
        assert_eq!(r.choose(None).unwrap(), "ada@home.test");
        assert_eq!(r.choose(Some("ADA@home.test")).unwrap(), "ada@home.test");
        assert!(matches!(
            r.choose(Some("ada@brightfield.test")),
            Err(Error::Rule(_))
        ));
    }

    #[test]
    fn on_a_ticket_they_have_not_written_on_the_agent_picks() {
        let several = recipient(None, &["ada@brightfield.test", "ada@home.test"]);
        assert!(matches!(several.choose(None), Err(Error::Rule(_))));
        assert_eq!(
            several.choose(Some("ada@home.test")).unwrap(),
            "ada@home.test"
        );
        assert!(matches!(
            several.choose(Some("ivo@harbor.test")),
            Err(Error::Rule(_))
        ));
        let one = recipient(None, &["ada@brightfield.test"]);
        assert_eq!(one.choose(None).unwrap(), "ada@brightfield.test");

        // An earlier pick holds for the next reply, and the agent can still pick another.
        let picked = Recipient {
            last_picked: Some("ada@home.test".into()),
            ..several
        };
        assert_eq!(picked.choose(None).unwrap(), "ada@home.test");
        assert_eq!(
            picked.choose(Some("ada@brightfield.test")).unwrap(),
            "ada@brightfield.test"
        );
    }

    #[test]
    fn addresses_are_cleaned_and_checked() {
        assert_eq!(
            clean_address(" Ada@Brightfield.TEST ").unwrap(),
            "ada@brightfield.test"
        );
        for bad in [
            "",
            "ada",
            "@brightfield.test",
            "ada@localhost",
            "a da@b.test",
        ] {
            assert!(clean_address(bad).is_err(), "{bad:?} accepted");
        }
    }

    #[sqlx::test]
    async fn an_address_belongs_to_one_requester(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let ivo = testing::requester(&db, harbor, "ivo@harbor.test").await;

        add_address(&db, ada, " Ada@Home.test").await.unwrap();
        assert_eq!(
            addresses(&db, ada).await.unwrap(),
            ["ada@brightfield.test", "ada@home.test"]
        );
        let Err(Error::Rule(why)) = add_address(&db, ivo, "ada@home.test").await else {
            panic!("a second owner was accepted");
        };
        assert!(why.contains("Brightfield"), "{why}");
        assert!(matches!(
            add_address(&db, ada, "ada@home.test").await,
            Err(Error::Rule(_))
        ));
        assert!(matches!(
            add_address(&db, Uuid::new_v4(), "x@home.test").await,
            Err(Error::NotFound(_))
        ));
    }

    #[sqlx::test]
    async fn a_requester_keeps_at_least_one_address(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        add_address(&db, ada, "ada@home.test").await.unwrap();

        remove_address(&db, ada, "ADA@brightfield.test")
            .await
            .unwrap();
        assert_eq!(addresses(&db, ada).await.unwrap(), ["ada@home.test"]);
        assert!(matches!(
            remove_address(&db, ada, "ada@home.test").await,
            Err(Error::Rule(_))
        ));
        assert!(matches!(
            remove_address(&db, ada, "ada@brightfield.test").await,
            Err(Error::NotFound(_))
        ));
    }

    /// An address taken from the requester no longer receives their replies.
    #[sqlx::test]
    async fn a_removed_address_gets_no_more_replies(db: PgPool) {
        use crate::tickets::{self, TicketState};
        let brightfield = testing::customer(&db, "Brightfield").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        add_address(&db, ada, "ada@home.test").await.unwrap();
        add_address(&db, ada, "ada@studio.test").await.unwrap();
        let agent = testing::agent(&db, "marco@northgate.test").await;
        let n = testing::ticket(&db, brightfield, Some(ada), "h@home.test").await;
        testing::message_from(&db, n, "ada@home.test").await;
        tickets::reply(
            &db,
            &testing::sender(),
            n,
            agent,
            "Hi.",
            None,
            TicketState::Waiting,
        )
        .await
        .unwrap();

        remove_address(&db, ada, "ada@home.test").await.unwrap();
        let requester = tickets::detail(&db, n).await.unwrap().requester.unwrap();
        assert_eq!(requester.reply_to, None);
        assert_eq!(
            requester.last_picked, None,
            "the last reply went to a removed address"
        );
        assert_eq!(requester.email, "ada@brightfield.test");
        assert!(matches!(
            tickets::reply(
                &db,
                &testing::sender(),
                n,
                agent,
                "Hi.",
                None,
                TicketState::Waiting
            )
            .await,
            Err(Error::Rule(_))
        ));
    }

    /// The database keeps the rules even when the code above is bypassed.
    #[sqlx::test]
    async fn the_database_keeps_the_address_rules(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;

        let last = sqlx::query("DELETE FROM requester_addresses WHERE requester_id = $1")
            .bind(ada)
            .execute(&db)
            .await;
        assert!(last.is_err(), "the last address was removed");

        let mut tx = db.begin().await.unwrap();
        sqlx::query("INSERT INTO requesters (id, customer_id) VALUES ($1, $2)")
            .bind(Uuid::new_v4())
            .bind(brightfield)
            .execute(&mut *tx)
            .await
            .unwrap();
        assert!(tx.commit().await.is_err(), "a requester without an address");

        let taken = sqlx::query(
            "INSERT INTO requester_addresses (address, requester_id) VALUES ('ada@brightfield.test', $1)",
        )
        .bind(testing::requester(&db, brightfield, "lena@brightfield.test").await)
        .execute(&db)
        .await;
        assert!(taken.is_err(), "an address with two requesters");

        let upper = sqlx::query(
            "INSERT INTO requester_addresses (address, requester_id) VALUES ('Ada@Home.test', $1)",
        )
        .bind(ada)
        .execute(&db)
        .await;
        assert!(upper.is_err(), "an address that is not lowercase");
    }

    /// The requesters registered before addresses were their own keep theirs.
    #[sqlx::test(migrations = false)]
    async fn existing_requesters_keep_their_address(db: PgPool) {
        let migrator = sqlx::migrate!();
        migrator.run_to(12, &db).await.unwrap();
        let customer = Uuid::new_v4();
        let ada = Uuid::new_v4();
        sqlx::query("INSERT INTO customers (id, name) VALUES ($1, 'Brightfield')")
            .bind(customer)
            .execute(&db)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO requesters (id, customer_id, email) VALUES ($1, $2, 'ada@brightfield.test')",
        )
        .bind(ada)
        .bind(customer)
        .execute(&db)
        .await
        .unwrap();

        migrator.run(&db).await.unwrap();
        assert_eq!(addresses(&db, ada).await.unwrap(), ["ada@brightfield.test"]);
    }
}
