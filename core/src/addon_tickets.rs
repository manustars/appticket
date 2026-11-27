//! What an add-on with the scope `tickets:write` does on tickets (docs/domain-rules.md,
//! "Add-ons"): it opens tickets without a requester for a customer, with a priority, assets and
//! services, and a reference of its own that names the ticket for good; it adds internal notes to
//! any ticket and resolves the tickets it opened. It never replies and changes nothing else.

use serde::Deserialize;
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::{Error, Result, rule};
use crate::tickets::{NoteAuthor, Priority, StateChanger, TicketState};

/// The longest reference an add-on gives a ticket, in characters, once trimmed.
pub const MAX_REFERENCE_LENGTH: usize = 255;

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
    /// The add-on's own reference for the ticket, such as the id of the problem it is about,
    /// compared exactly once trimmed: it names this ticket for good, and opening again with it
    /// answers this ticket, unchanged. Not blank, without control characters, at most 255
    /// characters. Without one, each open opens a new ticket.
    #[serde(default)]
    #[schema(max_length = 255)]
    pub reference: Option<String>,
}

/// The ticket an add-on opened, or the one it had opened before with the same reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Opened {
    pub number: i64,
    /// Opened now; false when the reference named a ticket the add-on had opened before.
    pub created: bool,
}

/// `reference` trimmed, refused with a sentence when it does not fit.
fn checked_reference(reference: &str) -> Result<&str> {
    crate::name_lists::trimmed(reference, MAX_REFERENCE_LENGTH, || {
        "a ticket's reference".to_owned()
    })
}

/// The ticket `addon` opened with `reference`, compared exactly.
async fn find<'e>(
    db: impl sqlx::PgExecutor<'e>,
    addon: Uuid,
    reference: &str,
) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar(
        "SELECT number FROM tickets WHERE opened_by_addon_id = $1 AND addon_reference = $2",
    )
    .bind(addon)
    .bind(reference)
    .fetch_optional(db)
    .await?)
}

/// Opens a ticket without a requester for `addon`. The ticket, its links and its description are
/// written together or not at all: an asset or service that does not fit refuses the whole
/// ticket, with a sentence naming it. With a reference the add-on gave before, it opens nothing
/// and answers the ticket opened then, in any state and unchanged, whatever else `new` says; so
/// does the second of two opens with the same reference at the same time.
pub async fn open(db: &PgPool, addon: Uuid, new: &NewTicket) -> Result<Opened> {
    let reference = new
        .reference
        .as_deref()
        .map(checked_reference)
        .transpose()?;
    if let Some(reference) = reference
        && let Some(number) = find(db, addon, reference).await?
    {
        return Ok(Opened {
            number,
            created: false,
        });
    }
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
    // An open of the add-on with the same reference at the same time makes this one wait: once
    // that one is written, this one writes nothing and answers its ticket.
    let number: Option<i64> = sqlx::query_scalar(
        "INSERT INTO tickets (id, customer_id, title, priority, opened_by_addon_id,
                              addon_reference)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (opened_by_addon_id, addon_reference) DO NOTHING
         RETURNING number",
    )
    .bind(id)
    .bind(new.customer_id)
    .bind(title)
    .bind(new.priority.unwrap_or(Priority::Normal))
    .bind(addon)
    .bind(reference)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(number) = number else {
        tx.rollback().await?;
        let reference = reference.expect("without a reference nothing conflicts");
        let number = find(db, addon, reference).await?.ok_or_else(|| {
            anyhow::anyhow!("the ticket with the reference {reference} is not there")
        })?;
        return Ok(Opened {
            number,
            created: false,
        });
    };
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
    Ok(Opened {
        number,
        created: true,
    })
}

