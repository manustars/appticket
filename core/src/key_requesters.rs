//! Key requesters: the requesters who speak for their customer. They get the replies on its
//! tickets that have no requester and the messages about failures that affect it, at their contact
//! address. Rules are in docs/domain-rules.md ("People", "Tickets").

use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::{Result, rule};

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
}
