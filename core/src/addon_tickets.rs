//! What an add-on with the scope `tickets:write` does on tickets (docs/domain-rules.md,
//! "Add-ons"): it opens tickets without a requester for a customer, with a priority, assets and
//! services, adds internal notes to any ticket and resolves the tickets it opened. It never replies
//! and changes nothing else.

use serde::Deserialize;
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::{Error, Result, rule};
use crate::tickets::{NoteAuthor, Priority, StateChanger, TicketState};

/// A ticket an add-on opens.
#[derive(Debug, Deserialize, ToSchema)]
#[schema(as = AddonNewTicket)]
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
    match customer {
        None => return Err(Error::NotFound("customer")),
        Some((name, true)) => {
            return rule(format!("{name} is offboarded and gets no new tickets"));
        }
        Some((_, false)) => {}
    }
    for &asset in &new.asset_ids {
        crate::catalog::ensure_asset_fits(&mut tx, asset, new.customer_id).await?;
    }
    for &service in &new.service_ids {
        crate::catalog::ensure_service_fits(&mut tx, service, new.customer_id).await?;
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
        crate::tickets::insert_note(&mut tx, id, NoteAuthor::Addon(addon), description).await?;
    }
    crate::tickets::emit_opened(&mut tx, number).await?;
    tx.commit().await?;
    Ok(number)
}

/// Adds an internal note by `addon` to any ticket, closed ones too.
pub async fn add_note(db: &PgPool, addon: Uuid, number: i64, body: &str) -> Result<Uuid> {
    crate::tickets::write_note(db, number, NoteAuthor::Addon(addon), body).await
}

