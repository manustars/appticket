//! Proposals: what an add-on such as the Assistant suggests on an inbox message or a ticket. A
//! proposal takes effect only when an agent accepts it (ADR 0006, docs/domain-rules.md).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::{Error, Result, rule};
use crate::tickets::{Priority, TicketState};

/// The languages of the agents' UI; a rationale should be written in each of them. Kept equal to
/// the catalogs in web/src/i18n/locales by a test.
pub const UI_LANGUAGES: &[&str] = &["en", "it"];

/// Why a proposal is made, one sentence per language: `{"en": "...", "it": "..."}`.
pub type Rationale = BTreeMap<String, String>;

/// Checks a rationale: language tags of 2 to 8 lowercase letters, each with a non-empty sentence of
/// at most 500 characters. Returns it trimmed.
pub fn clean_rationale(rationale: &Rationale) -> Result<Rationale> {
    let mut clean = Rationale::new();
    for (language, text) in rationale {
        let valid_tag =
            (2..=8).contains(&language.len()) && language.chars().all(|c| c.is_ascii_lowercase());
        if !valid_tag {
            return rule(format!(
                "{language:?} is not a language tag such as \"en\" or \"it\""
            ));
        }
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        if text.chars().count() > 500 {
            return rule(format!(
                "the {language} rationale is longer than 500 characters"
            ));
        }
        clean.insert(language.clone(), text.to_owned());
    }
    Ok(clean)
}

/// What is proposed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Content {
    /// What to do with an inbox message.
    Triage(Triage),
    /// The ticket's priority.
    Priority { priority: Priority },
    /// Assets of the ticket's customer the ticket is about.
    Assets { asset_ids: Vec<Uuid> },
    /// Services the ticket's customer uses that the ticket is about.
    Services { service_ids: Vec<Uuid> },
    /// A draft reply to the requester; accepting it puts it in the agent's composer.
    Reply { body: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Triage {
    OpenTicket { customer_id: Uuid },
    Attach { ticket_number: i64 },
    Discard,
}

impl Content {
    fn kind(&self) -> &'static str {
        match self {
            Content::Triage(_) => "triage",
            Content::Priority { .. } => "priority",
            Content::Assets { .. } => "assets",
            Content::Services { .. } => "services",
            Content::Reply { .. } => "reply",
        }
    }
}

/// Where a proposal applies.
#[derive(Debug, Clone, Copy)]
pub enum Target {
    Message(Uuid),
    Ticket(i64),
}

