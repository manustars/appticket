//! Customers, the mail domains that identify their requesters, and the requesters themselves.
//! Rules are in docs/domain-rules.md ("Customers, assets and services", "People").

use serde::Serialize;
use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::{Error, Result, rule};

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct CustomerSummary {
    pub id: Uuid,
    pub name: String,
    /// Whether this is the help desk's own organization.
    pub is_help_desk: bool,
    #[serde(with = "time::serde::rfc3339::option")]
    pub offboarded_at: Option<OffsetDateTime>,
    pub domains: Vec<String>,
    pub requesters: i64,
    /// Tickets that are not closed.
    pub active_tickets: i64,
}

#[derive(Serialize, ToSchema)]
pub struct CustomerDetail {
    #[serde(flatten)]
    pub summary: CustomerSummary,
    pub requester_list: Vec<RequesterEntry>,
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct RequesterEntry {
    pub id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub tickets: i64,
}

/// Customer summaries: all of them (offboarded ones last), or only `$1` when it is not null.
const SUMMARIES: &str = "SELECT c.id, c.name, c.is_help_desk, c.offboarded_at,
        coalesce((SELECT array_agg(d.domain ORDER BY d.domain) FROM customer_domains d
                  WHERE d.customer_id = c.id), '{}') AS domains,
        (SELECT count(*) FROM requesters r WHERE r.customer_id = c.id) AS requesters,
        (SELECT count(*) FROM tickets t WHERE t.customer_id = c.id AND t.state <> 'closed')
            AS active_tickets
     FROM customers c
     WHERE $1::uuid IS NULL OR c.id = $1
     ORDER BY c.offboarded_at IS NOT NULL, c.name";

/// Every customer, offboarded ones last.
pub async fn list(db: &PgPool) -> Result<Vec<CustomerSummary>> {
    Ok(sqlx::query_as(SUMMARIES)
        .bind(None::<Uuid>)
        .fetch_all(db)
        .await?)
}

pub async fn detail(db: &PgPool, id: Uuid) -> Result<CustomerDetail> {
    let summary: CustomerSummary = sqlx::query_as(SUMMARIES)
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or(Error::NotFound("customer"))?;
    let requester_list = sqlx::query_as(
        "SELECT r.id, r.email, r.name,
                (SELECT count(*) FROM tickets t WHERE t.requester_id = r.id) AS tickets
         FROM requesters r WHERE r.customer_id = $1 ORDER BY r.email",
    )
    .bind(id)
    .fetch_all(db)
    .await?;
    Ok(CustomerDetail {
        summary,
        requester_list,
    })
}

fn clean_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty() {
        return rule("a customer needs a name");
    }
    Ok(name)
}

/// Lowercases and checks a mail domain such as `brightfield.test`.
pub fn clean_domain(domain: &str) -> Result<String> {
    let domain = domain.trim().trim_start_matches('@').to_lowercase();
    let valid = domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && domain
            .chars()
            .all(|c| c.is_alphanumeric() || c == '.' || c == '-');
    if !valid {
        return rule(format!("{domain:?} is not a mail domain"));
    }
    Ok(domain)
}

