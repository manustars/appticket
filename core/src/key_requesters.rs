//! Key requesters: the requesters who speak for their customer. They get the replies on its
//! tickets that have no requester and the messages about failures that affect it, at their contact
//! address. Rules are in docs/domain-rules.md ("People", "Tickets").

use serde::Serialize;
use sqlx::{PgExecutor, PgPool, Postgres, Transaction};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::{Result, rule};

/// A requester who speaks for their customer.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
pub struct KeyRequester {
    pub id: Uuid,
    pub name: Option<String>,
    /// The address the messages for key requesters go to, one of the requester's.
    pub contact_address: String,
}

/// The customer's key requesters, by contact address.
pub async fn of_customer<'e>(db: impl PgExecutor<'e>, customer: Uuid) -> Result<Vec<KeyRequester>> {
    Ok(sqlx::query_as(
        "SELECT r.id, r.name, k.contact_address
         FROM key_requesters k JOIN requesters r ON r.id = k.requester_id
         WHERE k.customer_id = $1
         ORDER BY k.contact_address",
    )
    .bind(customer)
    .fetch_all(db)
    .await?)
}

/// The key requesters of `customers`, each with their customer, locked until the transaction ends
/// so that none is unmarked or written to elsewhere meanwhile. Key requesters are locked after
/// customers and requesters and before tickets.
pub(crate) async fn locked_of_customers(
    tx: &mut Transaction<'_, Postgres>,
    customers: &[Uuid],
) -> Result<Vec<(Uuid, KeyRequester)>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        customer_id: Uuid,
        #[sqlx(flatten)]
        key: KeyRequester,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT k.customer_id, r.id, r.name, k.contact_address
         FROM key_requesters k JOIN requesters r ON r.id = k.requester_id
         WHERE k.customer_id = ANY($1)
         ORDER BY k.requester_id
         FOR SHARE OF k",
    )
    .bind(customers)
    .fetch_all(&mut **tx)
    .await?;
    let mut keys: Vec<(Uuid, KeyRequester)> =
        rows.into_iter().map(|r| (r.customer_id, r.key)).collect();
    keys.sort_by(|a, b| a.1.contact_address.cmp(&b.1.contact_address));
    Ok(keys)
}

/// Who a reply on a ticket of `customer` without a requester goes to: its key requesters, locked
/// as [`locked_of_customers`] does. With none, there is nobody to reply to and it is refused.
pub(crate) async fn to_reply_to(
    tx: &mut Transaction<'_, Postgres>,
    customer: Uuid,
) -> Result<Vec<KeyRequester>> {
    let keys: Vec<KeyRequester> = locked_of_customers(tx, &[customer])
        .await?
        .into_iter()
        .map(|(_, key)| key)
        .collect();
    if keys.is_empty() {
        return rule(
            "the ticket has no requester and its customer has no key requesters: there is nobody to reply to",
        );
    }
    Ok(keys)
}

