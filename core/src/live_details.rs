//! The live details of an asset (docs/domain-rules.md, "Add-ons"; ADR 0005): when agents look at
//! assets, the core asks the source of each and each add-on linked to it
//! `GET <url>/assets/{reference}`, with the reference that add-on knows the asset by, all at once
//! and within [`TIMEOUT`]. An add-on that
//! does not answer in time, answers anything but a 200 with live details, or is switched off,
//! has its details unavailable; nothing else waits for it.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::Result;

/// How long the core waits for an add-on's live details: the whole exchange, from connecting to
/// the last byte of the answer.
pub const TIMEOUT: Duration = Duration::from_secs(2);

/// A label in each language the add-on wrote it in: `{"en": "Disk", "it": "Disco"}`.
pub type Label = BTreeMap<String, String>;

/// What an add-on says of an asset now.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct LiveDetails {
    pub state: DetailState,
    pub fields: Vec<DetailField>,
    pub links: Vec<DetailLink>,
}

/// How the asset is doing, as the add-on sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DetailState {
    Ok,
    Warning,
    Problem,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct DetailField {
    /// Per language; agents read theirs, else English.
    pub label: Label,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct DetailLink {
    /// Per language; agents read theirs, else English.
    pub label: Label,
    /// An `http` or `https` URL in the add-on's system.
    pub url: String,
}

/// The live details one add-on gives of one asset.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct AssetLiveDetails {
    pub asset_id: Uuid,
    /// The add-on asked.
    pub addon: String,
    /// `null` when they are unavailable: the add-on did not answer in time, answered with an
    /// error or with something that is not live details, or is switched off.
    pub details: Option<LiveDetails>,
}

/// The most an add-on's answer may weigh; a larger one is not live details.
pub const MAX_ANSWER_BYTES: usize = 64 * 1024;
/// The most fields an answer may have.
pub const MAX_FIELDS: usize = 20;
/// The most links an answer may have.
pub const MAX_LINKS: usize = 10;
/// The longest label, in characters.
pub const MAX_LABEL: usize = 100;
/// The longest value, in characters.
pub const MAX_VALUE: usize = 500;
/// The longest link, in characters.
pub const MAX_URL: usize = 2000;

/// The most reads of one add-on under way at once for one request of the agents: the others wait
/// for a free one, within the same deadline.
pub const READS_PER_ADDON: usize = 8;

/// The client the core asks add-ons for live details with. It follows no redirect: the signature
/// is for the add-on's own URL, not for wherever that sends it.
pub fn http_client() -> anyhow::Result<reqwest::Client> {
    crate::http_client::http_client(reqwest::redirect::Policy::none())
}

/// An add-on to ask for the live details of an asset, with the reference it knows the asset by.
/// Not `Debug`: it holds the add-on's event secret.
#[derive(Clone, sqlx::FromRow)]
pub struct ToAsk {
    pub asset_id: Uuid,
    pub addon: String,
    pub url: String,
    pub event_secret: String,
    /// A switched-off add-on is not asked: its details are unavailable.
    pub enabled: bool,
    pub reference: String,
}

/// The one place that says which add-ons are asked for the live details of `assets`, and with
/// which reference: for each active asset of a customer that is not offboarded, its source with
/// the reference it supplied the asset with, then each add-on linked to it, by name, with the
/// reference that add-on knows it by. A retired asset left its source's system, and an
/// offboarded customer's assets are no longer the add-ons' to report on.
pub async fn addons_to_ask(db: &PgPool, assets: &[Uuid]) -> Result<Vec<ToAsk>> {
    Ok(sqlx::query_as(
        "WITH asked (asset_id, addon, reference, is_source) AS (
             SELECT id, source_addon, source_ref, true FROM assets
             WHERE id = ANY($1) AND source_addon IS NOT NULL AND source_ref IS NOT NULL
             UNION ALL
             SELECT asset_id, addon, reference, false FROM asset_linked_addons
             WHERE asset_id = ANY($1)
         )
         SELECT a.id AS asset_id, d.name AS addon, d.url, d.event_secret, d.enabled,
                k.reference
         FROM asked k
         JOIN assets a ON a.id = k.asset_id
         JOIN customers c ON c.id = a.customer_id
         JOIN addons d ON d.name = k.addon
         WHERE a.retired_at IS NULL AND c.offboarded_at IS NULL
         ORDER BY array_position($1, a.id), k.is_source DESC, d.name",
    )
    .bind(assets)
    .fetch_all(db)
    .await?)
}