async fn add_domain_in(
    tx: &mut Transaction<'_, Postgres>,
    customer: Uuid,
    domain: &str,
) -> Result<()> {
    let domain = clean_domain(domain)?;
    let owner: Option<String> = sqlx::query_scalar(
        "SELECT c.name FROM customer_domains d JOIN customers c ON c.id = d.customer_id
         WHERE d.domain = $1",
    )
    .bind(&domain)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(owner) = owner {
        return rule(format!(
            "the domain {domain} is already registered on {owner}"
        ));
    }
    sqlx::query("INSERT INTO customer_domains (domain, customer_id) VALUES ($1, $2)")
        .bind(&domain)
        .bind(customer)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// A customer that exists and is not offboarded, locked.
async fn lock_active(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<bool> {
    let row: Option<(bool, bool)> = sqlx::query_as(
        "SELECT offboarded_at IS NOT NULL, is_help_desk FROM customers WHERE id = $1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    match row {
        None => Err(Error::NotFound("customer")),
        Some((true, _)) => rule("the customer is offboarded"),
        Some((false, help_desk)) => Ok(help_desk),
    }
}

/// Adds a customer with the mail domains of its requesters. The help desk's own organization is
/// created once, at setup, from the command line.
pub async fn create(db: &PgPool, name: &str, domains: &[String]) -> Result<Uuid> {
    let name = clean_name(name)?;
    let mut tx = db.begin().await?;
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO customers (id, name) VALUES ($1, $2)")
        .bind(id)
        .bind(name)
        .execute(&mut *tx)
        .await?;
    for domain in domains {
        add_domain_in(&mut tx, id, domain).await?;
    }
    tx.commit().await?;
    Ok(id)
}

pub async fn rename(db: &PgPool, id: Uuid, name: &str) -> Result<()> {
    let name = clean_name(name)?;
    let mut tx = db.begin().await?;
    lock_active(&mut tx, id).await?;
    sqlx::query("UPDATE customers SET name = $2 WHERE id = $1")
        .bind(id)
        .bind(name)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn add_domain(db: &PgPool, id: Uuid, domain: &str) -> Result<()> {
    let mut tx = db.begin().await?;
    lock_active(&mut tx, id).await?;
    add_domain_in(&mut tx, id, domain).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn remove_domain(db: &PgPool, id: Uuid, domain: &str) -> Result<()> {
    let removed =
        sqlx::query("DELETE FROM customer_domains WHERE customer_id = $1 AND domain = $2")
            .bind(id)
            .bind(domain.trim().to_lowercase())
            .execute(db)
            .await?
            .rows_affected();
    if removed == 0 {
        return Err(Error::NotFound("domain"));
    }
    Ok(())
}

/// Registers a requester with the customer, so that their mail opens tickets directly.
pub async fn add_requester(db: &PgPool, id: Uuid, email: &str, name: Option<&str>) -> Result<Uuid> {
    let email = email.trim().to_lowercase();
    match email.rsplit_once('@') {
        Some((local, domain)) if !local.is_empty() && domain.contains('.') => {}
        _ => return rule(format!("{email:?} is not an e-mail address")),
    }
    let mut tx = db.begin().await?;
    lock_active(&mut tx, id).await?;
    let owner: Option<String> = sqlx::query_scalar(
        "SELECT c.name FROM requesters r JOIN customers c ON c.id = r.customer_id WHERE r.email = $1",
    )
    .bind(&email)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(owner) = owner {
        return rule(format!("this address is already a requester of {owner}"));
    }
    let requester = Uuid::new_v4();
    sqlx::query("INSERT INTO requesters (id, customer_id, email, name) VALUES ($1, $2, $3, $4)")
        .bind(requester)
        .bind(id)
        .bind(&email)
        .bind(name.map(str::trim).filter(|n| !n.is_empty()))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(requester)
}

/// Offboards a customer: it gets no new tickets, and mail from its requesters and domains waits
/// in the inbox. Its history stays. The help desk's own organization cannot be offboarded.
pub async fn offboard(db: &PgPool, id: Uuid) -> Result<()> {
    let mut tx = db.begin().await?;
    if lock_active(&mut tx, id).await? {
        return rule("the help desk's own organization cannot be offboarded");
    }
    sqlx::query("UPDATE customers SET offboarded_at = now() WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;

    #[test]
    fn domains_are_cleaned_and_checked() {
        assert_eq!(
            clean_domain(" @Brightfield.TEST ").unwrap(),
            "brightfield.test"
        );
        for bad in [
            "",
            "localhost",
            "a b.test",
            "ada@brightfield.test",
            ".test",
            "x.",
        ] {
            assert!(clean_domain(bad).is_err(), "{bad:?} accepted");
        }
    }

    #[sqlx::test]
    async fn a_domain_belongs_to_one_customer(db: PgPool) {
        let brightfield = create(&db, "Brightfield", &["brightfield.test".into()])
            .await
            .unwrap();
        let harbor = create(&db, "Harbor Lane", &[]).await.unwrap();
        let Err(Error::Rule(why)) = add_domain(&db, harbor, "BRIGHTFIELD.test").await else {
            panic!("a second owner was accepted");
        };
        assert!(why.contains("Brightfield"), "{why}");
        remove_domain(&db, brightfield, "brightfield.test")
            .await
            .unwrap();
        add_domain(&db, harbor, "brightfield.test").await.unwrap();
        assert_eq!(
            detail(&db, harbor).await.unwrap().summary.domains,
            ["brightfield.test"]
        );
    }

    #[sqlx::test]
    async fn an_address_is_a_requester_of_one_customer(db: PgPool) {
        let brightfield = create(&db, "Brightfield", &[]).await.unwrap();
        let harbor = create(&db, "Harbor Lane", &[]).await.unwrap();
        add_requester(
            &db,
            brightfield,
            "Ada@Brightfield.test",
            Some("Ada Ferrini"),
        )
        .await
        .unwrap();
        assert!(matches!(
            add_requester(&db, harbor, "ada@brightfield.test", None).await,
            Err(Error::Rule(_))
        ));
        let customer = detail(&db, brightfield).await.unwrap();
        assert_eq!(customer.requester_list[0].email, "ada@brightfield.test");
        assert_eq!(customer.summary.requesters, 1);
    }

    #[sqlx::test]
    async fn offboarding_keeps_history_and_spares_the_help_desk(db: PgPool) {
        let brightfield = create(&db, "Brightfield", &[]).await.unwrap();
        testing::ticket(&db, brightfield, None, "a@brightfield.test").await;
        offboard(&db, brightfield).await.unwrap();
        assert!(matches!(
            offboard(&db, brightfield).await,
            Err(Error::Rule(_))
        ));
        assert!(matches!(
            rename(&db, brightfield, "X").await,
            Err(Error::Rule(_))
        ));
        assert_eq!(
            detail(&db, brightfield)
                .await
                .unwrap()
                .summary
                .active_tickets,
            1
        );

        let own = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO customers (id, name, is_help_desk) VALUES ($1, 'Northgate', true)",
        )
        .bind(own)
        .execute(&db)
        .await
        .unwrap();
        assert!(matches!(offboard(&db, own).await, Err(Error::Rule(_))));
        let names: Vec<String> = list(&db)
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, ["Northgate", "Brightfield"], "offboarded last");
    }
}