/// Records an add-on's proposal, replacing the open one of the same kind on the same target.
/// The proposal must make sense now: the message waits in the inbox, the ticket is not closed,
/// the customer, ticket and assets exist and fit.
pub async fn submit(
    db: &PgPool,
    addon: &str,
    target: Target,
    content: &Content,
    rationale: &Rationale,
) -> Result<Uuid> {
    let rationale = clean_rationale(rationale)?;
    let mut tx = db.begin().await?;
    let addon_id: Uuid = sqlx::query_scalar("SELECT id FROM addons WHERE name = $1")
        .bind(addon)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(Error::NotFound("add-on"))?;
    let (message_id, ticket_id) = match (target, content) {
        (Target::Message(message), Content::Triage(triage)) => {
            let waiting: Option<bool> = sqlx::query_scalar(
                "SELECT ticket_id IS NULL AND discarded_at IS NULL FROM messages
                 WHERE id = $1 AND direction = 'inbound'",
            )
            .bind(message)
            .fetch_optional(&mut *tx)
            .await?;
            match waiting {
                None => return Err(Error::NotFound("message")),
                Some(false) => return rule("the message is no longer in the inbox"),
                Some(true) => {}
            }
            match triage {
                Triage::OpenTicket { customer_id } => {
                    let active: Option<bool> = sqlx::query_scalar(
                        "SELECT offboarded_at IS NULL FROM customers WHERE id = $1",
                    )
                    .bind(customer_id)
                    .fetch_optional(&mut *tx)
                    .await?;
                    match active {
                        None => return Err(Error::NotFound("customer")),
                        Some(false) => return rule("the customer is offboarded"),
                        Some(true) => {}
                    }
                }
                Triage::Attach { ticket_number } => {
                    let state: Option<TicketState> =
                        sqlx::query_scalar("SELECT state FROM tickets WHERE number = $1")
                            .bind(ticket_number)
                            .fetch_optional(&mut *tx)
                            .await?;
                    match state {
                        None => return Err(Error::NotFound("ticket")),
                        Some(TicketState::Closed) => return rule("the ticket is closed"),
                        Some(_) => {}
                    }
                }
                Triage::Discard => {}
            }
            (Some(message), None)
        }
        (Target::Ticket(_), Content::Triage(_)) => {
            return rule("triage is proposed on an inbox message, not on a ticket");
        }
        (Target::Message(_), _) => {
            return rule("only triage is proposed on an inbox message");
        }
        (Target::Ticket(number), content) => {
            let row: Option<(Uuid, Uuid, TicketState, bool)> = sqlx::query_as(
                "SELECT id, customer_id, state, requester_id IS NOT NULL FROM tickets
                 WHERE number = $1",
            )
            .bind(number)
            .fetch_optional(&mut *tx)
            .await?;
            let (ticket, customer, state, has_requester) = row.ok_or(Error::NotFound("ticket"))?;
            if state == TicketState::Closed {
                return rule("the ticket is closed");
            }
            match content {
                Content::Assets { asset_ids } => {
                    if asset_ids.is_empty() {
                        return rule("an assets proposal names at least one asset");
                    }
                    let fitting: i64 = sqlx::query_scalar(
                        "SELECT count(*) FROM assets
                         WHERE id = ANY($1) AND customer_id = $2 AND retired_at IS NULL",
                    )
                    .bind(asset_ids)
                    .bind(customer)
                    .fetch_one(&mut *tx)
                    .await?;
                    if fitting as usize != asset_ids.len() {
                        return rule(
                            "every proposed asset must be an active asset of the ticket's customer",
                        );
                    }
                }
                Content::Services { service_ids } => {
                    if service_ids.is_empty() {
                        return rule("a services proposal names at least one service");
                    }
                    let fitting: i64 = sqlx::query_scalar(
                        "SELECT count(*) FROM services s
                         JOIN customer_services cs ON cs.service_id = s.id AND cs.customer_id = $2
                         WHERE s.id = ANY($1) AND s.retired_at IS NULL",
                    )
                    .bind(service_ids)
                    .bind(customer)
                    .fetch_one(&mut *tx)
                    .await?;
                    if fitting as usize != service_ids.len() {
                        return rule(
                            "every proposed service must be an active service the ticket's customer uses",
                        );
                    }
                }
                Content::Reply { body } => {
                    if body.trim().is_empty() {
                        return rule("a draft reply cannot be empty");
                    }
                    if !has_requester {
                        return rule("the ticket has no requester to reply to");
                    }
                }
                Content::Priority { .. } | Content::Triage(_) => {}
            }
            (None, Some(ticket))
        }
    };

    sqlx::query(
        "UPDATE proposals SET state = 'superseded', decided_at = now()
         WHERE state = 'open' AND kind = $1
           AND (message_id = $2 OR ticket_id = $3)",
    )
    .bind(content.kind())
    .bind(message_id)
    .bind(ticket_id)
    .execute(&mut *tx)
    .await?;
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO proposals (id, addon_id, message_id, ticket_id, kind, content, rationale)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(id)
    .bind(addon_id)
    .bind(message_id)
    .bind(ticket_id)
    .bind(content.kind())
    .bind(sqlx::types::Json(content))
    .bind(sqlx::types::Json(&rationale))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(id)
}