/// The live details of `assets`, from every add-on to ask for each, in the order of `assets`.
/// Every add-on is asked at the same time, each for at most [`READS_PER_ADDON`] assets at once,
/// and every read ends within one deadline [`TIMEOUT`] away, so the answer comes within it however
/// slow the add-ons are; a read not answered by then is unavailable.
pub async fn of_assets(
    db: &PgPool,
    client: &reqwest::Client,
    assets: &[Uuid],
) -> Result<Vec<AssetLiveDetails>> {
    use futures_util::StreamExt;
    // Read first: no connection is held while the add-ons are asked.
    let to_ask = addons_to_ask(db, assets).await?;
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    let mut by_addon: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, one) in to_ask.iter().enumerate() {
        by_addon.entry(&one.addon).or_default().push(i);
    }
    let each_addon = by_addon.into_values().map(|reads| {
        futures_util::stream::iter(reads)
            .map(|i| {
                let one = &to_ask[i];
                async move { (i, read(client, one, deadline).await) }
            })
            .buffer_unordered(READS_PER_ADDON)
            .collect::<Vec<_>>()
    });
    let mut details: Vec<Option<LiveDetails>> = vec![None; to_ask.len()];
    for (i, answer) in futures_util::future::join_all(each_addon)
        .await
        .into_iter()
        .flatten()
    {
        details[i] = answer;
    }
    Ok(to_ask
        .into_iter()
        .zip(details)
        .map(|(one, details)| AssetLiveDetails {
            asset_id: one.asset_id,
            addon: one.addon,
            details,
        })
        .collect())
}

/// The live details of a customer's active assets.
pub async fn of_customer(
    db: &PgPool,
    client: &reqwest::Client,
    customer: Uuid,
) -> Result<Vec<AssetLiveDetails>> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM customers WHERE id = $1)")
        .bind(customer)
        .fetch_one(db)
        .await?;
    if !exists {
        return Err(crate::error::Error::NotFound("customer"));
    }
    let assets: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM assets WHERE customer_id = $1 AND retired_at IS NULL")
            .bind(customer)
            .fetch_all(db)
            .await?;
    of_assets(db, client, &assets).await
}

/// The live details of the assets a ticket links.
pub async fn of_ticket(
    db: &PgPool,
    client: &reqwest::Client,
    number: i64,
) -> Result<Vec<AssetLiveDetails>> {
    let ticket: Uuid = sqlx::query_scalar("SELECT id FROM tickets WHERE number = $1")
        .bind(number)
        .fetch_optional(db)
        .await?
        .ok_or(crate::error::Error::NotFound("ticket"))?;
    let assets: Vec<Uuid> = sqlx::query_scalar(
        "SELECT ta.asset_id FROM ticket_assets ta
         JOIN assets a ON a.id = ta.asset_id
         JOIN asset_kinds k ON k.id = a.kind_id
         WHERE ta.ticket_id = $1 ORDER BY lower(k.name), a.name",
    )
    .bind(ticket)
    .fetch_all(db)
    .await?;
    of_assets(db, client, &assets).await
}

/// What the core signs for a read of the live details of the asset an add-on knows by
/// `reference`, in place of a body: `GET /assets/<reference>`, the reference as it is, not
/// percent-encoded.
pub fn signed_read(reference: &str) -> String {
    format!("GET /assets/{reference}")
}

