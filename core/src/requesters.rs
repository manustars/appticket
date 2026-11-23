//! Requesters and their e-mail addresses: a requester has one or more, each address belongs to
//! exactly one requester, and a reply goes to the address the requester last wrote from on the
//! ticket. Rules are in docs/domain-rules.md ("People").

use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};
use utoipa::ToSchema;
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
        .bind(filled(name))
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
                .bind(filled(name))
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

/// Gives the requester one more address; an address already belonging to a requester is refused.
pub async fn add_address(db: &PgPool, requester: Uuid, address: &str) -> Result<()> {
    let address = clean_address(address)?;
    let mut tx = db.begin().await?;
    lock_with_customer(&mut tx, requester).await?;
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
    lock_with_customer(&mut tx, requester).await?;
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

/// What agents who manage customers correct on a requester.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct RequesterDetails {
    /// Empty for a requester whose name is not known.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub phone_numbers: Vec<String>,
    #[serde(default)]
    pub job_title: Option<String>,
    /// Internal notes, seen only by agents.
    #[serde(default)]
    pub notes: String,
    /// One of the customer's sites that is not retired.
    #[serde(default)]
    pub site_id: Option<Uuid>,
}

/// The site a requester is at.
#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct SiteRef {
    pub id: Uuid,
    pub name: String,
}

/// A requester's customer, as their page shows it.
#[derive(Debug, Serialize, ToSchema)]
pub struct RequesterCustomer {
    pub id: Uuid,
    pub name: String,
    /// An offboarded customer's requesters do not change.
    pub offboarded: bool,
}

/// Everything a requester's page shows but their tickets.
#[derive(Debug, Serialize, ToSchema)]
pub struct RequesterDetail {
    pub id: Uuid,
    pub customer: RequesterCustomer,
    /// The requester's addresses, the first registered first.
    pub addresses: Vec<String>,
    pub name: Option<String>,
    pub phone_numbers: Vec<String>,
    pub job_title: Option<String>,
    /// Internal notes, seen only by agents.
    pub notes: String,
    pub site: Option<SiteRef>,
}

