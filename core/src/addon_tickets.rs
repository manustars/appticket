//! What an add-on with the scope `tickets:write` does on tickets (docs/domain-rules.md,
//! "Add-ons"): it opens tickets without a requester for a customer, with a priority, assets and
//! services, and adds internal notes to any ticket. It never replies and changes nothing else.

use serde::Deserialize;
use sqlx::{PgPool, Postgres, Transaction};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::{Error, Result, rule};
use crate::tickets::Priority;

/// A ticket an add-on opens.
#[derive(Debug, Deserialize, ToSchema)]
pub struct NewTicket {
    /// The customer the ticket is for; an offboarded customer gets no new tickets.
    pub customer_id: Uuid,
    pub title: String,
    /// What the add-on knows of the problem, such as the alert it saw. It becomes the ticket's
    /// first internal note, by the add-on: seen by agents only, never sent to the customer.
    #[serde(default)]
    pub description: Option<String>,
    /// Normal when omitted.
    #[serde(default)]
    pub priority: Option<Priority>,
    /// Active assets of the customer the ticket is about.
    #[serde(default)]
    pub asset_ids: Vec<Uuid>,
    /// Active services the customer uses that the ticket is about.
    #[serde(default)]
    pub service_ids: Vec<Uuid>,
}

/// Opens a ticket without a requester for `addon`; returns its number. The ticket, its links and
/// its description are written together or not at all: an asset or service that does not fit
/// refuses the whole ticket, with a sentence naming it.
pub async fn open(db: &PgPool, addon: Uuid, new: &NewTicket) -> Result<i64> {
    let title = new.title.trim();
    if title.is_empty() {
        return rule("a ticket needs a title");
    }
    let mut tx = db.begin().await?;
    // Shared locks hold the customer, assets and services as checked until the ticket links them:
    // offboarding the customer or retiring one of them waits for it.
    let customer: Option<(String, bool)> = sqlx::query_as(
        "SELECT name, offboarded_at IS NOT NULL FROM customers WHERE id = $1 FOR SHARE",
    )
    .bind(new.customer_id)
    .fetch_optional(&mut *tx)
    .await?;
    let customer_name = match customer {
        None => return Err(Error::NotFound("customer")),
        Some((name, true)) => {
            return rule(format!("{name} is offboarded and gets no new tickets"));
        }
        Some((name, false)) => name,
    };
    for &asset in &new.asset_ids {
        let row: Option<(Uuid, String, bool)> = sqlx::query_as(
            "SELECT customer_id, name, retired_at IS NOT NULL FROM assets WHERE id = $1 FOR SHARE",
        )
        .bind(asset)
        .fetch_optional(&mut *tx)
        .await?;
        match row {
            None => return rule(format!("there is no asset {asset}")),
            Some((owner, name, _)) if owner != new.customer_id => {
                return rule(format!(
                    "the asset {name} belongs to another customer than {customer_name}"
                ));
            }
            Some((_, name, true)) => {
                return rule(format!(
                    "the asset {name} is retired and gets no new tickets"
                ));
            }
            Some(_) => {}
        }
    }
    for &service in &new.service_ids {
        let row: Option<(String, bool, bool)> = sqlx::query_as(
            "SELECT s.name, s.retired_at IS NOT NULL,
                    EXISTS (SELECT 1 FROM customer_services cs
                            WHERE cs.service_id = s.id AND cs.customer_id = $2)
             FROM services s WHERE s.id = $1 FOR SHARE",
        )
        .bind(service)
        .bind(new.customer_id)
        .fetch_optional(&mut *tx)
        .await?;
        match row {
            None => return rule(format!("there is no service {service}")),
            Some((name, true, _)) => {
                return rule(format!(
                    "the service {name} is retired and gets no new tickets"
                ));
            }
            Some((name, false, false)) => {
                return rule(format!("{customer_name} does not use the service {name}"));
            }
            Some(_) => {}
        }
    }
    let id = Uuid::new_v4();
    let number: i64 = sqlx::query_scalar(
        "INSERT INTO tickets (id, customer_id, title, priority, opened_by_addon_id)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING number",
    )
    .bind(id)
    .bind(new.customer_id)
    .bind(title)
    .bind(new.priority.unwrap_or(Priority::Normal))
    .bind(addon)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO ticket_assets (ticket_id, customer_id, asset_id)
         SELECT $1, $2, unnest($3::uuid[]) ON CONFLICT DO NOTHING",
    )
    .bind(id)
    .bind(new.customer_id)
    .bind(&new.asset_ids)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO ticket_services (ticket_id, customer_id, service_id)
         SELECT $1, $2, unnest($3::uuid[]) ON CONFLICT DO NOTHING",
    )
    .bind(id)
    .bind(new.customer_id)
    .bind(&new.service_ids)
    .execute(&mut *tx)
    .await?;
    if let Some(description) = new.description.as_deref().map(str::trim)
        && !description.is_empty()
    {
        insert_note(&mut tx, id, addon, description).await?;
    }
    crate::tickets::emit_opened(&mut tx, number).await?;
    tx.commit().await?;
    Ok(number)
}