/// One read, ended by `deadline`: the live details, or none when they are unavailable, with the
/// reason in the log. A switched-off add-on is not asked.
async fn read(
    client: &reqwest::Client,
    one: &ToAsk,
    deadline: tokio::time::Instant,
) -> Option<LiveDetails> {
    if !one.enabled {
        return None;
    }
    let why = match tokio::time::timeout_at(deadline, ask(client, one)).await {
        Ok(Ok(details)) => return Some(details),
        Ok(Err(why)) => why,
        Err(_) => anyhow::anyhow!("no answer within {} seconds", TIMEOUT.as_secs()),
    };
    tracing::warn!(addon = %one.addon, asset = %one.asset_id, why = format!("{why:#}"),
        "live details unavailable");
    None
}

/// Asks one add-on; anything but a 200 with live details is an error.
async fn ask(client: &reqwest::Client, one: &ToAsk) -> anyhow::Result<LiveDetails> {
    use anyhow::{Context, bail};
    let mut url = url::Url::parse(&one.url).context("the add-on's URL")?;
    url.path_segments_mut()
        .map_err(|()| anyhow::anyhow!("the add-on's URL cannot have a path"))?
        .pop_if_empty()
        .push("assets")
        .push(&one.reference);
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let signature = crate::addons::signature(
        &one.event_secret,
        timestamp,
        signed_read(&one.reference).as_bytes(),
    );
    let mut response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(crate::addons::HEADER_TIMESTAMP, timestamp.to_string())
        .header(crate::addons::HEADER_SIGNATURE, signature)
        .send()
        .await?;
    if response.status() != reqwest::StatusCode::OK {
        bail!("the add-on answered {}", response.status());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len() + chunk.len() > MAX_ANSWER_BYTES {
            bail!("the answer is larger than {MAX_ANSWER_BYTES} bytes");
        }
        body.extend_from_slice(&chunk);
    }
    parse(&body).map_err(anyhow::Error::msg)
}

/// A label as an add-on sends it: one per language, or a single one taken as English.
#[derive(Deserialize)]
#[serde(untagged)]
enum LabelInput {
    PerLanguage(Label),
    English(String),
}

#[derive(Deserialize)]
struct FieldInput {
    label: LabelInput,
    value: String,
}

#[derive(Deserialize)]
struct LinkInput {
    label: LabelInput,
    url: String,
}

#[derive(Deserialize)]
struct AnswerInput {
    state: DetailState,
    #[serde(default)]
    fields: Vec<FieldInput>,
    #[serde(default)]
    links: Vec<LinkInput>,
}

/// Live details out of an add-on's answer, or why it is not live details: it breaks the
/// contract (sdk/README.md) as a whole, never in part.
pub fn parse(body: &[u8]) -> std::result::Result<LiveDetails, String> {
    let answer: AnswerInput =
        serde_json::from_slice(body).map_err(|e| format!("the answer is not live details: {e}"))?;
    if answer.fields.len() > MAX_FIELDS {
        return Err(format!("more than {MAX_FIELDS} fields"));
    }
    if answer.links.len() > MAX_LINKS {
        return Err(format!("more than {MAX_LINKS} links"));
    }
    let fields = answer
        .fields
        .into_iter()
        .map(|field| {
            let value = field.value.trim();
            if value.chars().count() > MAX_VALUE {
                return Err(format!("a value is longer than {MAX_VALUE} characters"));
            }
            Ok(DetailField {
                label: clean_label(field.label)?,
                value: value.to_owned(),
            })
        })
        .collect::<std::result::Result<_, String>>()?;
    let links = answer
        .links
        .into_iter()
        .map(|link| {
            let url = link.url.trim();
            if url.chars().count() > MAX_URL {
                return Err(format!("a link is longer than {MAX_URL} characters"));
            }
            let parsed = url::Url::parse(url).map_err(|_| format!("{url:?} is not a URL"))?;
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err(format!("the link {url:?} is not http or https"));
            }
            Ok(DetailLink {
                label: clean_label(link.label)?,
                url: url.to_owned(),
            })
        })
        .collect::<std::result::Result<_, String>>()?;
    Ok(LiveDetails {
        state: answer.state,
        fields,
        links,
    })
}