/// Makes the requester a key requester of their customer, written to at `contact_address`, one
/// of their addresses; for a key requester already, it changes their contact address. Without
/// one, a requester with a single address is written to there, and a key requester keeps theirs.
/// A departed requester cannot be one, and an offboarded customer's requesters do not change.
pub async fn mark(db: &PgPool, requester: Uuid, contact_address: Option<&str>) -> Result<()> {
    let mut tx = db.begin().await?;
    let customer = crate::requesters::lock_with_customer(&mut tx, requester).await?;
    let (departed, current): (bool, Option<String>) = sqlx::query_as(
        "SELECT r.departed_at IS NOT NULL, k.contact_address
         FROM requesters r LEFT JOIN key_requesters k ON k.requester_id = r.id
         WHERE r.id = $1",
    )
    .bind(requester)
    .fetch_one(&mut *tx)
    .await?;
    if departed {
        return rule("a departed requester cannot be a key requester");
    }
    let addresses = crate::requesters::addresses(&mut *tx, requester).await?;
    let picked = contact_address
        .map(|a| a.trim().to_lowercase())
        .filter(|a| !a.is_empty());
    let contact = match (picked, current, addresses.as_slice()) {
        (Some(picked), _, addresses) if addresses.contains(&picked) => picked,
        (Some(picked), _, _) => {
            return rule(format!(
                "the address {picked} is not one of the requester's addresses"
            ));
        }
        (None, Some(current), _) => current,
        (None, None, [only]) => only.clone(),
        (None, None, _) => {
            return rule(
                "the requester has several addresses: pick the one key requester messages go to",
            );
        }
    };
    sqlx::query(
        "INSERT INTO key_requesters (requester_id, customer_id, contact_address)
         VALUES ($1, $2, $3)
         ON CONFLICT (requester_id) DO UPDATE SET contact_address = excluded.contact_address",
    )
    .bind(requester)
    .bind(customer)
    .bind(&contact)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Makes a key requester a plain requester of their customer again.
pub async fn unmark(db: &PgPool, requester: Uuid) -> Result<()> {
    let mut tx = db.begin().await?;
    crate::requesters::lock_with_customer(&mut tx, requester).await?;
    if unmark_in(&mut tx, requester).await?.is_none() {
        return rule("the requester is not a key requester");
    }
    tx.commit().await?;
    Ok(())
}

/// Removes the requester from their customer's key requesters, with the requester locked; returns
/// the contact address they had, if they were one.
pub(crate) async fn unmark_in(
    tx: &mut Transaction<'_, Postgres>,
    requester: Uuid,
) -> Result<Option<String>> {
    Ok(sqlx::query_scalar(
        "DELETE FROM key_requesters WHERE requester_id = $1 RETURNING contact_address",
    )
    .bind(requester)
    .fetch_optional(&mut **tx)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use crate::testing;

    /// Each requester on the customer's page, with their contact address while they are a key
    /// requester.
    async fn contacts(db: &PgPool, customer: Uuid) -> Vec<(Uuid, Option<String>)> {
        crate::customers::detail(db, customer)
            .await
            .unwrap()
            .requester_list
            .into_iter()
            .map(|r| (r.id, r.contact_address))
            .collect()
    }

    #[sqlx::test]
    async fn key_requesters_are_marked_and_unmarked(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let lena = testing::requester(&db, brightfield, "lena@brightfield.test").await;
        crate::requesters::add_address(&db, ada, "ada@home.test")
            .await
            .unwrap();

        mark(&db, ada, Some(" ADA@home.test ")).await.unwrap();
        mark(&db, lena, None).await.unwrap();
        assert_eq!(
            contacts(&db, brightfield).await,
            [
                (ada, Some("ada@home.test".to_owned())),
                (lena, Some("lena@brightfield.test".to_owned()))
            ]
        );

        // Marking again changes the contact address; unmarking leaves a plain requester.
        mark(&db, ada, Some("ada@brightfield.test")).await.unwrap();
        unmark(&db, lena).await.unwrap();
        assert_eq!(
            contacts(&db, brightfield).await,
            [(ada, Some("ada@brightfield.test".to_owned())), (lena, None)]
        );
        assert!(matches!(unmark(&db, lena).await, Err(Error::Rule(_))));
    }

    /// A key requester is written to at one of their own addresses, picked when they have several,
    /// and a departed requester is not one.
    #[sqlx::test]
    async fn a_key_requester_is_present_and_written_to_at_their_own_address(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let lena = testing::requester(&db, brightfield, "lena@brightfield.test").await;
        crate::requesters::add_address(&db, ada, "ada@home.test")
            .await
            .unwrap();
        let refused = |requester: Uuid, contact: Option<&'static str>| {
            let db = db.clone();
            async move {
                match mark(&db, requester, contact).await {
                    Err(Error::Rule(why)) => why,
                    other => panic!("marked: {other:?}"),
                }
            }
        };

        assert!(refused(ada, None).await.contains("pick"));
        assert!(
            refused(ada, Some("lena@brightfield.test"))
                .await
                .contains("not one of")
        );
        crate::requesters::depart(&db, lena).await.unwrap();
        assert!(refused(lena, None).await.contains("departed"));
        assert_eq!(
            contacts(&db, brightfield).await,
            [(ada, None), (lena, None)]
        );
    }

    /// A requester who departs stops being a key requester, and coming back does not make them
    /// one again.
    #[sqlx::test]
    async fn departing_ends_being_a_key_requester(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        mark(&db, ada, None).await.unwrap();

        crate::requesters::depart(&db, ada).await.unwrap();
        assert_eq!(contacts(&db, brightfield).await, [(ada, None)]);
        crate::requesters::bring_back(&db, ada).await.unwrap();
        assert_eq!(contacts(&db, brightfield).await, [(ada, None)]);
    }

    /// A requester moved to another customer, from their page or with one of their tickets, stops
    /// being a key requester: they are one of neither customer.
    #[sqlx::test]
    async fn moving_ends_being_a_key_requester(db: PgPool) {
        use crate::tickets::{self, RequesterMove};
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let lena = testing::requester(&db, brightfield, "lena@brightfield.test").await;
        let hers = testing::ticket(&db, brightfield, Some(lena), "l1@brightfield.test").await;
        mark(&db, ada, None).await.unwrap();
        mark(&db, lena, None).await.unwrap();
        // The agent is told before moving.
        let preview = crate::requesters::move_preview(&db, ada, harbor)
            .await
            .unwrap();
        assert!(preview.key_requester);

        crate::requesters::move_to_customer(&db, ada, harbor)
            .await
            .unwrap();
        tickets::move_to_customer(&db, hers, harbor, RequesterMove::Move)
            .await
            .unwrap();
        assert_eq!(contacts(&db, brightfield).await, []);
        let mut at_harbor = contacts(&db, harbor).await;
        at_harbor.sort();
        let mut expected = [(ada, None), (lena, None)];
        expected.sort();
        assert_eq!(at_harbor, expected);
    }

    /// While a reply is being written to a customer's key requesters, they stay key requesters:
    /// unmarking one waits until the reply is queued.
    #[sqlx::test]
    async fn key_requesters_being_written_to_are_not_unmarked_meanwhile(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let lena = testing::requester(&db, brightfield, "lena@brightfield.test").await;
        mark(&db, lena, None).await.unwrap();

        let mut writing = db.begin().await.unwrap();
        let keys = to_reply_to(&mut writing, brightfield).await.unwrap();
        assert_eq!(keys[0].contact_address, "lena@brightfield.test");
        let unmarking = tokio::spawn({
            let db = db.clone();
            async move { unmark(&db, lena).await }
        });
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert!(!unmarking.is_finished(), "unmarked while being written to");
        writing.commit().await.unwrap();
        unmarking.await.unwrap().unwrap();
        assert_eq!(contacts(&db, brightfield).await, [(lena, None)]);
    }

    /// Merging a key requester into another leaves the one that remains a key requester: written
    /// to where the merged one was, an address that passed to them, unless they were one already.
    #[sqlx::test]
    async fn a_merge_keeps_the_key_requester(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let twin = testing::requester(&db, brightfield, "a.ferrini@home.test").await;
        let lena = testing::requester(&db, brightfield, "lena@brightfield.test").await;
        let third = testing::requester(&db, brightfield, "lena.r@studio.test").await;
        mark(&db, twin, None).await.unwrap();
        mark(&db, lena, None).await.unwrap();
        mark(&db, third, None).await.unwrap();

        crate::requesters::merge(&db, ada, twin).await.unwrap();
        crate::requesters::merge(&db, lena, third).await.unwrap();
        assert_eq!(
            contacts(&db, brightfield).await,
            [
                (ada, Some("a.ferrini@home.test".to_owned())),
                (lena, Some("lena@brightfield.test".to_owned()))
            ]
        );
    }

    /// A key requester's contact address stays one of their addresses: it is not removed until
    /// an agent picks another, while their other addresses are.
    #[sqlx::test]
    async fn the_contact_address_is_not_removed(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        for address in ["ada@home.test", "ada.f@studio.test"] {
            crate::requesters::add_address(&db, ada, address)
                .await
                .unwrap();
        }
        mark(&db, ada, Some("ada@home.test")).await.unwrap();

        let Err(Error::Rule(why)) =
            crate::requesters::remove_address(&db, ada, "ada@home.test").await
        else {
            panic!("the contact address was removed");
        };
        assert!(why.contains("contact address"), "{why}");
        crate::requesters::remove_address(&db, ada, "ada.f@studio.test")
            .await
            .unwrap();
        mark(&db, ada, Some("ada@brightfield.test")).await.unwrap();
        crate::requesters::remove_address(&db, ada, "ada@home.test")
            .await
            .unwrap();
        assert_eq!(
            contacts(&db, brightfield).await,
            [(ada, Some("ada@brightfield.test".to_owned()))]
        );
    }

    /// Runs `sql` with the requester as `$1` and `text` as `$2`, as code that bypasses the core
    /// would.
    async fn run(db: &PgPool, sql: &'static str, requester: Uuid, text: &str) -> bool {
        sqlx::query(sql)
            .bind(requester)
            .bind(text.to_owned())
            .execute(db)
            .await
            .is_ok()
    }

    /// Like [`run`], for a statement the database refuses: why it does.
    async fn refusal(db: &PgPool, sql: &'static str, requester: Uuid, text: &str) -> String {
        sqlx::query(sql)
            .bind(requester)
            .bind(text.to_owned())
            .execute(db)
            .await
            .expect_err(sql)
            .to_string()
    }

    /// The key that keeps a contact address among the requester's own addresses.
    const CONTACT_KEY: &str = "key_requesters_contact_address_requester_id_fkey";

    const MARK: &str = "INSERT INTO key_requesters (requester_id, customer_id, contact_address)
                        SELECT id, customer_id, $2 FROM requesters WHERE id = $1";

    /// The database keeps a departed requester from being a key requester, also when the code is
    /// bypassed: one is not marked, and a key requester does not depart.
    #[sqlx::test]
    async fn the_database_keeps_departed_requesters_from_being_key_requesters(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let lena = testing::requester(&db, brightfield, "lena@brightfield.test").await;
        let depart = "UPDATE requesters SET departed_at = now() WHERE id = $1 AND $2 <> ''";

        assert!(run(&db, MARK, ada, "ada@brightfield.test").await);
        let why = refusal(&db, depart, ada, "x").await;
        assert!(why.contains("a key requester does not depart"), "{why}");
        assert!(run(&db, depart, lena, "x").await);
        let why = refusal(&db, MARK, lena, "lena@brightfield.test").await;
        assert!(why.contains("a departed requester is not"), "{why}");
    }

    /// The database keeps a key requester's contact address among their addresses, and a key
    /// requester with their customer, also when the code is bypassed.
    #[sqlx::test]
    async fn the_database_keeps_the_contact_address_and_the_customer(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let lena = testing::requester(&db, brightfield, "lena@brightfield.test").await;
        crate::requesters::add_address(&db, ada, "ada@home.test")
            .await
            .unwrap();

        // Written to at another requester's address, or at an address of nobody.
        for address in ["lena@brightfield.test", "ada@elsewhere.test"] {
            let why = refusal(&db, MARK, ada, address).await;
            assert!(why.contains(CONTACT_KEY), "{why}");
        }
        assert!(run(&db, MARK, ada, "ada@home.test").await);
        let why = refusal(
            &db,
            "DELETE FROM requester_addresses WHERE requester_id = $1 AND address = $2",
            ada,
            "ada@home.test",
        )
        .await;
        assert!(
            why.contains(CONTACT_KEY),
            "the contact address was removed: {why}"
        );
        // A merge is the one way an address of a requester who has not departed passes on.
        let mut tx = db.begin().await.unwrap();
        sqlx::query("INSERT INTO requester_merges (merged_id, kept_id) VALUES ($1, $2)")
            .bind(ada)
            .bind(lena)
            .execute(&mut *tx)
            .await
            .unwrap();
        let passed = sqlx::query(
            "UPDATE requester_addresses SET requester_id = $2 WHERE address = 'ada@home.test'",
        )
        .bind(ada)
        .bind(lena)
        .execute(&mut *tx)
        .await
        .expect_err("the contact address passed to another")
        .to_string();
        assert!(passed.contains(CONTACT_KEY), "{passed}");
        drop(tx);
        let why = refusal(
            &db,
            "UPDATE requesters SET customer_id = $2::uuid WHERE id = $1",
            ada,
            &harbor.to_string(),
        )
        .await;
        assert!(
            why.contains("key_requesters_requester_id_customer_id_fkey"),
            "a key requester moved to another customer: {why}"
        );
        assert_eq!(
            contacts(&db, brightfield).await,
            [(ada, Some("ada@home.test".to_owned())), (lena, None)]
        );
    }
}