/// A proposal as agents see it, with the names it refers to.
#[derive(Debug, Serialize, ToSchema)]
pub struct ProposalView {
    pub id: Uuid,
    /// The add-on that made it.
    pub addon: String,
    pub content: Content,
    /// Why it is proposed, by language; show the agent's language, else English.
    pub rationale: Rationale,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// For triage that opens a ticket: the customer's name.
    pub customer_name: Option<String>,
    /// For an assets proposal: the assets' names.
    pub assets: Vec<ProposedAsset>,
    /// For a services proposal: the services' names.
    pub services: Vec<ProposedService>,
}

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct ProposedService {
    pub id: Uuid,
    pub name: String,
}

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct ProposedAsset {
    pub id: Uuid,
    pub kind: String,
    pub name: String,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    addon: String,
    content: sqlx::types::Json<Content>,
    rationale: sqlx::types::Json<Rationale>,
    created_at: OffsetDateTime,
}

async fn views(db: &PgPool, rows: Vec<Row>) -> Result<Vec<ProposalView>> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let content = row.content.0;
        let customer_name = match &content {
            Content::Triage(Triage::OpenTicket { customer_id }) => {
                sqlx::query_scalar("SELECT name FROM customers WHERE id = $1")
                    .bind(customer_id)
                    .fetch_optional(db)
                    .await?
            }
            _ => None,
        };
        let assets = match &content {
            Content::Assets { asset_ids } => {
                sqlx::query_as("SELECT id, kind, name FROM assets WHERE id = ANY($1) ORDER BY name")
                    .bind(asset_ids)
                    .fetch_all(db)
                    .await?
            }
            _ => Vec::new(),
        };
        let services = match &content {
            Content::Services { service_ids } => {
                sqlx::query_as("SELECT id, name FROM services WHERE id = ANY($1) ORDER BY name")
                    .bind(service_ids)
                    .fetch_all(db)
                    .await?
            }
            _ => Vec::new(),
        };
        out.push(ProposalView {
            id: row.id,
            addon: row.addon,
            content,
            rationale: row.rationale.0,
            created_at: row.created_at,
            customer_name,
            assets,
            services,
        });
    }
    Ok(out)
}

/// The open proposals on a ticket.
pub async fn open_on_ticket(db: &PgPool, ticket: Uuid) -> Result<Vec<ProposalView>> {
    let rows = sqlx::query_as(
        "SELECT p.id, a.name AS addon, p.content, p.rationale, p.created_at
         FROM proposals p JOIN addons a ON a.id = p.addon_id
         WHERE p.ticket_id = $1 AND p.state = 'open' ORDER BY p.kind",
    )
    .bind(ticket)
    .fetch_all(db)
    .await?;
    views(db, rows).await
}

/// The open triage proposal on each of `messages`, by message.
pub async fn open_on_messages(db: &PgPool, messages: &[Uuid]) -> Result<Vec<(Uuid, ProposalView)>> {
    #[derive(sqlx::FromRow)]
    struct WithMessage {
        message_id: Uuid,
        #[sqlx(flatten)]
        row: Row,
    }
    let rows: Vec<WithMessage> = sqlx::query_as(
        "SELECT p.message_id, p.id, a.name AS addon, p.content, p.rationale, p.created_at
         FROM proposals p JOIN addons a ON a.id = p.addon_id
         WHERE p.message_id = ANY($1) AND p.state = 'open'",
    )
    .bind(messages)
    .fetch_all(db)
    .await?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.message_id).collect();
    let views = views(db, rows.into_iter().map(|r| r.row).collect()).await?;
    Ok(ids.into_iter().zip(views).collect())
}

/// What accepting a proposal did.
#[derive(Debug, Default, Serialize, ToSchema)]
pub struct Accepted {
    /// The ticket the proposal opened or acted on, when there is one.
    pub ticket_number: Option<i64>,
    /// For a draft reply: the text to put in the composer. Nothing has been sent.
    pub reply_body: Option<String>,
}

/// A stored proposal: its content, message, ticket number and state.
type StoredProposal = (
    sqlx::types::Json<Content>,
    Option<Uuid>,
    Option<i64>,
    String,
);