/// A label with language tags of 2 to 8 lowercase letters, each text trimmed and not longer
/// than [`MAX_LABEL`], empty ones dropped, and at least one left.
fn clean_label(label: LabelInput) -> std::result::Result<Label, String> {
    let label = match label {
        LabelInput::PerLanguage(label) => label,
        LabelInput::English(text) => Label::from([("en".to_owned(), text)]),
    };
    let mut clean = Label::new();
    for (language, text) in label {
        let valid_tag =
            (2..=8).contains(&language.len()) && language.chars().all(|c| c.is_ascii_lowercase());
        if !valid_tag {
            return Err(format!("{language:?} is not a language tag"));
        }
        let text = text.trim();
        if text.chars().count() > MAX_LABEL {
            return Err(format!("a label is longer than {MAX_LABEL} characters"));
        }
        if !text.is_empty() {
            clean.insert(language, text.to_owned());
        }
    }
    if clean.is_empty() {
        return Err("a label is empty".into());
    }
    Ok(clean)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{
        self, Answer, addon_at, fake_addon, live_details_answer as healthy,
        supplied_asset as supplied,
    };
    use axum::http::StatusCode;
    use serde_json::{Value, json};

    fn label(pairs: &[(&str, &str)]) -> Label {
        pairs
            .iter()
            .map(|(l, t)| ((*l).to_owned(), (*t).to_owned()))
            .collect()
    }

    /// The core asks an asset's source with the reference it knows the asset by, signed with
    /// the add-on's event secret, and gives its fields, state and links, a single label taken as
    /// English.
    #[sqlx::test]
    async fn the_source_gives_the_live_details_of_its_asset(db: PgPool) {
        let (url, asked) = fake_addon(|_| Answer::Json(healthy())).await;
        let watchtower = addon_at(&db, "watchtower", &url).await;
        let brightfield = testing::customer(&db, "Brightfield").await;
        let srv = supplied(&db, brightfield, "SRV-01", "watchtower", "host 10/42").await;

        let client = http_client().unwrap();
        let details = of_assets(&db, &client, &[srv]).await.unwrap();
        assert_eq!(
            details,
            [AssetLiveDetails {
                asset_id: srv,
                addon: "watchtower".into(),
                details: Some(LiveDetails {
                    state: DetailState::Warning,
                    fields: vec![
                        DetailField {
                            label: label(&[("en", "Disk /var"), ("it", "Disco /var")]),
                            value: "91% full".into(),
                        },
                        DetailField {
                            label: label(&[("en", "Uptime")]),
                            value: "12 days".into(),
                        },
                    ],
                    links: vec![DetailLink {
                        label: label(&[("en", "Open in Watchtower")]),
                        url: "https://watchtower.example.com/hosts/1042".into(),
                    }],
                }),
            }]
        );

        let asked = asked.lock().unwrap();
        assert_eq!(asked.len(), 1);
        let (reference, headers) = &asked[0];
        assert_eq!(
            reference, "host 10/42",
            "one path segment, decoded by the add-on"
        );
        let timestamp: u64 = headers[crate::addons::HEADER_TIMESTAMP]
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(
            headers[crate::addons::HEADER_SIGNATURE].to_str().unwrap(),
            crate::addons::signature(
                &watchtower.event_secret,
                timestamp,
                b"GET /assets/host 10/42"
            )
        );
    }

    /// An add-on built on the Rust SDK serves the core's read: the SDK's router verifies the
    /// core's signature with the event secret, hands its handler the reference as the add-on
    /// gave it, and its live details and its 404 reach the agents as the core gives them.
    #[sqlx::test]
    async fn an_addon_built_on_the_sdk_serves_the_core_s_read(db: PgPool) {
        use hd_addon_sdk::live_details::{self as sdk, in_english};
        let watchtower = crate::addons::register(
            &db,
            "watchtower",
            "http://127.0.0.1:9",
            &[],
            &[crate::addons::CATALOG_WRITE.into()],
        )
        .await
        .unwrap();
        let app = hd_addon_sdk::server::live_details_router(
            hd_addon_sdk::Verifier::new(watchtower.event_secret),
            |reference: String| async move {
                Ok((reference == "host 10/42").then(|| sdk::LiveDetails {
                    state: sdk::DetailState::Problem,
                    fields: vec![sdk::Field::new(in_english("Disk /var"), "100% full")],
                    links: vec![sdk::Link::new(
                        in_english("Open"),
                        "https://watchtower.example.com/hosts/1042",
                    )],
                }))
            },
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        // Behind a path of its own, as an add-on may be.
        let url = format!("http://{}/watchtower", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, axum::Router::new().nest("/watchtower", app)).await
        });
        sqlx::query("UPDATE addons SET url = $1")
            .bind(&url)
            .execute(&db)
            .await
            .unwrap();
        let brightfield = testing::customer(&db, "Brightfield").await;
        let srv = supplied(&db, brightfield, "SRV-01", "watchtower", "host 10/42").await;
        let unknown = supplied(&db, brightfield, "SRV-02", "watchtower", "host-7").await;

        let client = http_client().unwrap();
        let details = of_assets(&db, &client, &[srv, unknown]).await.unwrap();
        assert_eq!(
            details,
            [
                AssetLiveDetails {
                    asset_id: srv,
                    addon: "watchtower".into(),
                    details: Some(LiveDetails {
                        state: DetailState::Problem,
                        fields: vec![DetailField {
                            label: label(&[("en", "Disk /var")]),
                            value: "100% full".into(),
                        }],
                        links: vec![DetailLink {
                            label: label(&[("en", "Open")]),
                            url: "https://watchtower.example.com/hosts/1042".into(),
                        }],
                    }),
                },
                AssetLiveDetails {
                    asset_id: unknown,
                    addon: "watchtower".into(),
                    details: None,
                },
            ],
            "the add-on answers 404 for host-7, which agents see as unavailable"
        );
    }

    /// The core waits 2 seconds for an add-on: an answer after 1.5 seconds is shown, one after
    /// 2.5 seconds is not, and the whole read takes no longer than the bound.
    #[sqlx::test]
    async fn the_core_waits_two_seconds_for_an_answer(db: PgPool) {
        let (url, _) = fake_addon(|reference| match reference {
            "in-time" => Answer::Late(Duration::from_millis(1500), healthy()),
            _ => Answer::Late(Duration::from_millis(2500), healthy()),
        })
        .await;
        addon_at(&db, "watchtower", &url).await;
        let brightfield = testing::customer(&db, "Brightfield").await;
        let in_time = supplied(&db, brightfield, "SRV-01", "watchtower", "in-time").await;
        let too_late = supplied(&db, brightfield, "SRV-02", "watchtower", "too-late").await;

        let started = std::time::Instant::now();
        let details = of_assets(&db, &http_client().unwrap(), &[in_time, too_late])
            .await
            .unwrap();
        let took = started.elapsed();
        assert!(took < Duration::from_secs(3), "took {took:?}");
        let shown: Vec<(Uuid, bool)> = details
            .iter()
            .map(|d| (d.asset_id, d.details.is_some()))
            .collect();
        assert_eq!(shown, [(in_time, true), (too_late, false)]);
    }

    /// The core asks one add-on for at most 8 assets at a time, all within one 2-second deadline:
    /// with 24 assets answered in 0.7 seconds each, the first 16 are shown and the last 8, not
    /// answered by the deadline, are unavailable.
    #[sqlx::test]
    async fn an_addon_is_asked_a_few_assets_at_a_time_within_one_deadline(db: PgPool) {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        // Counts the reads it is answering at once, and the most it answered at once.
        let in_flight = Arc::new(AtomicUsize::new(0));
        let most = Arc::new(AtomicUsize::new(0));
        let app = axum::Router::new().route(
            "/assets/{reference}",
            axum::routing::get({
                let (in_flight, most) = (in_flight.clone(), most.clone());
                move || async move {
                    let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    most.fetch_max(now, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(700)).await;
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                    axum::Json(healthy())
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });
        addon_at(&db, "watchtower", &url).await;
        let brightfield = testing::customer(&db, "Brightfield").await;
        let mut assets = Vec::new();
        for n in 0..24 {
            let name = format!("SRV-{n:02}");
            assets.push(supplied(&db, brightfield, &name, "watchtower", &name).await);
        }

        let started = std::time::Instant::now();
        let details = of_assets(&db, &http_client().unwrap(), &assets)
            .await
            .unwrap();
        let took = started.elapsed();
        assert!(took < Duration::from_secs(3), "took {took:?}");
        assert_eq!(details.len(), 24);
        assert_eq!(details.iter().filter(|d| d.details.is_some()).count(), 16);
        assert_eq!(most.load(Ordering::SeqCst), READS_PER_ADDON);
    }

    /// The core does not follow an add-on's redirect: the signature is for that add-on only.
    #[sqlx::test]
    async fn a_redirect_is_not_followed(db: PgPool) {
        let (elsewhere, elsewhere_asked) = fake_addon(|_| Answer::Json(healthy())).await;
        let (url, _) = fake_addon(move |reference| {
            Answer::Redirect(format!("{elsewhere}/assets/{reference}"))
        })
        .await;
        addon_at(&db, "watchtower", &url).await;
        let brightfield = testing::customer(&db, "Brightfield").await;
        let srv = supplied(&db, brightfield, "SRV-01", "watchtower", "host-1042").await;

        let details = of_assets(&db, &http_client().unwrap(), &[srv])
            .await
            .unwrap();
        assert_eq!(details[0].details, None);
        assert!(elsewhere_asked.lock().unwrap().is_empty());
    }

    /// An add-on that answers late, with an error status (404 too), with something that is not
    /// live details or more than 64 KiB, or that nothing answers for, has its details
    /// unavailable, and a switched-off one is not even asked; the add-ons are asked all at once,
    /// so one slow add-on does not hold up the others, and the answer comes within the timeout.
    #[sqlx::test]
    async fn an_addon_that_does_not_answer_in_time_or_well_has_its_details_unavailable(db: PgPool) {
        let (url, asked) = fake_addon(|reference| match reference {
            "late" => Answer::Late(Duration::from_secs(5), healthy()),
            "late-too" => Answer::Late(Duration::from_secs(5), healthy()),
            "failing" => Answer::Status(StatusCode::INTERNAL_SERVER_ERROR),
            "garbage" => Answer::Garbage,
            "wrong-shape" => Answer::Json(json!({"state": "fine", "fields": []})),
            "unknown" => Answer::Status(StatusCode::NOT_FOUND),
            "too-large" => Answer::Json(json!({"state": "ok", "padding": "x".repeat(70_000)})),
            _ => Answer::Json(healthy()),
        })
        .await;
        addon_at(&db, "watchtower", &url).await;
        let (off_url, off_asked) = fake_addon(|_| Answer::Json(healthy())).await;
        addon_at(&db, "inventory", &off_url).await;
        crate::addons::set_enabled(&db, "inventory", false)
            .await
            .unwrap();
        let brightfield = testing::customer(&db, "Brightfield").await;
        let mut assets = Vec::new();
        for reference in [
            "late",
            "late-too",
            "failing",
            "garbage",
            "wrong-shape",
            "unknown",
            "fine",
        ] {
            assets.push(supplied(&db, brightfield, reference, "watchtower", reference).await);
        }
        let off = supplied(&db, brightfield, "PC-0042", "inventory", "pc-42").await;
        assets.push(off);
        // More than 64 KiB, though live details otherwise.
        let too_large = supplied(&db, brightfield, "too-large", "watchtower", "too-large").await;
        assets.push(too_large);
        // Nothing listens where this add-on is.
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let down_url = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        addon_at(&db, "printers", &down_url).await;
        let down = supplied(&db, brightfield, "PRN-01", "printers", "prn-1").await;
        assets.push(down);

        let client = http_client().unwrap();
        let started = std::time::Instant::now();
        let details = of_assets(&db, &client, &assets).await.unwrap();
        let took = started.elapsed();
        assert!(took < Duration::from_secs(4), "took {took:?}");

        let got: Vec<(Uuid, &str, bool)> = details
            .iter()
            .map(|d| (d.asset_id, d.addon.as_str(), d.details.is_some()))
            .collect();
        let expected: Vec<(Uuid, &str, bool)> = vec![
            (assets[0], "watchtower", false),
            (assets[1], "watchtower", false),
            (assets[2], "watchtower", false),
            (assets[3], "watchtower", false),
            (assets[4], "watchtower", false),
            (assets[5], "watchtower", false),
            (assets[6], "watchtower", true),
            (off, "inventory", false),
            (too_large, "watchtower", false),
            (down, "printers", false),
        ];
        assert_eq!(got, expected);
        assert_eq!(asked.lock().unwrap().len(), 8);
        assert!(
            off_asked.lock().unwrap().is_empty(),
            "a switched-off add-on is not asked"
        );
    }

    /// The source of an asset and each add-on linked to it are asked, each with its own
    /// reference, the source first and the linked ones by name; an add-on that is neither is not
    /// asked, and a switched-off linked add-on is unavailable without being asked.
    #[sqlx::test]
    async fn the_source_and_each_linked_addon_are_asked_with_their_own_reference(db: PgPool) {
        let (source_url, source_asked) = fake_addon(|_| Answer::Json(healthy())).await;
        addon_at(&db, "watchtower", &source_url).await;
        let (linked_url, linked_asked) =
            fake_addon(|_| Answer::Json(json!({"state": "problem"}))).await;
        addon_at(&db, "inventory", &linked_url).await;
        let (off_url, off_asked) = fake_addon(|_| Answer::Json(healthy())).await;
        addon_at(&db, "alarms", &off_url).await;
        let (other_url, other_asked) = fake_addon(|_| Answer::Json(healthy())).await;
        addon_at(&db, "printers", &other_url).await;
        let brightfield = testing::customer(&db, "Brightfield").await;
        let srv = supplied(&db, brightfield, "SRV-01", "watchtower", "host-1042").await;
        testing::link_addon(&db, srv, "inventory", "inv-77").await;
        testing::link_addon(&db, srv, "alarms", "alarm-5").await;
        crate::addons::set_enabled(&db, "alarms", false)
            .await
            .unwrap();
        // The printers add-on supplies an asset of its own, but is neither the source of SRV-01
        // nor linked to it.
        supplied(&db, brightfield, "PRN-01", "printers", "prn-1").await;

        let details = of_assets(&db, &http_client().unwrap(), &[srv])
            .await
            .unwrap();
        let got: Vec<(&str, Option<DetailState>)> = details
            .iter()
            .map(|d| (d.addon.as_str(), d.details.as_ref().map(|l| l.state)))
            .collect();
        assert_eq!(
            got,
            [
                ("watchtower", Some(DetailState::Warning)),
                ("alarms", None),
                ("inventory", Some(DetailState::Problem)),
            ]
        );
        let references = |asked: &testing::Asked| -> Vec<String> {
            asked
                .lock()
                .unwrap()
                .iter()
                .map(|(r, _)| r.clone())
                .collect()
        };
        assert_eq!(references(&source_asked), ["host-1042"]);
        assert_eq!(references(&linked_asked), ["inv-77"]);
        assert!(references(&off_asked).is_empty(), "switched off");
        assert!(
            references(&other_asked).is_empty(),
            "neither source nor linked"
        );
    }

    /// Only active assets of customers that are not offboarded are asked about, and only of
    /// their add-ons: a hand-made asset no add-on is linked to has none to ask.
    #[sqlx::test]
    async fn only_the_source_of_an_active_asset_is_asked(db: PgPool) {
        let (url, asked) = fake_addon(|_| Answer::Json(healthy())).await;
        addon_at(&db, "watchtower", &url).await;
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Freight Lines").await;
        let by_hand = testing::asset(&db, brightfield, "Laptop", "PC-0001").await;
        let retired = supplied(&db, brightfield, "SRV-02", "watchtower", "host-2").await;
        sqlx::query("UPDATE assets SET retired_at = now() WHERE id = $1")
            .bind(retired)
            .execute(&db)
            .await
            .unwrap();
        let gone = supplied(&db, harbor, "SRV-03", "watchtower", "host-3").await;
        sqlx::query("UPDATE customers SET offboarded_at = now() WHERE id = $1")
            .bind(harbor)
            .execute(&db)
            .await
            .unwrap();
        let active = supplied(&db, brightfield, "SRV-01", "watchtower", "host-1").await;

        let client = http_client().unwrap();
        let details = of_assets(&db, &client, &[by_hand, retired, gone, active])
            .await
            .unwrap();
        assert_eq!(details.len(), 1);
        assert_eq!(details[0].asset_id, active);
        let asked: Vec<String> = asked
            .lock()
            .unwrap()
            .iter()
            .map(|(r, _)| r.clone())
            .collect();
        assert_eq!(asked, ["host-1"]);
    }

    /// What an add-on answers is checked as a whole: a state of the list, at most 20 fields and
    /// 10 links, labels with language tags, values and links within their length, and links to
    /// http or https only, since agents open them.
    #[test]
    fn an_answer_is_live_details_only_when_it_keeps_the_contract() {
        let parsed = |v: Value| parse(v.to_string().as_bytes());
        let minimal = parsed(json!({"state": "ok"})).unwrap();
        assert_eq!(
            minimal,
            LiveDetails {
                state: DetailState::Ok,
                fields: vec![],
                links: vec![]
            }
        );
        let trimmed = parsed(json!({"state": "problem", "fields": [
            {"label": {"en": " Disk ", "it": " "}, "value": " full "}]}))
        .unwrap();
        assert_eq!(trimmed.fields[0].label, label(&[("en", "Disk")]));
        assert_eq!(trimmed.fields[0].value, "full");

        let field = json!({"label": "Disk", "value": "full"});
        let link = json!({"label": "Open", "url": "https://watchtower.example.com/h/1"});
        for (why, answer) in [
            ("no state", json!({"fields": []})),
            ("unknown state", json!({"state": "critical"})),
            (
                "too many fields",
                json!({"state": "ok", "fields": vec![field.clone(); 21]}),
            ),
            (
                "too many links",
                json!({"state": "ok", "links": vec![link.clone(); 11]}),
            ),
            (
                "a number as value",
                json!({"state": "ok", "fields": [{"label": "Disk", "value": 91}]}),
            ),
            (
                "an empty label",
                json!({"state": "ok", "fields": [{"label": " ", "value": "x"}]}),
            ),
            (
                "a bad language tag",
                json!({"state": "ok", "fields": [{"label": {"EN": "Disk"}, "value": "x"}]}),
            ),
            (
                "a long label",
                json!({"state": "ok", "fields": [{"label": "x".repeat(101), "value": "x"}]}),
            ),
            (
                "a long value",
                json!({"state": "ok", "fields": [{"label": "Disk", "value": "x".repeat(501)}]}),
            ),
            (
                "a script link",
                json!({"state": "ok", "links": [{"label": "Open", "url": "javascript:alert(1)"}]}),
            ),
            (
                "not a URL",
                json!({"state": "ok", "links": [{"label": "Open", "url": "/hosts/1"}]}),
            ),
            (
                "a long link",
                json!({"state": "ok", "links": [{"label": "Open",
                "url": format!("https://watchtower.example.com/{}", "x".repeat(2000))}]}),
            ),
        ] {
            assert!(parsed(answer).is_err(), "{why}");
        }
        assert_eq!(
            parsed(json!({"state": "ok", "fields": vec![field; 20], "links": vec![link; 10]}))
                .map(|d| (d.fields.len(), d.links.len())),
            Ok((20, 10))
        );
    }
}