/// The number of the ticket `addon` opened with `reference`, compared exactly once trimmed, in
/// any state; not found for a reference it never gave, another add-on's included.
pub async fn by_reference(db: &PgPool, addon: Uuid, reference: &str) -> Result<i64> {
    find(db, addon, reference.trim())
        .await?
        .ok_or(Error::NotFound("ticket"))
}

/// The reference `addon` gave the ticket, when it opened it with one; none for a ticket another
/// add-on, an agent or a message opened.
pub async fn reference_of(db: &PgPool, addon: Uuid, number: i64) -> Result<Option<String>> {
    Ok(sqlx::query_scalar::<_, Option<String>>(
        "SELECT addon_reference FROM tickets WHERE number = $1 AND opened_by_addon_id = $2",
    )
    .bind(number)
    .bind(addon)
    .fetch_optional(db)
    .await?
    .flatten())
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
    let (ticket, state) = crate::tickets::lock(&mut tx, number).await?;
    let (opener, name): (Option<Uuid>, String) = sqlx::query_as(
        "SELECT t.opened_by_addon_id, a.name FROM tickets t, addons a
         WHERE t.id = $1 AND a.id = $2",
    )
    .bind(ticket)
    .bind(addon)
    .fetch_one(&mut *tx)
    .await?;
    if opener != Some(addon) {
        return rule(format!(
            "the add-on {name} did not open the ticket #{number}: an add-on resolves only the \
             tickets it opened"
        ));
    }
    crate::tickets::not_closed(state)?;
    if state == TicketState::Resolved {
        return Ok(());
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
            reference: None,
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
        .unwrap()
        .number;

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
            .unwrap()
            .number;
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
            .unwrap()
            .number;

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
        .unwrap()
        .number;
        let backup = open(&db, watchtower, &ticket_for(brightfield, "Backup failed"))
            .await
            .unwrap()
            .number;
        testing::set_state(&db, backup, TicketState::Waiting)
            .await
            .unwrap();
        let done = open(&db, watchtower, &ticket_for(brightfield, "UPS on battery"))
            .await
            .unwrap()
            .number;
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
            .unwrap()
            .number;

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

    /// An agent has worked on a ticket once they have been its assignee, written a reply, a note
    /// or an announcement on it, or changed it by hand: its state, priority, links, customer or
    /// parent. It stays so: unassigning the agent changes nothing. Notes of add-ons, its
    /// opener's included, and messages received do not count.
    #[sqlx::test]
    async fn a_ticket_says_whether_an_agent_has_worked_on_it(db: PgPool) {
        let northgate = testing::help_desk(&db).await;
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let lena = testing::requester(&db, brightfield, "lena@brightfield.test").await;
        crate::key_requesters::mark(&db, lena, None).await.unwrap();
        let server = testing::asset(&db, brightfield, "Server", "SRV-01").await;
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
                .number
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
        resolve(&db, watchtower, untouched).await.unwrap();
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
        assert!(worked_on(assigned).await, "still, once unassigned");

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

        let reopened = opened("Link down").await;
        resolve(&db, watchtower, reopened).await.unwrap();
        crate::tickets::set_state(&db, reopened, TicketState::Open, marco)
            .await
            .unwrap();
        assert!(worked_on(reopened).await, "reopened by hand, with no note");

        let urgent = opened("RAID degraded").await;
        crate::tickets::set_priority(&db, urgent, Priority::Urgent)
            .await
            .unwrap();
        assert!(worked_on(urgent).await, "a priority set by hand");

        let linked = opened("Fan failure").await;
        crate::catalog::link_asset(&db, linked, server, true)
            .await
            .unwrap();
        assert!(worked_on(linked).await, "an asset linked by hand");

        let mail = crate::catalog::create_service(&db, "Hosted mail", "")
            .await
            .unwrap();
        crate::catalog::use_service(&db, mail, brightfield)
            .await
            .unwrap();
        let served = opened("Mailbox full").await;
        crate::catalog::link_service(&db, served, mail, true)
            .await
            .unwrap();
        assert!(worked_on(served).await, "a service linked by hand");

        // An inbox message an agent attaches, to an open ticket: no change of state, but work.
        let attached = opened("Disk quota").await;
        let message = testing::inbox_message(&db, "someone@elsewhere.test").await;
        crate::triage::attach(&db, message, attached, marco)
            .await
            .unwrap();
        assert_eq!(testing::state(&db, attached).await, "open");
        assert!(
            worked_on(attached).await,
            "an inbox message attached by hand"
        );

        let moved = opened("Printer offline").await;
        crate::tickets::move_to_customer(&db, moved, harbor, crate::tickets::RequesterMove::Remove)
            .await
            .unwrap();
        assert!(worked_on(moved).await, "moved to another customer");

        let outage = open(&db, watchtower, &ticket_for(northgate, "Mail relay down"))
            .await
            .unwrap()
            .number;
        let child = opened("Mail bounces").await;
        crate::parents::attach(&db, child, outage).await.unwrap();
        assert!(worked_on(child).await, "attached to a parent by hand");
        assert!(worked_on(outage).await, "a child attached to it by hand");
        let split = open(&db, watchtower, &ticket_for(northgate, "Relay slow"))
            .await
            .unwrap()
            .number;
        let leaving = opened("Mail delayed").await;
        let attach = |child: i64, parent: i64| {
            let db = db.clone();
            async move {
                sqlx::query(
                    "UPDATE tickets SET parent_id = (SELECT id FROM tickets WHERE number = $1)
                     WHERE number = $2",
                )
                .bind(parent)
                .bind(child)
                .execute(&db)
                .await
                .unwrap();
            }
        };
        attach(leaving, split).await;
        assert!(!worked_on(split).await && !worked_on(leaving).await);
        crate::parents::detach(&db, leaving).await.unwrap();
        assert!(worked_on(leaving).await, "detached by hand");
        assert!(worked_on(split).await, "a child detached from it by hand");
        let announcing = open(&db, watchtower, &ticket_for(northgate, "Backup site down"))
            .await
            .unwrap()
            .number;
        let affected = opened("Backups late").await;
        attach(affected, announcing).await;
        assert!(!worked_on(announcing).await);
        crate::announcements::send(
            &db,
            &testing::sender(),
            announcing,
            marco,
            "The backup site is down.",
        )
        .await
        .unwrap();
        assert!(worked_on(announcing).await, "an agent's announcement");
        assert!(
            !worked_on(affected).await,
            "an announcement from its parent is no work on the child"
        );
        assert!(!worked_on(untouched).await, "nothing reached the first one");

        // A parent's children resolved by an agent: one is written to, one only resolved.
        let failure = open(
            &db,
            watchtower,
            &ticket_for(northgate, "Storage array down"),
        )
        .await
        .unwrap()
        .number;
        let written = opened("Shares unreachable").await;
        let quiet = open(&db, watchtower, &ticket_for(harbor, "Shares unreachable"))
            .await
            .unwrap()
            .number;
        for child in [written, quiet] {
            attach(child, failure).await;
        }
        crate::parents::resolve_children(
            &db,
            &testing::sender(),
            failure,
            marco,
            "The storage array is back.",
        )
        .await
        .unwrap();
        assert_eq!(testing::state(&db, quiet).await, "resolved");
        assert!(worked_on(written).await, "resolved with a reply");
        assert!(
            worked_on(quiet).await,
            "resolved by an agent, with nobody to write to"
        );
    }

    /// The database keeps "worked on" also when the code is bypassed: an assignee and an agent's
    /// note or message mark the ticket, a note of an add-on does not, and nothing clears the mark.
    /// (An announcement, which needs its recipients, is checked through the code above.)
    #[sqlx::test]
    async fn the_database_keeps_a_ticket_worked_on(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let marco = testing::agent(&db, "marco@northgate.test").await;
        let watchtower = addon(&db, "watchtower").await;
        let worked_on = |n: i64| {
            let db = db.clone();
            async move {
                sqlx::query_scalar::<_, bool>(
                    "SELECT worked_on_by_agent FROM tickets WHERE number = $1",
                )
                .bind(n)
                .fetch_one(&db)
                .await
                .unwrap()
            }
        };
        let run = |sql: &'static str, n: i64, who: Uuid| {
            let db = db.clone();
            async move {
                sqlx::query(sql)
                    .bind(n)
                    .bind(who)
                    .bind(Uuid::new_v4())
                    .execute(&db)
                    .await
                    .unwrap();
            }
        };

        let by_addon = testing::ticket(&db, brightfield, None, "a1@brightfield.test").await;
        run(
            "INSERT INTO notes (id, ticket_id, addon_id, body)
             SELECT $3, id, $2, 'Checked.' FROM tickets WHERE number = $1",
            by_addon,
            watchtower,
        )
        .await;
        assert!(!worked_on(by_addon).await, "a note of an add-on");

        let assigned = testing::ticket(&db, brightfield, None, "a2@brightfield.test").await;
        run(
            "UPDATE tickets SET assignee_id = $2 WHERE number = $1 AND $3::uuid IS NOT NULL",
            assigned,
            marco,
        )
        .await;
        run(
            "UPDATE tickets SET assignee_id = NULL, worked_on_by_agent = false
             WHERE number = $1 AND $2::uuid IS NOT NULL AND $3::uuid IS NOT NULL",
            assigned,
            marco,
        )
        .await;
        assert!(
            worked_on(assigned).await,
            "an assignee, and nothing clears it"
        );

        let noted = testing::ticket(&db, brightfield, None, "a3@brightfield.test").await;
        run(
            "INSERT INTO notes (id, ticket_id, author_id, body)
             SELECT $3, id, $2, 'Checked.' FROM tickets WHERE number = $1",
            noted,
            marco,
        )
        .await;
        assert!(worked_on(noted).await, "an agent's note");

        let lena = testing::requester(&db, brightfield, "lena@brightfield.test").await;
        let replied = testing::ticket(&db, brightfield, Some(lena), "a4@brightfield.test").await;
        run(
            "INSERT INTO messages (id, ticket_id, direction, author_id, from_address, to_address,
                                   subject, body_text)
             SELECT $3, id, 'outbound', $2, 'helpdesk@helpdesk.test', 'lena@brightfield.test',
                    'Re: Printer jams', 'Done.'
             FROM tickets WHERE number = $1",
            replied,
            marco,
        )
        .await;
        assert!(worked_on(replied).await, "an agent's message");
    }

    /// Tickets from before "worked on" was kept are marked by what they show of agents: an
    /// assignee, an agent's note or message, a change of state by an agent among the events, or
    /// a parent and its child, which an agent attached.
    #[sqlx::test(migrations = false)]
    async fn tickets_from_before_are_marked_by_what_agents_did(db: PgPool) {
        let migrator = sqlx::migrate!();
        let this = migrator
            .iter()
            .find(|m| &*m.description == "addon resolves tickets")
            .unwrap()
            .version;
        migrator.run_to(this - 1, &db).await.unwrap();
        // Written directly: the code of today writes columns this schema does not have yet.
        let brightfield = testing::customer(&db, "Brightfield").await;
        let marco = testing::agent(&db, "marco@northgate.test").await;
        let watchtower = addon(&db, "watchtower").await;
        let ticket = |id: &'static str| {
            let db = db.clone();
            async move { testing::ticket(&db, brightfield, None, id).await }
        };
        let run = |sql: &'static str, n: i64, who: Uuid| {
            let db = db.clone();
            async move {
                sqlx::query(sql)
                    .bind(n)
                    .bind(who)
                    .bind(Uuid::new_v4())
                    .execute(&db)
                    .await
                    .unwrap();
            }
        };
        let untouched = ticket("u1@brightfield.test").await;
        run(
            "INSERT INTO notes (id, ticket_id, addon_id, body)
             SELECT $3, id, $2, 'Checked.' FROM tickets WHERE number = $1",
            untouched,
            watchtower,
        )
        .await;
        let assigned = ticket("u2@brightfield.test").await;
        run(
            "UPDATE tickets SET assignee_id = $2 WHERE number = $1 AND $3::uuid IS NOT NULL",
            assigned,
            marco,
        )
        .await;
        let noted = ticket("u3@brightfield.test").await;
        run(
            "INSERT INTO notes (id, ticket_id, author_id, body)
             SELECT $3, id, $2, 'Checked.' FROM tickets WHERE number = $1",
            noted,
            marco,
        )
        .await;
        let moved = ticket("u4@brightfield.test").await;
        run(
            "INSERT INTO events (id, kind, data)
             VALUES ($3, 'ticket.state_changed', jsonb_build_object('ticket_number', $1::bigint,
                 'old_state', 'open', 'new_state', 'waiting', 'changed_by',
                 jsonb_build_object('kind', 'agent',
                     'agent', jsonb_build_object('id', $2::uuid, 'name', 'marco'))))",
            moved,
            marco,
        )
        .await;

        let northgate = testing::help_desk(&db).await;
        let parent = testing::ticket(&db, northgate, None, "p1@northgate.test").await;
        let child = ticket("u5@brightfield.test").await;
        sqlx::query(
            "UPDATE tickets SET parent_id = (SELECT id FROM tickets WHERE number = $1)
             WHERE number = $2",
        )
        .bind(parent)
        .bind(child)
        .execute(&db)
        .await
        .unwrap();

        migrator.run(&db).await.unwrap();
        let marked: Vec<(i64, bool)> =
            sqlx::query_as("SELECT number, worked_on_by_agent FROM tickets ORDER BY number")
                .fetch_all(&db)
                .await
                .unwrap();
        assert_eq!(
            marked,
            [
                (untouched, false),
                (assigned, true),
                (noted, true),
                (moved, true),
                (parent, true),
                (child, true),
            ]
        );
    }

    async fn event_count(db: &PgPool) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM events")
            .fetch_one(db)
            .await
            .unwrap()
    }

    /// A reference names the ticket the add-on opened with it, for good: opening again with it
    /// answers that ticket in any state, closed included, and changes nothing on it, whatever
    /// else the request says; it opens nothing else and records no event.
    #[sqlx::test]
    async fn opening_again_with_a_reference_answers_the_same_ticket(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let watchtower = addon(&db, "watchtower").await;

        let first = open(
            &db,
            watchtower,
            &NewTicket {
                reference: Some("  4711  ".into()),
                description: Some("Disk /var is 97% full.".into()),
                ..ticket_for(brightfield, "SRV-01: disk full")
            },
        )
        .await
        .unwrap();
        assert!(first.created);
        testing::set_state(&db, first.number, TicketState::Closed)
            .await
            .unwrap();
        crate::customers::offboard(&db, brightfield).await.unwrap();
        let events = event_count(&db).await;

        let again = open(
            &db,
            watchtower,
            &NewTicket {
                reference: Some("4711".into()),
                description: Some("Disk /var is 99% full.".into()),
                priority: Some(Priority::Urgent),
                asset_ids: vec![Uuid::new_v4()],
                ..ticket_for(harbor, "   ")
            },
        )
        .await
        .unwrap();
        assert_eq!(
            again,
            Opened {
                number: first.number,
                created: false
            }
        );
        let ticket = detail(&db, first.number).await.unwrap();
        assert_eq!(ticket.title, "SRV-01: disk full");
        assert_eq!(ticket.customer.id, brightfield);
        assert_eq!(ticket.priority, Priority::Normal);
        assert_eq!(ticket.state, TicketState::Closed);
        assert_eq!(ticket.timeline.len(), 1, "only the first description");
        assert_eq!(ticket_count(&db).await, 1);
        assert_eq!(event_count(&db).await, events, "no event either");

        // Tickets without a reference are opened anew each time, as before.
        for _ in 0..2 {
            let plain = open(&db, watchtower, &ticket_for(harbor, "Backup failed"))
                .await
                .unwrap();
            assert!(plain.created);
        }
        assert_eq!(ticket_count(&db).await, 3);
    }

    /// References are the add-on's own: another add-on opens its own ticket with the same one,
    /// and each finds only its own by reference, compared exactly once trimmed.
    #[sqlx::test]
    async fn an_addon_finds_its_own_tickets_by_reference(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let watchtower = addon(&db, "watchtower").await;
        let pinger = addon(&db, "pinger").await;
        let with = |reference: &str| NewTicket {
            reference: Some(reference.into()),
            ..ticket_for(brightfield, "Host unreachable")
        };

        let ours = open(&db, watchtower, &with("4711")).await.unwrap();
        let theirs = open(&db, pinger, &with("4711")).await.unwrap();
        assert!(theirs.created);
        assert_ne!(ours.number, theirs.number);
        let upper = open(&db, watchtower, &with("host-A")).await.unwrap();
        let lower = open(&db, watchtower, &with("host-a")).await.unwrap();
        assert!(lower.created, "case counts");
        assert_ne!(upper.number, lower.number);
        let plain = open(&db, watchtower, &ticket_for(brightfield, "Backup failed"))
            .await
            .unwrap();

        assert_eq!(
            by_reference(&db, watchtower, " 4711 ").await.unwrap(),
            ours.number
        );
        assert_eq!(
            by_reference(&db, pinger, "4711").await.unwrap(),
            theirs.number
        );
        for (who, reference) in [(watchtower, "4712"), (pinger, "host-a"), (watchtower, "")] {
            assert!(
                matches!(
                    by_reference(&db, who, reference).await,
                    Err(Error::NotFound("ticket"))
                ),
                "{reference:?}"
            );
        }
        assert_eq!(
            reference_of(&db, watchtower, ours.number).await.unwrap(),
            Some("4711".into())
        );
        assert_eq!(
            reference_of(&db, pinger, ours.number).await.unwrap(),
            None,
            "another add-on's"
        );
        assert_eq!(
            reference_of(&db, watchtower, plain.number).await.unwrap(),
            None
        );
    }

    /// A reference that is blank, holds a control character or is longer than the limit is
    /// refused with a sentence, and nothing is opened; the longest one fits, in any characters.
    #[sqlx::test]
    async fn a_reference_that_does_not_fit_is_refused(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let watchtower = addon(&db, "watchtower").await;
        let with = |reference: String| NewTicket {
            reference: Some(reference),
            ..ticket_for(brightfield, "Disk full")
        };
        let too_long = "é".repeat(MAX_REFERENCE_LENGTH + 1);
        for (reference, expected) in [
            (
                "   ".to_owned(),
                "a ticket's reference cannot be blank".to_owned(),
            ),
            (
                "".to_owned(),
                "a ticket's reference cannot be blank".to_owned(),
            ),
            (
                "47\n11".to_owned(),
                "a ticket's reference holds a control character".to_owned(),
            ),
            (
                too_long,
                format!("a ticket's reference is longer than {MAX_REFERENCE_LENGTH} characters"),
            ),
        ] {
            match open(&db, watchtower, &with(reference.clone())).await {
                Err(Error::Rule(why)) => assert_eq!(why, expected),
                other => panic!("{reference:?}: {other:?}"),
            }
        }
        assert_eq!(ticket_count(&db).await, 0);
        let longest = "é".repeat(MAX_REFERENCE_LENGTH);
        let opened = open(&db, watchtower, &with(longest.clone())).await.unwrap();
        assert_eq!(
            by_reference(&db, watchtower, &longest).await.unwrap(),
            opened.number
        );
    }

    /// The database keeps the rules of references also when the code is bypassed: one ticket per
    /// reference of an add-on, a reference only on a ticket an add-on opened, trimmed, not blank,
    /// without control characters and within the limit, and it stays as it was given.
    #[sqlx::test]
    async fn the_database_keeps_one_ticket_per_reference_of_an_addon(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let watchtower = addon(&db, "watchtower").await;
        let pinger = addon(&db, "pinger").await;
        let insert = |addon: Option<Uuid>, reference: Option<String>| {
            let db = db.clone();
            async move {
                sqlx::query(
                    "INSERT INTO tickets (id, customer_id, title, opened_by_addon_id,
                                          addon_reference)
                     VALUES ($1, $2, 'Disk full', $3, $4) RETURNING number",
                )
                .bind(Uuid::new_v4())
                .bind(brightfield)
                .bind(addon)
                .bind(reference)
                .fetch_one(&db)
                .await
                .map(|row| sqlx::Row::get::<i64, _>(&row, 0))
                .map_err(|e| e.to_string())
            }
        };
        let given = |r: &str| Some(r.to_owned());
        let first = insert(Some(watchtower), given("4711")).await.unwrap();
        assert!(
            insert(Some(watchtower), given("4711")).await.is_err(),
            "twice"
        );
        assert!(
            insert(Some(pinger), given("4711")).await.is_ok(),
            "another add-on's"
        );
        assert!(insert(Some(watchtower), None).await.is_ok());
        assert!(insert(Some(watchtower), None).await.is_ok(), "none twice");
        assert!(
            insert(None, given("4712")).await.is_err(),
            "nobody's reference"
        );
        for wrong in [" 4712", "4712 ", "", "47\t12", "47\u{7f}12"] {
            assert!(
                insert(Some(watchtower), given(wrong)).await.is_err(),
                "{wrong:?}"
            );
        }
        let longest = "é".repeat(MAX_REFERENCE_LENGTH);
        assert!(insert(Some(watchtower), given(&longest)).await.is_ok());
        let too_long = "é".repeat(MAX_REFERENCE_LENGTH + 1);
        assert!(insert(Some(watchtower), given(&too_long)).await.is_err());

        let plain = insert(Some(watchtower), None).await.unwrap();
        let change = |sql: &'static str, number: i64| {
            let db = db.clone();
            async move {
                sqlx::query(sql)
                    .bind(number)
                    .bind(pinger)
                    .execute(&db)
                    .await
                    .is_ok()
            }
        };
        for sql in [
            "UPDATE tickets SET addon_reference = '4713' WHERE number = $1 AND $2::uuid IS NOT NULL",
            "UPDATE tickets SET addon_reference = NULL WHERE number = $1 AND $2::uuid IS NOT NULL",
            "UPDATE tickets SET opened_by_addon_id = $2 WHERE number = $1",
        ] {
            assert!(!change(sql, first).await, "{sql}");
        }
        assert!(
            !change(
                "UPDATE tickets SET addon_reference = '4713' WHERE number = $1 AND $2::uuid IS NOT NULL",
                plain
            )
            .await,
            "a reference added later"
        );
        assert!(
            change(
                "UPDATE tickets SET title = 'Disk still full' WHERE number = $1 AND $2::uuid IS NOT NULL",
                first
            )
            .await,
            "the rest of the ticket changes as before"
        );
        assert_eq!(by_reference(&db, watchtower, "4711").await.unwrap(), first);
    }

    /// The number of the one ticket there is.
    async fn only_ticket_number(db: &PgPool) -> i64 {
        sqlx::query_scalar("SELECT number FROM tickets")
            .fetch_one(db)
            .await
            .unwrap()
    }
}