/// Applies an open proposal through the same operations an agent would use, and records who
/// accepted it. When the operation is refused (the situation changed), the proposal stays open.
pub async fn accept(db: &PgPool, id: Uuid, agent: Uuid) -> Result<Accepted> {
    let row: Option<StoredProposal> = sqlx::query_as(
        "SELECT p.content, p.message_id, t.number, p.state
             FROM proposals p LEFT JOIN tickets t ON t.id = p.ticket_id
             WHERE p.id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await?;
    let (content, message, number, state) = row.ok_or(Error::NotFound("proposal"))?;
    if state != "open" {
        return rule("the proposal is no longer open");
    }
    let mut accepted = Accepted::default();
    match (content.0, message, number) {
        (Content::Triage(triage), Some(message), _) => match triage {
            Triage::OpenTicket { customer_id } => {
                accepted.ticket_number =
                    Some(crate::triage::open_ticket(db, message, customer_id).await?);
            }
            Triage::Attach { ticket_number } => {
                crate::triage::attach(db, message, ticket_number).await?;
                accepted.ticket_number = Some(ticket_number);
            }
            Triage::Discard => crate::triage::discard(db, message).await?,
        },
        (Content::Priority { priority }, _, Some(number)) => {
            crate::tickets::set_priority(db, number, priority).await?;
            accepted.ticket_number = Some(number);
        }
        (Content::Assets { asset_ids }, _, Some(number)) => {
            for asset in asset_ids {
                crate::catalog::link_asset(db, number, asset, true).await?;
            }
            accepted.ticket_number = Some(number);
        }
        (Content::Services { service_ids }, _, Some(number)) => {
            for service in service_ids {
                crate::catalog::link_service(db, number, service, true).await?;
            }
            accepted.ticket_number = Some(number);
        }
        (Content::Reply { body }, _, Some(number)) => {
            // The draft is kept for the agent until sent, even if they leave the ticket.
            crate::drafts::save(db, number, agent, crate::drafts::DraftMode::Reply, &body).await?;
            accepted.ticket_number = Some(number);
            accepted.reply_body = Some(body);
        }
        _ => {
            return Err(Error::Internal(anyhow::anyhow!(
                "proposal {id} has no target"
            )));
        }
    }
    decide(db, id, agent, "accepted").await?;
    Ok(accepted)
}

/// The permission deciding on the proposal needs (docs/domain-rules.md, "Agents, roles and
/// permissions"): sorting the inbox for triage, replying for a draft reply when accepting it,
/// working tickets otherwise.
pub async fn needs(db: &PgPool, id: Uuid, accepting: bool) -> Result<crate::roles::Permission> {
    use crate::roles::Permission;
    let kind: Option<String> = sqlx::query_scalar("SELECT kind FROM proposals WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await?;
    Ok(match kind.ok_or(Error::NotFound("proposal"))?.as_str() {
        "triage" => Permission::SortInbox,
        "reply" if accepting => Permission::Reply,
        _ => Permission::WorkTickets,
    })
}

pub async fn discard(db: &PgPool, id: Uuid, agent: Uuid) -> Result<()> {
    let state: Option<String> = sqlx::query_scalar("SELECT state FROM proposals WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await?;
    match state.as_deref() {
        None => Err(Error::NotFound("proposal")),
        Some("open") => decide(db, id, agent, "discarded").await,
        Some(_) => rule("the proposal is no longer open"),
    }
}

async fn decide(db: &PgPool, id: Uuid, agent: Uuid, state: &str) -> Result<()> {
    sqlx::query(
        "UPDATE proposals SET state = $3, decided_at = now(), decided_by = $2
         WHERE id = $1 AND state = 'open'",
    )
    .bind(id)
    .bind(agent)
    .bind(state)
    .execute(db)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;

    fn en(text: &str) -> Rationale {
        Rationale::from([("en".to_owned(), text.to_owned())])
    }

    #[test]
    fn rationales_are_checked_per_language() {
        let mut r = en("  Domain of Brightfield ");
        r.insert("it".into(), "Dominio di Brightfield".into());
        r.insert("de".into(), "   ".into());
        let clean = clean_rationale(&r).unwrap();
        assert_eq!(clean.len(), 2, "empty sentences are dropped");
        assert_eq!(clean["en"], "Domain of Brightfield");
        assert!(clean_rationale(&Rationale::from([("EN".into(), "x".into())])).is_err());
        assert!(clean_rationale(&Rationale::from([("en".into(), "x".repeat(501))])).is_err());
    }

    #[test]
    fn ui_languages_match_the_catalogs() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/i18n/locales");
        let mut catalogs: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| {
                e.ok()?
                    .file_name()
                    .to_str()?
                    .strip_suffix(".json")
                    .map(str::to_owned)
            })
            .collect();
        catalogs.sort();
        let mut ours: Vec<&str> = UI_LANGUAGES.to_vec();
        ours.sort();
        assert_eq!(catalogs, ours);
    }

    async fn assistant(db: &PgPool) {
        crate::addons::register(db, "assistant", "http://127.0.0.1:9", &[], &[])
            .await
            .unwrap();
    }

    #[sqlx::test]
    async fn triage_is_accepted_through_the_normal_operation(db: PgPool) {
        assistant(&db).await;
        let brightfield = testing::customer(&db, "Brightfield").await;
        let agent = testing::agent(&db, "marco@northgate.test").await;
        let m = testing::inbox_message(&db, "ada@brightfield.test").await;

        let discard = Content::Triage(Triage::Discard);
        let first = submit(
            &db,
            "assistant",
            Target::Message(m),
            &discard,
            &en("Looks like spam"),
        )
        .await
        .unwrap();
        let open = Content::Triage(Triage::OpenTicket {
            customer_id: brightfield,
        });
        let second = submit(
            &db,
            "assistant",
            Target::Message(m),
            &open,
            &en("Domain of Brightfield"),
        )
        .await
        .unwrap();
        assert!(
            matches!(accept(&db, first, agent).await, Err(Error::Rule(_))),
            "superseded"
        );

        let views = open_on_messages(&db, &[m]).await.unwrap();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].1.customer_name.as_deref(), Some("Brightfield"));

        let done = accept(&db, second, agent).await.unwrap();
        let number = done.ticket_number.unwrap();
        assert_eq!(
            crate::tickets::detail(&db, number)
                .await
                .unwrap()
                .customer
                .id,
            brightfield
        );
        assert!(open_on_messages(&db, &[m]).await.unwrap().is_empty());
        // The message left the inbox: a new triage proposal no longer applies.
        assert!(matches!(
            submit(&db, "assistant", Target::Message(m), &discard, &en("")).await,
            Err(Error::Rule(_))
        ));
    }

    #[sqlx::test]
    async fn services_are_proposed_and_an_accepted_draft_is_kept(db: PgPool) {
        assistant(&db).await;
        let brightfield = testing::customer(&db, "Brightfield").await;
        let agent = testing::agent(&db, "marco@northgate.test").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let n = testing::ticket(&db, brightfield, Some(ada), "a@brightfield.test").await;
        let mail = crate::catalog::create_service(&db, "Hosted mail", "")
            .await
            .unwrap();
        let backup = crate::catalog::create_service(&db, "Backup", "")
            .await
            .unwrap();
        crate::catalog::use_service(&db, mail, brightfield)
            .await
            .unwrap();
        let t = Target::Ticket(n);

        assert!(
            matches!(
                submit(
                    &db,
                    "assistant",
                    t,
                    &Content::Services {
                        service_ids: vec![backup]
                    },
                    &en("")
                )
                .await,
                Err(Error::Rule(_))
            ),
            "the customer does not use backup"
        );
        let services = submit(
            &db,
            "assistant",
            t,
            &Content::Services {
                service_ids: vec![mail],
            },
            &en("Mail is down"),
        )
        .await
        .unwrap();
        let ticket_id: Uuid = sqlx::query_scalar("SELECT id FROM tickets WHERE number = $1")
            .bind(n)
            .fetch_one(&db)
            .await
            .unwrap();
        let views = open_on_ticket(&db, ticket_id).await.unwrap();
        assert_eq!(views[0].services[0].name, "Hosted mail");
        accept(&db, services, agent).await.unwrap();
        assert_eq!(
            crate::tickets::detail(&db, n).await.unwrap().services.len(),
            1
        );

        let reply = submit(
            &db,
            "assistant",
            t,
            &Content::Reply {
                body: "Buongiorno Ada,".into(),
            },
            &en(""),
        )
        .await
        .unwrap();
        accept(&db, reply, agent).await.unwrap();
        let draft = crate::drafts::get(&db, n, agent).await.unwrap().unwrap();
        assert_eq!(draft.body, "Buongiorno Ada,");
        assert_eq!(draft.mode, crate::drafts::DraftMode::Reply);
    }

    #[sqlx::test]
    async fn ticket_proposals_are_checked_and_applied(db: PgPool) {
        assistant(&db).await;
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let agent = testing::agent(&db, "marco@northgate.test").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let n = testing::ticket(&db, brightfield, Some(ada), "a@brightfield.test").await;
        let pc = crate::catalog::create_asset(&db, brightfield, "computer", "PC-0042")
            .await
            .unwrap();
        let other = crate::catalog::create_asset(&db, harbor, "printer", "P2")
            .await
            .unwrap();
        let t = Target::Ticket(n);

        assert!(matches!(
            submit(
                &db,
                "assistant",
                t,
                &Content::Assets {
                    asset_ids: vec![pc, other]
                },
                &en("")
            )
            .await,
            Err(Error::Rule(_))
        ));
        let assets = submit(
            &db,
            "assistant",
            t,
            &Content::Assets {
                asset_ids: vec![pc],
            },
            &en(""),
        )
        .await
        .unwrap();
        let priority = submit(
            &db,
            "assistant",
            t,
            &Content::Priority {
                priority: Priority::High,
            },
            &en("Nobody can work"),
        )
        .await
        .unwrap();
        let reply = submit(
            &db,
            "assistant",
            t,
            &Content::Reply {
                body: "Please restart.".into(),
            },
            &en(""),
        )
        .await
        .unwrap();
        let ticket_id: Uuid = sqlx::query_scalar("SELECT id FROM tickets WHERE number = $1")
            .bind(n)
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(open_on_ticket(&db, ticket_id).await.unwrap().len(), 3);

        accept(&db, assets, agent).await.unwrap();
        accept(&db, priority, agent).await.unwrap();
        let drafted = accept(&db, reply, agent).await.unwrap();
        assert_eq!(drafted.reply_body.as_deref(), Some("Please restart."));
        let ticket = crate::tickets::detail(&db, n).await.unwrap();
        assert_eq!(ticket.priority, Priority::High);
        assert_eq!(ticket.assets.len(), 1);
        let outbound: i64 =
            sqlx::query_scalar("SELECT count(*) FROM messages WHERE direction = 'outbound'")
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(outbound, 0, "an accepted draft is not sent");
        assert!(open_on_ticket(&db, ticket_id).await.unwrap().is_empty());

        let again = submit(
            &db,
            "assistant",
            t,
            &Content::Priority {
                priority: Priority::Low,
            },
            &en(""),
        )
        .await
        .unwrap();
        discard(&db, again, agent).await.unwrap();
        assert!(matches!(
            discard(&db, again, agent).await,
            Err(Error::Rule(_))
        ));
        assert!(matches!(
            submit(
                &db,
                "assistant",
                t,
                &Content::Triage(Triage::Discard),
                &en("")
            )
            .await,
            Err(Error::Rule(_))
        ));
    }
}