/// A requester with their customer, details and site.
pub async fn detail(db: &PgPool, id: Uuid) -> Result<RequesterDetail> {
    #[derive(sqlx::FromRow)]
    struct Row {
        customer_id: Uuid,
        customer_name: String,
        offboarded: bool,
        name: Option<String>,
        phone_numbers: Vec<String>,
        job_title: Option<String>,
        notes: String,
        site_id: Option<Uuid>,
        site_name: Option<String>,
    }
    let row: Row = sqlx::query_as(
        "SELECT r.customer_id, c.name AS customer_name, c.offboarded_at IS NOT NULL AS offboarded,
                r.name, r.phone_numbers, r.job_title, r.notes,
                r.site_id, s.name AS site_name
         FROM requesters r
         JOIN customers c ON c.id = r.customer_id
         LEFT JOIN sites s ON s.id = r.site_id
         WHERE r.id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await?
    .ok_or(Error::NotFound("requester"))?;
    Ok(RequesterDetail {
        id,
        customer: RequesterCustomer {
            id: row.customer_id,
            name: row.customer_name,
            offboarded: row.offboarded,
        },
        addresses: addresses(db, id).await?,
        name: row.name,
        phone_numbers: row.phone_numbers,
        job_title: row.job_title,
        notes: row.notes,
        site: row
            .site_id
            .zip(row.site_name)
            .map(|(id, name)| SiteRef { id, name }),
    })
}

/// Trimmed, or `None` when blank.
fn filled(text: Option<&str>) -> Option<&str> {
    text.map(str::trim).filter(|t| !t.is_empty())
}

/// Corrects a requester's details. Their site must be one of their customer's that is not
/// retired.
pub async fn update(db: &PgPool, id: Uuid, details: &RequesterDetails) -> Result<()> {
    let mut phone_numbers = Vec::new();
    for number in &details.phone_numbers {
        match filled(Some(number)) {
            Some(number) => phone_numbers.push(number),
            None => return rule("a phone number cannot be blank"),
        }
    }
    let mut tx = db.begin().await?;
    let customer = lock_with_customer(&mut tx, id).await?;
    if let Some(site) = details.site_id {
        let retired: Option<bool> = sqlx::query_scalar(
            "SELECT retired_at IS NOT NULL FROM sites WHERE id = $1 AND customer_id = $2",
        )
        .bind(site)
        .bind(customer)
        .fetch_optional(&mut *tx)
        .await?;
        match retired {
            None => return rule("the site is not one of the requester's customer"),
            Some(true) => return rule("the site is retired"),
            Some(false) => {}
        }
    }
    sqlx::query(
        "UPDATE requesters SET name = $2, phone_numbers = $3, job_title = $4, notes = $5,
                site_id = $6
         WHERE id = $1",
    )
    .bind(id)
    .bind(filled(details.name.as_deref()))
    .bind(&phone_numbers)
    .bind(filled(details.job_title.as_deref()))
    .bind(details.notes.trim())
    .bind(details.site_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Locks the requester's customer, which must not be offboarded, then the requester, for any
/// change to the requester; retiring a site locks the customer first too. Returns the customer.
async fn lock_with_customer(tx: &mut Transaction<'_, Postgres>, requester: Uuid) -> Result<Uuid> {
    let customer: Uuid = sqlx::query_scalar("SELECT customer_id FROM requesters WHERE id = $1")
        .bind(requester)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(Error::NotFound("requester"))?;
    crate::customers::lock_active(tx, customer).await?;
    // A requester can be moved to another customer in between: check under the lock.
    let now: Uuid =
        sqlx::query_scalar("SELECT customer_id FROM requesters WHERE id = $1 FOR UPDATE")
            .bind(requester)
            .fetch_one(&mut **tx)
            .await?;
    if now != customer {
        return rule("the requester has just moved to another customer; reload and try again");
    }
    Ok(customer)
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

    #[sqlx::test]
    async fn a_requester_s_details_are_corrected(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;

        let details = RequesterDetails {
            name: Some(" Ada Ferrini ".into()),
            phone_numbers: vec![" 000 000 0101 ".into(), "000 000 0102".into()],
            job_title: Some("Office manager".into()),
            notes: "Prefers phone calls in the morning.".into(),
            site_id: None,
        };
        update(&db, ada, &details).await.unwrap();
        let seen = detail(&db, ada).await.unwrap();
        assert_eq!(seen.name.as_deref(), Some("Ada Ferrini"));
        assert_eq!(seen.phone_numbers, ["000 000 0101", "000 000 0102"]);
        assert_eq!(seen.job_title.as_deref(), Some("Office manager"));
        assert_eq!(seen.notes, "Prefers phone calls in the morning.");
        assert_eq!(seen.addresses, ["ada@brightfield.test"]);
        assert_eq!(seen.customer.name, "Brightfield");
        assert!(seen.site.is_none());

        // Emptied details are forgotten; a blank phone number is refused.
        update(
            &db,
            ada,
            &RequesterDetails {
                name: Some("  ".into()),
                job_title: Some("".into()),
                ..RequesterDetails::default()
            },
        )
        .await
        .unwrap();
        let seen = detail(&db, ada).await.unwrap();
        assert_eq!((seen.name, seen.job_title), (None, None));
        assert!(seen.phone_numbers.is_empty());
        assert!(matches!(
            update(
                &db,
                ada,
                &RequesterDetails {
                    phone_numbers: vec!["  ".into()],
                    ..RequesterDetails::default()
                }
            )
            .await,
            Err(Error::Rule(_))
        ));
        assert!(matches!(
            detail(&db, Uuid::new_v4()).await,
            Err(Error::NotFound(_))
        ));
    }

    #[sqlx::test]
    async fn a_requester_is_at_an_active_site_of_their_customer(db: PgPool) {
        let brightfield = crate::customers::create(&db, "Brightfield", &[])
            .await
            .unwrap();
        let harbor = crate::customers::create(&db, "Harbor Lane", &[])
            .await
            .unwrap();
        let head = crate::sites::create(&db, brightfield, "Head office", "Via dei Tigli 1")
            .await
            .unwrap();
        let old = crate::sites::create(&db, brightfield, "Old office", "Via del Glicine 3")
            .await
            .unwrap();
        let quay = crate::sites::create(&db, harbor, "Quay office", "Molo delle Reti 2")
            .await
            .unwrap();
        crate::sites::retire(&db, old).await.unwrap();
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let at = |site| RequesterDetails {
            site_id: Some(site),
            ..RequesterDetails::default()
        };

        update(&db, ada, &at(head)).await.unwrap();
        let site = detail(&db, ada).await.unwrap().site.unwrap();
        assert_eq!((site.id, site.name.as_str()), (head, "Head office"));
        assert!(
            matches!(update(&db, ada, &at(quay)).await, Err(Error::Rule(_))),
            "another customer's"
        );
        assert!(
            matches!(update(&db, ada, &at(old)).await, Err(Error::Rule(_))),
            "retired"
        );
        update(&db, ada, &RequesterDetails::default())
            .await
            .unwrap();
        assert!(detail(&db, ada).await.unwrap().site.is_none());

        crate::customers::offboard(&db, brightfield).await.unwrap();
        assert!(
            matches!(update(&db, ada, &at(head)).await, Err(Error::Rule(_))),
            "offboarded"
        );
    }

    #[sqlx::test]
    async fn retiring_a_site_leaves_its_requesters_without_one(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let head = crate::sites::create(&db, brightfield, "Head office", "Via dei Tigli 1")
            .await
            .unwrap();
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        update(
            &db,
            ada,
            &RequesterDetails {
                site_id: Some(head),
                ..RequesterDetails::default()
            },
        )
        .await
        .unwrap();

        crate::sites::retire(&db, head).await.unwrap();
        assert!(detail(&db, ada).await.unwrap().site.is_none());
    }

    #[sqlx::test]
    async fn moving_a_requester_leaves_their_site(db: PgPool) {
        use crate::tickets::{self, RequesterMove};
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let head = crate::sites::create(&db, brightfield, "Head office", "Via dei Tigli 1")
            .await
            .unwrap();
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        update(
            &db,
            ada,
            &RequesterDetails {
                site_id: Some(head),
                ..RequesterDetails::default()
            },
        )
        .await
        .unwrap();
        let n = testing::ticket(&db, brightfield, Some(ada), "a@brightfield.test").await;

        tickets::move_to_customer(&db, n, harbor, RequesterMove::Move)
            .await
            .unwrap();
        let seen = detail(&db, ada).await.unwrap();
        assert_eq!(seen.customer.id, harbor);
        assert!(seen.site.is_none(), "a site of the old customer");
    }

    /// The database keeps the details' rules even when the code above is bypassed.
    #[sqlx::test]
    async fn the_database_keeps_the_details_rules(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let head = crate::sites::create(&db, brightfield, "Head office", "Via dei Tigli 1")
            .await
            .unwrap();
        let quay = crate::sites::create(&db, harbor, "Quay office", "Molo delle Reti 2")
            .await
            .unwrap();
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let set = |column: &'static str, value: &'static str| {
            let db = db.clone();
            async move {
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "UPDATE requesters SET {column} = {value} WHERE id = $1"
                )))
                .bind(ada)
                .execute(&db)
                .await
            }
        };
        assert!(set("phone_numbers", "ARRAY['000 000 0101']").await.is_ok());
        assert!(
            set("phone_numbers", "ARRAY['000 000', ' ']").await.is_err(),
            "a blank phone"
        );
        assert!(
            set("phone_numbers", "ARRAY[E'000 000\n']").await.is_err(),
            "an untrimmed phone"
        );
        assert!(
            set("job_title", "' Manager'").await.is_err(),
            "an untrimmed job title"
        );
        assert!(set("name", "' Ada'").await.is_err(), "an untrimmed name");
        assert!(
            set("name", "''").await.is_err(),
            "a blank name rather than none"
        );

        // A name taken from a message is kept trimmed.
        let mut tx = db.begin().await.unwrap();
        let lena = create(
            &mut tx,
            brightfield,
            "lena@brightfield.test",
            Some(" Lena Moro "),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            detail(&db, lena).await.unwrap().name.as_deref(),
            Some("Lena Moro")
        );
        let at = |site: Uuid| {
            let db = db.clone();
            async move {
                sqlx::query("UPDATE requesters SET site_id = $2 WHERE id = $1")
                    .bind(ada)
                    .bind(site)
                    .execute(&db)
                    .await
            }
        };
        assert!(at(quay).await.is_err(), "another customer's site");
        assert!(at(head).await.is_ok());
        let retire = sqlx::query("UPDATE sites SET retired_at = now() WHERE id = $1")
            .bind(head)
            .execute(&db)
            .await;
        assert!(retire.is_err(), "a site retired with a requester at it");

        let old = crate::sites::create(&db, brightfield, "Old office", "Via del Glicine 3")
            .await
            .unwrap();
        crate::sites::retire(&db, old).await.unwrap();
        assert!(at(old).await.is_err(), "a retired site");
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