/// Adds an internal note by `addon` to any ticket, closed ones too.
pub async fn add_note(db: &PgPool, addon: Uuid, number: i64, body: &str) -> Result<Uuid> {
    let body = body.trim();
    if body.is_empty() {
        return rule("a note cannot be empty");
    }
    let mut tx = db.begin().await?;
    let (ticket, _) = crate::tickets::lock(&mut tx, number).await?;
    let id = insert_note(&mut tx, ticket, addon, body).await?;
    sqlx::query("UPDATE tickets SET updated_at = now() WHERE id = $1")
        .bind(ticket)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(id)
}

async fn insert_note(
    tx: &mut Transaction<'_, Postgres>,
    ticket: Uuid,
    addon: Uuid,
    body: &str,
) -> Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO notes (id, ticket_id, addon_id, body) VALUES ($1, $2, $3, $4)")
        .bind(id)
        .bind(ticket)
        .bind(addon)
        .bind(body)
        .execute(&mut **tx)
        .await?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::testing;
    use crate::tickets::{TicketState, TimelineEntry, detail};

    async fn addon(db: &PgPool, name: &str) -> Uuid {
        crate::addons::register(
            db,
            name,
            "http://127.0.0.1:9",
            &[],
            &[crate::addons::TICKETS_WRITE.into()],
        )
        .await
        .unwrap()
        .id
    }

    fn ticket_for(customer: Uuid, title: &str) -> NewTicket {
        NewTicket {
            customer_id: customer,
            title: title.to_owned(),
            description: None,
            priority: None,
            asset_ids: Vec::new(),
            service_ids: Vec::new(),
        }
    }

    /// The ticket an add-on opens has no requester and no assignee, the priority, assets and
    /// services it gave, the add-on as its opener, and the description as the add-on's note.
    #[sqlx::test]
    async fn an_addon_opens_a_ticket_without_a_requester(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let pc = testing::asset(&db, brightfield, "Server", "SRV-01").await;
        let mail = crate::catalog::create_service(&db, "Hosted mail", "")
            .await
            .unwrap();
        crate::catalog::use_service(&db, mail, brightfield)
            .await
            .unwrap();
        let watchtower = addon(&db, "watchtower").await;

        let number = open(
            &db,
            watchtower,
            &NewTicket {
                description: Some("  Disk /var is 97% full.  ".into()),
                priority: Some(Priority::High),
                asset_ids: vec![pc],
                service_ids: vec![mail],
                ..ticket_for(brightfield, "SRV-01: disk almost full")
            },
        )
        .await
        .unwrap();

        let ticket = detail(&db, number).await.unwrap();
        assert_eq!(ticket.title, "SRV-01: disk almost full");
        assert_eq!(ticket.customer.id, brightfield);
        assert!(ticket.requester.is_none());
        assert!(ticket.assignee.is_none());
        assert_eq!(ticket.state, TicketState::Open);
        assert_eq!(ticket.priority, Priority::High);
        assert_eq!(ticket.opened_by_addon.as_deref(), Some("watchtower"));
        assert_eq!(ticket.assets.iter().map(|a| a.id).collect::<Vec<_>>(), [pc]);
        assert_eq!(
            ticket.services.iter().map(|s| s.id).collect::<Vec<_>>(),
            [mail]
        );
        match &ticket.timeline[..] {
            [TimelineEntry::Note(note)] => {
                assert_eq!(note.body, "Disk /var is 97% full.");
                assert_eq!(note.author_name, "watchtower");
                assert!(note.by_addon);
            }
            other => panic!("{} entries", other.len()),
        }

        let plain = open(&db, watchtower, &ticket_for(brightfield, "Backup failed"))
            .await
            .unwrap();
        let ticket = detail(&db, plain).await.unwrap();
        assert_eq!(ticket.priority, Priority::Normal, "the default");
        assert!(ticket.timeline.is_empty(), "no description, no note");
    }

    async fn tickets(db: &PgPool) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM tickets")
            .fetch_one(db)
            .await
            .unwrap()
    }

    /// Only an active customer gets the ticket, and only with its own assets and the services it
    /// uses, none retired; the refusal names what does not fit, and nothing is opened.
    #[sqlx::test]
    async fn what_does_not_fit_the_customer_refuses_the_ticket(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let gone = testing::customer(&db, "Old Mill").await;
        crate::customers::offboard(&db, gone).await.unwrap();
        let ours = testing::asset(&db, brightfield, "Server", "SRV-01").await;
        let theirs = testing::asset(&db, harbor, "Server", "HL-SRV-07").await;
        let old = testing::asset(&db, brightfield, "Server", "SRV-00").await;
        crate::catalog::retire_asset(&db, old).await.unwrap();
        let mail = crate::catalog::create_service(&db, "Hosted mail", "")
            .await
            .unwrap();
        crate::catalog::use_service(&db, mail, brightfield)
            .await
            .unwrap();
        let backup = crate::catalog::create_service(&db, "Backup", "")
            .await
            .unwrap();
        let fax = crate::catalog::create_service(&db, "Fax gateway", "")
            .await
            .unwrap();
        crate::catalog::use_service(&db, fax, brightfield)
            .await
            .unwrap();
        crate::catalog::retire_service(&db, fax).await.unwrap();
        let watchtower = addon(&db, "watchtower").await;
        let nowhere = Uuid::new_v4();

        let refusals = [
            (ticket_for(gone, "Disk full"), "Old Mill is offboarded"),
            (ticket_for(brightfield, "   "), "a ticket needs a title"),
            (
                NewTicket {
                    asset_ids: vec![ours, theirs],
                    ..ticket_for(brightfield, "Disk full")
                },
                "the asset HL-SRV-07 belongs to another customer than Brightfield",
            ),
            (
                NewTicket {
                    asset_ids: vec![old],
                    ..ticket_for(brightfield, "Disk full")
                },
                "the asset SRV-00 is retired",
            ),
            (
                NewTicket {
                    asset_ids: vec![nowhere],
                    ..ticket_for(brightfield, "Disk full")
                },
                "there is no asset",
            ),
            (
                NewTicket {
                    service_ids: vec![mail, backup],
                    ..ticket_for(brightfield, "Disk full")
                },
                "Brightfield does not use the service Backup",
            ),
            (
                NewTicket {
                    service_ids: vec![fax],
                    ..ticket_for(brightfield, "Disk full")
                },
                "the service Fax gateway is retired",
            ),
            (
                NewTicket {
                    service_ids: vec![nowhere],
                    ..ticket_for(brightfield, "Disk full")
                },
                "there is no service",
            ),
        ];
        for (new, expected) in refusals {
            match open(&db, watchtower, &new).await {
                Err(Error::Rule(why)) => assert!(why.starts_with(expected), "{why}"),
                other => panic!("{expected}: {other:?}"),
            }
        }
        assert!(matches!(
            open(&db, watchtower, &ticket_for(nowhere, "Disk full")).await,
            Err(Error::NotFound("customer"))
        ));
        assert_eq!(tickets(&db).await, 0, "nothing was opened");
        let events: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(events, 0);

        open(&db, watchtower, &ticket_for(brightfield, "Disk full"))
            .await
            .unwrap();
        let (kind, number): (String, i64) =
            sqlx::query_as("SELECT kind, (data->>'ticket_number')::bigint FROM events")
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(kind, crate::addons::TICKET_OPENED);
        assert_eq!(number, detail_number(&db).await);
    }

    /// An add-on adds notes to any ticket, one a requester opened or a closed one too, shown with
    /// the add-on as author next to the agents' notes; an empty note is refused.
    #[sqlx::test]
    async fn an_addon_adds_notes_to_any_ticket(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let marco = testing::agent(&db, "marco@northgate.test").await;
        let n = testing::ticket(&db, brightfield, Some(ada), "a1@brightfield.test").await;
        crate::tickets::add_note(&db, n, marco, "Asked Ada to reboot.")
            .await
            .unwrap();
        let watchtower = addon(&db, "watchtower").await;

        add_note(&db, watchtower, n, "  The disk is back to 60%.  ")
            .await
            .unwrap();
        crate::tickets::set_state(&db, n, TicketState::Closed)
            .await
            .unwrap();
        add_note(&db, watchtower, n, "Recovered again.")
            .await
            .unwrap();

        let notes: Vec<(String, bool, String)> = detail(&db, n)
            .await
            .unwrap()
            .timeline
            .into_iter()
            .filter_map(|entry| match entry {
                TimelineEntry::Note(note) => Some((note.author_name, note.by_addon, note.body)),
                _ => None,
            })
            .collect();
        assert_eq!(
            notes,
            [
                ("marco".into(), false, "Asked Ada to reboot.".into()),
                ("watchtower".into(), true, "The disk is back to 60%.".into()),
                ("watchtower".into(), true, "Recovered again.".into()),
            ]
        );
        assert!(matches!(
            add_note(&db, watchtower, n, "  ").await,
            Err(Error::Rule(_))
        ));
        assert!(matches!(
            add_note(&db, watchtower, 9999, "Recovered.").await,
            Err(Error::NotFound("ticket"))
        ));
    }

    /// An agent's reply on a ticket an add-on opened goes to the customer's key requesters, as on
    /// any ticket without a requester, and the ticket stays without one.
    #[sqlx::test]
    async fn replies_on_an_addon_s_ticket_go_to_the_key_requesters(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let lena = testing::requester(&db, brightfield, "lena@brightfield.test").await;
        crate::key_requesters::mark(&db, lena, None).await.unwrap();
        testing::requester(&db, brightfield, "nora@brightfield.test").await;
        let marco = testing::agent(&db, "marco@northgate.test").await;
        let watchtower = addon(&db, "watchtower").await;
        let n = open(&db, watchtower, &ticket_for(brightfield, "Backup failed"))
            .await
            .unwrap();

        crate::tickets::reply(
            &db,
            &testing::sender(),
            n,
            marco,
            "Last night's backup failed; we are rerunning it.",
            None,
            TicketState::Waiting,
        )
        .await
        .unwrap();
        let ticket = detail(&db, n).await.unwrap();
        let to: Vec<Option<String>> = ticket
            .timeline
            .into_iter()
            .filter_map(|entry| match entry {
                TimelineEntry::Message(m) => Some(m.to_address),
                _ => None,
            })
            .collect();
        assert_eq!(to, [Some("lena@brightfield.test".to_owned())]);
        assert!(ticket.requester.is_none());
    }

    /// The database keeps the rules also when the code is bypassed: a note has exactly one
    /// author, an agent or an add-on, and a ticket an add-on opened has no requester.
    #[sqlx::test]
    async fn the_database_keeps_one_author_per_note_and_addon_tickets_without_requester(
        db: PgPool,
    ) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let marco = testing::agent(&db, "marco@northgate.test").await;
        let watchtower = addon(&db, "watchtower").await;
        let n = testing::ticket(&db, brightfield, None, "a1@brightfield.test").await;
        let note = |agent: Option<Uuid>, addon: Option<Uuid>| {
            let db = db.clone();
            async move {
                sqlx::query(
                    "INSERT INTO notes (id, ticket_id, author_id, addon_id, body)
                     SELECT $1, id, $3, $4, 'Checked.' FROM tickets WHERE number = $2",
                )
                .bind(Uuid::new_v4())
                .bind(n)
                .bind(agent)
                .bind(addon)
                .execute(&db)
                .await
                .is_ok()
            }
        };
        assert!(note(Some(marco), None).await);
        assert!(note(None, Some(watchtower)).await);
        assert!(!note(Some(marco), Some(watchtower)).await, "two authors");
        assert!(!note(None, None).await, "no author");

        let opened = |requester: Option<Uuid>| {
            let db = db.clone();
            async move {
                sqlx::query(
                    "INSERT INTO tickets (id, customer_id, requester_id, title, opened_by_addon_id)
                     VALUES ($1, $2, $3, 'Disk full', $4)",
                )
                .bind(Uuid::new_v4())
                .bind(brightfield)
                .bind(requester)
                .bind(watchtower)
                .execute(&db)
                .await
                .is_ok()
            }
        };
        assert!(opened(None).await);
        assert!(
            !opened(Some(ada)).await,
            "an add-on's ticket with a requester"
        );
    }

    async fn detail_number(db: &PgPool) -> i64 {
        sqlx::query_scalar("SELECT number FROM tickets")
            .fetch_one(db)
            .await
            .unwrap()
    }
}
