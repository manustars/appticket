//! Customers, the mail domains that identify their requesters, and the requesters themselves.
//! Rules are in docs/domain-rules.md ("Customers, assets and services", "People").

use serde::{Deserialize, Serialize};
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
    pub legal_name: Option<String>,
    /// The tax identifier, such as a VAT number; it always comes with its country.
    pub tax_id: Option<String>,
    /// The country that issued the tax identifier, an ISO 3166-1 alpha-2 code such as `IT`.
    pub tax_country: Option<String>,
    /// The customer's postal address; it can span several lines.
    pub address: Option<String>,
    pub phone: Option<String>,
    /// An `http` or `https` address.
    pub website: Option<String>,
    /// Internal notes, seen only by agents.
    pub notes: String,
    pub requester_list: Vec<RequesterEntry>,
    /// The customer's sites, active ones first.
    pub sites: Vec<crate::sites::Site>,
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct RequesterEntry {
    pub id: Uuid,
    /// The requester's e-mail addresses, the first registered first; there is at least one unless
    /// the requester has departed.
    pub addresses: Vec<String>,
    pub name: Option<String>,
    pub tickets: i64,
    /// The site the requester is at, one of the customer's sites.
    pub site_id: Option<Uuid>,
    /// The requester has left the customer.
    pub departed: bool,
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
    let details: CustomerDetails = sqlx::query_as(
        "SELECT name, legal_name, tax_id, tax_country, address, phone, website, notes
         FROM customers WHERE id = $1",
    )
    .bind(id)
    .fetch_one(db)
    .await?;
    let requester_list = sqlx::query_as(
        "SELECT r.id, r.name, r.site_id, r.departed_at IS NOT NULL AS departed,
                coalesce((SELECT array_agg(a.address ORDER BY a.created_at, a.address)
                 FROM requester_addresses a WHERE a.requester_id = r.id), '{}') AS addresses,
                (SELECT count(*) FROM tickets t WHERE t.requester_id = r.id) AS tickets
         FROM requesters r WHERE r.customer_id = $1
         ORDER BY r.departed_at IS NOT NULL, requester_first_address(r.id), r.name, r.id",
    )
    .bind(id)
    .fetch_all(db)
    .await?;
    Ok(CustomerDetail {
        summary,
        legal_name: details.legal_name,
        tax_id: details.tax_id,
        tax_country: details.tax_country,
        address: details.address,
        phone: details.phone,
        website: details.website,
        notes: details.notes,
        requester_list,
        sites: crate::sites::of(db, id).await?,
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
pub(crate) async fn lock_active(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<bool> {
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

/// What agents who manage customers correct on a customer: its name, and the details beside it,
/// each optional. A blank detail is no detail.
#[derive(Debug, Clone, Default, Deserialize, ToSchema, sqlx::FromRow)]
pub struct CustomerDetails {
    pub name: String,
    #[serde(default)]
    pub legal_name: Option<String>,
    /// The tax identifier, such as a VAT number; it always comes with its country.
    #[serde(default)]
    pub tax_id: Option<String>,
    /// The country that issued the tax identifier, an ISO 3166-1 alpha-2 code such as `IT`.
    #[serde(default)]
    pub tax_country: Option<String>,
    /// The customer's postal address; it can span several lines.
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
    /// An `http` or `https` address; one without a scheme gets `https://`.
    #[serde(default)]
    pub website: Option<String>,
    /// Internal notes, seen only by agents.
    #[serde(default)]
    pub notes: String,
}

/// Trimmed, or `None` when blank.
fn filled(text: Option<&str>) -> Option<&str> {
    text.map(str::trim).filter(|t| !t.is_empty())
}

/// An ISO 3166-1 alpha-2 code, uppercased.
fn clean_country(country: &str) -> Result<String> {
    let country = country.to_ascii_uppercase();
    if country.len() != 2 || !country.bytes().all(|b| b.is_ascii_uppercase()) {
        return rule(format!(
            "{country:?} is not a country code: use the two letters of ISO 3166-1, such as IT"
        ));
    }
    Ok(country)
}

/// A web address reached over `http` or `https`, `https` when none is given.
fn clean_website(website: &str) -> Result<String> {
    let (scheme, rest) = match website.split_once("://") {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("http") => ("http", rest),
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("https") => ("https", rest),
        Some(_) => return rule("a website starts with http:// or https://"),
        None => ("https", website),
    };
    // A host name, with a port number or not.
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let (host, port) = authority.split_once(':').unwrap_or((authority, "80"));
    let valid = !host.is_empty()
        && !host.starts_with(['.', '-'])
        && host
            .chars()
            .all(|c| c.is_alphanumeric() || c == '.' || c == '-')
        && !port.is_empty()
        && port.chars().all(|c| c.is_ascii_digit())
        && !rest.chars().any(char::is_whitespace);
    if !valid {
        return rule(format!("{website:?} is not a web address"));
    }
    Ok(format!("{scheme}://{rest}"))
}

/// Corrects a customer's name and details. An offboarded customer's do not change.
pub async fn update(db: &PgPool, id: Uuid, details: &CustomerDetails) -> Result<()> {
    let name = clean_name(&details.name)?;
    let tax_id = filled(details.tax_id.as_deref());
    let tax_country = filled(details.tax_country.as_deref())
        .map(clean_country)
        .transpose()?;
    if tax_id.is_some() != tax_country.is_some() {
        return rule("a tax identifier always comes with its country");
    }
    let website = filled(details.website.as_deref())
        .map(clean_website)
        .transpose()?;
    let mut tx = db.begin().await?;
    lock_active(&mut tx, id).await?;
    sqlx::query(
        "UPDATE customers SET name = $2, legal_name = $3, tax_id = $4, tax_country = $5,
                address = $6, phone = $7, website = $8, notes = $9
         WHERE id = $1",
    )
    .bind(id)
    .bind(name)
    .bind(filled(details.legal_name.as_deref()))
    .bind(tax_id)
    .bind(tax_country)
    .bind(filled(details.address.as_deref()))
    .bind(filled(details.phone.as_deref()))
    .bind(website)
    .bind(details.notes.trim())
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

/// Registers a requester with the customer, so that their mail opens tickets directly. An
/// address already belonging to a requester is refused, unless that requester departed from
/// another customer: then it passes to the new requester.
pub async fn add_requester(db: &PgPool, id: Uuid, email: &str, name: Option<&str>) -> Result<Uuid> {
    let email = crate::requesters::clean_address(email)?;
    let mut tx = db.begin().await?;
    lock_active(&mut tx, id).await?;
    let name = name.map(str::trim).filter(|n| !n.is_empty());
    let requester = crate::requesters::create(&mut tx, id, &email, name).await?;
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

/// Brings an offboarded customer back, with its history, requesters and assets: from then on mail
/// from its requesters and domains opens tickets again, and it is offered for new tickets.
pub async fn bring_back(db: &PgPool, id: Uuid) -> Result<()> {
    let brought_back = sqlx::query(
        "UPDATE customers SET offboarded_at = NULL WHERE id = $1 AND offboarded_at IS NOT NULL",
    )
    .bind(id)
    .execute(db)
    .await?
    .rows_affected();
    if brought_back == 0 {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM customers WHERE id = $1)")
                .bind(id)
                .fetch_one(db)
                .await?;
        return if exists {
            rule("the customer is not offboarded")
        } else {
            Err(Error::NotFound("customer"))
        };
    }
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

    fn details(name: &str) -> CustomerDetails {
        CustomerDetails {
            name: name.into(),
            ..CustomerDetails::default()
        }
    }

    #[sqlx::test]
    async fn a_customer_s_details_are_corrected_and_cleared(db: PgPool) {
        let brightfield = create(&db, "Brightfield", &[]).await.unwrap();
        let filled = CustomerDetails {
            name: " Brightfield Labs ".into(),
            legal_name: Some(" Brightfield Labs S.r.l. ".into()),
            tax_id: Some(" 00000000000 ".into()),
            tax_country: Some(" it ".into()),
            address: Some("Via dei Tigli 1\nValfiorita".into()),
            phone: Some("000 000 0100".into()),
            website: Some("brightfield.test".into()),
            notes: "  Ask for Ada first.\n".into(),
        };
        update(&db, brightfield, &filled).await.unwrap();
        let shown = detail(&db, brightfield).await.unwrap();
        assert_eq!(shown.summary.name, "Brightfield Labs");
        assert_eq!(shown.legal_name.as_deref(), Some("Brightfield Labs S.r.l."));
        assert_eq!(shown.tax_id.as_deref(), Some("00000000000"));
        assert_eq!(shown.tax_country.as_deref(), Some("IT"));
        assert_eq!(
            shown.address.as_deref(),
            Some("Via dei Tigli 1\nValfiorita")
        );
        assert_eq!(shown.phone.as_deref(), Some("000 000 0100"));
        assert_eq!(
            shown.website.as_deref(),
            Some("https://brightfield.test"),
            "a website without a scheme is reached over https"
        );
        assert_eq!(shown.notes, "Ask for Ada first.");

        // Clearing a field removes its value; a blank one is no value.
        let cleared = CustomerDetails {
            legal_name: Some("  ".into()),
            ..details("Brightfield Labs")
        };
        update(&db, brightfield, &cleared).await.unwrap();
        let shown = detail(&db, brightfield).await.unwrap();
        assert_eq!(
            (
                shown.legal_name,
                shown.tax_id,
                shown.tax_country,
                shown.address,
                shown.phone,
                shown.website,
                shown.notes
            ),
            (None, None, None, None, None, None, String::new())
        );
        assert!(matches!(
            update(&db, brightfield, &details(" ")).await,
            Err(Error::Rule(_))
        ));
    }

    #[sqlx::test]
    async fn a_tax_identifier_comes_with_its_country(db: PgPool) {
        let brightfield = create(&db, "Brightfield", &[]).await.unwrap();
        for (tax_id, tax_country) in [(Some("00000000000"), None), (None, Some("IT"))] {
            let half = CustomerDetails {
                tax_id: tax_id.map(Into::into),
                tax_country: tax_country.map(Into::into),
                ..details("Brightfield")
            };
            let Err(Error::Rule(why)) = update(&db, brightfield, &half).await else {
                panic!("{tax_id:?} {tax_country:?} accepted");
            };
            assert!(why.contains("country"), "{why}");
        }
        for country in ["ITA", "I", "1T", "ÌT", "ß"] {
            let wrong = CustomerDetails {
                tax_id: Some("00000000000".into()),
                tax_country: Some(country.into()),
                ..details("Brightfield")
            };
            assert!(
                matches!(update(&db, brightfield, &wrong).await, Err(Error::Rule(_))),
                "{country:?} accepted"
            );
        }
    }

    #[test]
    fn a_website_is_reached_over_the_web() {
        assert_eq!(
            clean_website("HTTP://brightfield.test/support").unwrap(),
            "http://brightfield.test/support"
        );
        for bad in [
            "javascript:alert(1)",
            "ftp://brightfield.test",
            "https://",
            "https:///path",
            "bright field.test",
        ] {
            assert!(clean_website(bad).is_err(), "{bad:?} accepted");
        }
    }

    /// The database holds the rules on its own, for whatever writes to it.
    #[sqlx::test]
    async fn the_database_keeps_customer_details_clean(db: PgPool) {
        let brightfield = create(&db, "Brightfield", &[]).await.unwrap();
        for (set, why) in [
            (
                "tax_id = '00000000000'",
                "a tax identifier without its country",
            ),
            ("tax_country = 'IT'", "a country without a tax identifier"),
            (
                "tax_id = '00000000000', tax_country = 'it'",
                "a country not in capitals",
            ),
            (
                "tax_id = '00000000000', tax_country = 'ITA'",
                "a country of three letters",
            ),
            ("legal_name = ''", "a blank legal name"),
            ("address = 'Via dei Tigli 1\n'", "an address not trimmed"),
            ("phone = ' '", "a blank phone number"),
            (
                "website = 'javascript:alert(1)'",
                "a website not on the web",
            ),
            ("website = 'brightfield.test'", "a website without a scheme"),
            ("notes = ' notes'", "notes not trimmed"),
        ] {
            // The statements are the fixed ones above.
            let result = sqlx::query(sqlx::AssertSqlSafe(format!(
                "UPDATE customers SET {set} WHERE id = $1"
            )))
            .bind(brightfield)
            .execute(&db)
            .await;
            assert!(
                matches!(&result, Err(sqlx::Error::Database(e)) if e.is_check_violation()),
                "{why} was accepted: {result:?}"
            );
        }
        sqlx::query(
            "UPDATE customers SET tax_id = '00000000000', tax_country = 'IT',
                    website = 'https://brightfield.test/help', notes = ''
             WHERE id = $1",
        )
        .bind(brightfield)
        .execute(&db)
        .await
        .unwrap();
    }

    #[sqlx::test]
    async fn an_offboarded_customer_comes_back_with_its_history(db: PgPool) {
        let brightfield = create(&db, "Brightfield", &["brightfield.test".into()])
            .await
            .unwrap();
        add_requester(&db, brightfield, "ada@brightfield.test", None)
            .await
            .unwrap();
        testing::ticket(&db, brightfield, None, "a@brightfield.test").await;
        let laptop = testing::asset(&db, brightfield, "Laptop", "BF-LT-01").await;
        assert!(
            matches!(bring_back(&db, brightfield).await, Err(Error::Rule(_))),
            "a customer that is not offboarded cannot come back"
        );
        assert!(matches!(
            bring_back(&db, Uuid::new_v4()).await,
            Err(Error::NotFound(_))
        ));

        offboard(&db, brightfield).await.unwrap();
        bring_back(&db, brightfield).await.unwrap();
        let back = detail(&db, brightfield).await.unwrap();
        assert_eq!(back.summary.offboarded_at, None);
        assert_eq!(
            (
                back.summary.domains.as_slice(),
                back.summary.requesters,
                back.summary.active_tickets
            ),
            (["brightfield.test".to_owned()].as_slice(), 1, 1)
        );
        let assets = crate::catalog::assets_of(&db, brightfield).await.unwrap();
        assert_eq!(
            assets
                .iter()
                .map(|a| (a.id, a.retired_at.is_none()))
                .collect::<Vec<_>>(),
            [(laptop, true)],
            "its assets stay, not retired"
        );
        // It changes again and gets tickets again, by hand too.
        update(&db, brightfield, &details("Brightfield Labs"))
            .await
            .unwrap();
        let agent = testing::agent(&db, "marco@northgate.test").await;
        crate::tickets::open(
            &db,
            brightfield,
            "Printer jams",
            Some("ada@brightfield.test"),
            None,
            agent,
        )
        .await
        .unwrap();
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
        assert_eq!(
            customer.requester_list[0].addresses,
            ["ada@brightfield.test"]
        );
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
            update(&db, brightfield, &details("X")).await,
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