/// Resolves a ticket `addon` opened, open or waiting, with `ticket.state_changed` naming the
/// add-on; a ticket already resolved stays as it is. A ticket an agent, a message or another
/// add-on opened is refused, and so is a closed one. Whether an agent has worked on the ticket is
/// for the add-on to weigh (`worked_on_by_agent` in its view of the ticket), not a refusal.
pub async fn resolve(db: &PgPool, addon: Uuid, number: i64) -> Result<()> {
    let mut tx = db.begin().await?;
    let (ticket, state, opener, name): (Uuid, TicketState, Option<Uuid>, String) = sqlx::query_as(
        "SELECT t.id, t.state, t.opened_by_addon_id, a.name
         FROM tickets t, addons a WHERE t.number = $1 AND a.id = $2 FOR UPDATE OF t",
    )
    .bind(number)
    .bind(addon)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Error::NotFound("ticket"))?;
    if opener != Some(addon) {
        return rule(format!(
            "the add-on {name} did not open the ticket #{number}: an add-on resolves only the \
             tickets it opened"
        ));
    }
    match state {
        TicketState::Closed => return rule("the ticket is closed; a closed ticket cannot change"),
        TicketState::Resolved => return Ok(()),
        TicketState::Open | TicketState::Waiting => {}
    }
    crate::tickets::change_state(
        &mut tx,
        ticket,
        TicketState::Resolved,
        StateChanger::Addon(addon),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::testing;
    use crate::tickets::{TimelineEntry, detail};

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
        let data_centre = crate::sites::create(&db, brightfield, "Data centre", "Via dei Tigli 1")
            .await
            .unwrap();
        let server = testing::asset_kind(&db, "Server").await;
        let pc = crate::catalog::create_asset(
            &db,
            brightfield,
            &testing::asset_details(server, "SRV-01", None, Some(data_centre), None),
        )
        .await
        .unwrap();
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
        let at = crate::tickets::sites(&db, &[ticket.id]).await.unwrap();
        assert_eq!(
            at[&ticket.id].iter().map(|s| s.id).collect::<Vec<_>>(),
            [data_centre],
            "at the site of its asset"
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

    async fn ticket_count(db: &PgPool) -> i64 {
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
            (
                ticket_for(gone, "Disk full"),
                "Old Mill is offboarded and gets no new tickets",
            ),
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
        ];
        for (new, expected) in refusals {
            match open(&db, watchtower, &new).await {
                Err(Error::Rule(why)) => assert_eq!(why, expected),
                other => panic!("{expected}: {other:?}"),
            }
        }
        // Something that does not exist is not found, as when an agent links it.
        let missing = [
            (ticket_for(nowhere, "Disk full"), "customer"),
            (
                NewTicket {
                    asset_ids: vec![ours, nowhere],
                    ..ticket_for(brightfield, "Disk full")
                },
                "asset",
            ),
            (
                NewTicket {
                    service_ids: vec![mail, nowhere],
                    ..ticket_for(brightfield, "Disk full")
                },
                "service",
            ),
        ];
        for (new, what) in missing {
            match open(&db, watchtower, &new).await {
                Err(Error::NotFound(found)) => assert_eq!(found, what),
                other => panic!("{what}: {other:?}"),
            }
        }
        assert_eq!(ticket_count(&db).await, 0, "nothing was opened");
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
        assert_eq!(number, only_ticket_number(&db).await);
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
        testing::set_state(&db, n, TicketState::Closed)
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

        // Lena's message back joins the ticket by its headers; it still has no requester.
        let sent: String =
            sqlx::query_scalar("SELECT rfc_message_id FROM messages WHERE direction = 'outbound'")
                .fetch_one(&db)
                .await
                .unwrap();
        let raw = format!(
            "From: Lena <lena@brightfield.test>\r\nTo: helpdesk@helpdesk.test\r\nMessage-ID: <l1@brightfield.test>\r\nIn-Reply-To: <{sent}>\r\nSubject: Re: Backup failed\r\n\r\nThanks.\r\n"
        );
        let mut tx = db.begin().await.unwrap();
        crate::mail::store(&mut tx, 1, raw.as_bytes())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let ticket = detail(&db, n).await.unwrap();
        assert_eq!(ticket.timeline.len(), 2, "her message joined the ticket");
        assert!(ticket.requester.is_none());
        assert_eq!(ticket.state, TicketState::Open, "her message reopened it");
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

    /// An add-on resolves an open or waiting ticket it opened, and `ticket.state_changed` names
    /// it; resolving it again changes nothing and records nothing, and a closed one is refused.
    #[sqlx::test]
    async fn an_addon_resolves_the_tickets_it_opened(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let watchtower = addon(&db, "watchtower").await;
        let disk = open(
            &db,
            watchtower,
            &ticket_for(brightfield, "SRV-01: disk full"),
        )
        .await
        .unwrap();
        let backup = open(&db, watchtower, &ticket_for(brightfield, "Backup failed"))
            .await
            .unwrap();
        testing::set_state(&db, backup, TicketState::Waiting)
            .await
            .unwrap();
        let done = open(&db, watchtower, &ticket_for(brightfield, "UPS on battery"))
            .await
            .unwrap();
        testing::set_state(&db, done, TicketState::Closed)
            .await
            .unwrap();
        let before = testing::state_changes(&db).await.len();

        resolve(&db, watchtower, disk).await.unwrap();
        resolve(&db, watchtower, backup).await.unwrap();
        resolve(&db, watchtower, disk).await.unwrap();

        assert_eq!(testing::state(&db, disk).await, "resolved");
        assert_eq!(testing::state(&db, backup).await, "resolved");
        let by = serde_json::json!({"kind": "addon", "addon": "watchtower"});
        assert_eq!(
            testing::state_changes(&db).await[before..],
            [
                serde_json::json!({"ticket_number": disk, "old_state": "open",
                    "new_state": "resolved", "changed_by": by}),
                serde_json::json!({"ticket_number": backup, "old_state": "waiting",
                    "new_state": "resolved", "changed_by": by}),
            ],
            "nothing for the second time"
        );
        match resolve(&db, watchtower, done).await {
            Err(Error::Rule(why)) => {
                assert_eq!(why, "the ticket is closed; a closed ticket cannot change");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(testing::state(&db, done).await, "closed");
        assert!(matches!(
            resolve(&db, watchtower, 9999).await,
            Err(Error::NotFound("ticket"))
        ));
    }

    /// A ticket an agent opened, one a message opened and one another add-on opened are refused,
    /// and stay as they were.
    #[sqlx::test]
    async fn an_addon_resolves_no_ticket_it_did_not_open(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let marco = testing::agent(&db, "marco@northgate.test").await;
        let watchtower = addon(&db, "watchtower").await;
        let pinger = addon(&db, "pinger").await;
        let by_hand = crate::tickets::open(&db, brightfield, "Phone call", None, None, marco)
            .await
            .unwrap();
        let raw = "From: Ada <ada@brightfield.test>\r\nTo: helpdesk@helpdesk.test\r\nMessage-ID: <a1@brightfield.test>\r\nSubject: Printer jams\r\n\r\nIt jams.\r\n";
        let mut tx = db.begin().await.unwrap();
        crate::mail::store(&mut tx, 1, raw.as_bytes())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let by_message: i64 =
            sqlx::query_scalar("SELECT number FROM tickets WHERE title = 'Printer jams'")
                .fetch_one(&db)
                .await
                .unwrap();
        let by_another = open(&db, pinger, &ticket_for(brightfield, "Host unreachable"))
            .await
            .unwrap();

        for n in [by_hand, by_message, by_another] {
            match resolve(&db, watchtower, n).await {
                Err(Error::Rule(why)) => assert_eq!(
                    why,
                    format!(
                        "the add-on watchtower did not open the ticket #{n}: an add-on resolves \
                         only the tickets it opened"
                    )
                ),
                other => panic!("#{n}: {other:?}"),
            }
            assert_eq!(testing::state(&db, n).await, "open");
        }
        assert!(testing::state_changes(&db).await.is_empty());
    }

    /// An agent has worked on a ticket once it has an assignee, or an agent wrote a reply, a note
    /// or an announcement on it; notes of add-ons, its opener's included, and messages received
    /// do not count.
    #[sqlx::test]
    async fn a_ticket_says_whether_an_agent_has_worked_on_it(db: PgPool) {
        let northgate = testing::help_desk(&db).await;
        let brightfield = testing::customer(&db, "Brightfield").await;
        let lena = testing::requester(&db, brightfield, "lena@brightfield.test").await;
        crate::key_requesters::mark(&db, lena, None).await.unwrap();
        let marco = testing::agent(&db, "marco@northgate.test").await;
        let watchtower = addon(&db, "watchtower").await;
        let pinger = addon(&db, "pinger").await;
        let worked_on = |n: i64| {
            let db = db.clone();
            async move { detail(&db, n).await.unwrap().worked_on_by_agent }
        };
        let opened = |title: &'static str| {
            let db = db.clone();
            async move {
                open(
                    &db,
                    watchtower,
                    &NewTicket {
                        description: Some("Seen at 03:10.".into()),
                        ..ticket_for(brightfield, title)
                    },
                )
                .await
                .unwrap()
            }
        };

        let untouched = opened("Disk full").await;
        add_note(&db, watchtower, untouched, "Still full.")
            .await
            .unwrap();
        add_note(&db, pinger, untouched, "Host answers.")
            .await
            .unwrap();
        // A key requester's answer, as it joins the ticket by its headers.
        sqlx::query(
            "INSERT INTO messages (id, ticket_id, direction, from_address, subject, body_text)
             SELECT $1, id, 'inbound', 'lena@brightfield.test', 'Re: Disk full', 'Is it fixed?'
             FROM tickets WHERE number = $2",
        )
        .bind(Uuid::new_v4())
        .bind(untouched)
        .execute(&db)
        .await
        .unwrap();
        assert_eq!(
            detail(&db, untouched).await.unwrap().timeline.len(),
            4,
            "two notes of add-ons and Lena's message"
        );
        assert!(!worked_on(untouched).await);

        let assigned = opened("Backup failed").await;
        crate::tickets::assign(&db, assigned, Some(marco))
            .await
            .unwrap();
        assert!(worked_on(assigned).await, "an assignee");
        crate::tickets::assign(&db, assigned, None).await.unwrap();
        assert!(!worked_on(assigned).await, "no assignee any more");

        let noted = opened("UPS on battery").await;
        crate::tickets::add_note(&db, noted, marco, "Called the site.")
            .await
            .unwrap();
        assert!(worked_on(noted).await, "an agent's note");

        let replied = opened("Certificate expiring").await;
        crate::tickets::reply(
            &db,
            &testing::sender(),
            replied,
            marco,
            "We are renewing it.",
            None,
            TicketState::Open,
        )
        .await
        .unwrap();
        assert!(worked_on(replied).await, "an agent's reply, even unsent");

        let outage = open(&db, watchtower, &ticket_for(northgate, "Mail relay down"))
            .await
            .unwrap();
        crate::parents::attach(&db, untouched, outage)
            .await
            .unwrap();
        assert!(!worked_on(outage).await);
        crate::announcements::send(
            &db,
            &testing::sender(),
            outage,
            marco,
            "The mail relay is down.",
        )
        .await
        .unwrap();
        assert!(worked_on(outage).await, "an agent's announcement");
        assert!(
            !worked_on(untouched).await,
            "an announcement from its parent is no work on the child"
        );
    }

    /// The number of the one ticket there is.
    async fn only_ticket_number(db: &PgPool) -> i64 {
        sqlx::query_scalar("SELECT number FROM tickets")
            .fetch_one(db)
            .await
            .unwrap()
    }
}
