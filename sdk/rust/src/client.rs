//! The add-on API: an add-on's calls to the core with `Authorization: Bearer <token>`.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::{
    Asset, Customer, Identity, Service, Supplied, SuppliedAsset, SuppliedDetails, Ticket,
    TicketSummary,
};
use crate::proposals::{Content, Rationale, Target};
use crate::tickets::NewTicket;

/// Why a call to the core failed.
#[derive(Debug)]
pub enum Error {
    /// The core answered with an error status; `message` is its sentence for the reason, such as
    /// "The add-on assistant lacks the scope tickets:read." (409) or "Ticket not found." (404).
    Api { status: u16, message: String },
    /// The core could not be reached, or its answer could not be read.
    Http(reqwest::Error),
    /// The HTTP client could not be built.
    Setup(String),
}

impl Error {
    /// The HTTP status the core answered with, if it answered.
    pub fn status(&self) -> Option<u16> {
        match self {
            Error::Api { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// Worth trying again later: the core was unreachable or failed on its side. A refusal
    /// (4xx) will not change by retrying.
    pub fn is_transient(&self) -> bool {
        match self {
            Error::Api { status, .. } => *status >= 500 || *status == 429,
            Error::Http(e) => !e.is_decode() && !e.is_builder(),
            Error::Setup(_) => false,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Api { status, message } => write!(f, "the core answered {status}: {message}"),
            Error::Http(e) => write!(f, "calling the core failed: {e}"),
            Error::Setup(why) => write!(f, "building the HTTP client failed: {why}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Http(e) => Some(e),
            _ => None,
        }
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Http(e)
    }
}

/// An HTTP client with TLS through rustls and the public web PKI, like the core's own.
pub fn http_client(timeout: Duration) -> Result<reqwest::Client, Error> {
    let roots = tokio_rustls::rustls::RootCertStore::from_iter(
        webpki_roots::TLS_SERVER_ROOTS.iter().cloned(),
    );
    let tls = tokio_rustls::rustls::ClientConfig::builder_with_provider(Arc::new(
        tokio_rustls::rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| Error::Setup(e.to_string()))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    reqwest::Client::builder()
        .tls_backend_preconfigured(tls)
        .timeout(timeout)
        .build()
        .map_err(|e| Error::Setup(e.to_string()))
}

/// The add-on API of one core, called with one add-on's token.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    token: Arc<str>,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
struct Problem {
    error: String,
}

#[derive(Serialize)]
struct NewProposal<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    message_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ticket_number: Option<i64>,
    content: &'a Content,
    rationale: &'a Rationale,
}

#[derive(Deserialize)]
struct Created {
    id: Uuid,
}

#[derive(Deserialize)]
struct Opened {
    number: i64,
}

#[derive(Serialize)]
struct NewNote<'a> {
    body: &'a str,
}

impl Client {
    /// A client for the core at `base_url` (such as `https://helpdesk.example.com`), with a
    /// 30-second timeout per call.
    pub fn new(base_url: &str, token: &str) -> Result<Self, Error> {
        Ok(Self::with_http(
            http_client(Duration::from_secs(30))?,
            base_url,
            token,
        ))
    }

    /// A client that sends its calls through `http`.
    pub fn with_http(http: reqwest::Client, base_url: &str, token: &str) -> Self {
        Client {
            http,
            base: base_url.trim_end_matches('/').to_owned(),
            token: token.trim().into(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/addon{path}", self.base)
    }

    async fn read<T: DeserializeOwned>(response: reqwest::Response) -> Result<T, Error> {
        if response.status().is_success() {
            return Ok(response.json().await?);
        }
        Err(Self::refusal(response).await)
    }

    /// An answer without a body, such as 204.
    async fn read_nothing(response: reqwest::Response) -> Result<(), Error> {
        if response.status().is_success() {
            return Ok(());
        }
        Err(Self::refusal(response).await)
    }

    /// The core's sentence for an error status.
    async fn refusal(response: reqwest::Response) -> Error {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        let message = serde_json::from_str::<Problem>(&text)
            .map(|p| p.error)
            .unwrap_or_else(|_| {
                status
                    .canonical_reason()
                    .unwrap_or("unexpected answer")
                    .to_owned()
            });
        Error::Api {
            status: status.as_u16(),
            message,
        }
    }

    async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T, Error> {
        let response = self
            .http
            .get(self.url(path))
            .bearer_auth(&self.token)
            .query(query)
            .send()
            .await?;
        Self::read(response).await
    }

    /// The add-on's name and scopes. Needs no scope.
    pub async fn whoami(&self) -> Result<Identity, Error> {
        self.get("/whoami", &[]).await
    }

    /// Customers that can get tickets, with their mail domains and external references. Scope
    /// `directory:read`.
    pub async fn customers(&self) -> Result<Vec<Customer>, Error> {
        self.get("/customers", &[]).await
    }

    /// A customer's active assets. Scope `directory:read`.
    pub async fn customer_assets(&self, customer: Uuid) -> Result<Vec<Asset>, Error> {
        self.get(&format!("/customers/{customer}/assets"), &[])
            .await
    }

    /// The active services a customer uses. Scope `directory:read`.
    pub async fn customer_services(&self, customer: Uuid) -> Result<Vec<Service>, Error> {
        self.get(&format!("/customers/{customer}/services"), &[])
            .await
    }

    /// Tickets that are not closed whose requester has this address among theirs, most recent
    /// first.
    /// Scope `tickets:read`.
    pub async fn open_tickets_of_requester(
        &self,
        email: &str,
    ) -> Result<Vec<TicketSummary>, Error> {
        self.get("/tickets", &[("requester_email", email)]).await
    }

    /// Tickets of a customer that are not closed, most recent first. Scope `tickets:read`.
    pub async fn open_tickets_of_customer(
        &self,
        customer: Uuid,
    ) -> Result<Vec<TicketSummary>, Error> {
        self.get("/tickets", &[("customer", &customer.to_string())])
            .await
    }

    /// A ticket with its conversation and links. Scope `tickets:read`.
    pub async fn ticket(&self, number: i64) -> Result<Ticket, Error> {
        self.get(&format!("/tickets/{number}"), &[]).await
    }

    /// Creates the asset the add-on knows by `asset.reference`, or updates the one it supplied
    /// before with that reference (bringing it back if it retired it), so that the add-on can send
    /// its whole inventory again safely. The core refuses (409) an unknown kind, a site that is not
    /// one of the customer's active sites, and a reference of another customer's asset. Scope
    /// `catalog:write`.
    pub async fn supply_asset(&self, asset: &SuppliedAsset) -> Result<Supplied, Error> {
        let response = self
            .http
            .post(self.url("/assets"))
            .bearer_auth(&self.token)
            .json(asset)
            .send()
            .await?;
        let created = response.status() == reqwest::StatusCode::CREATED;
        Ok(Supplied {
            asset: Self::read(response).await?,
            created,
        })
    }

    /// Updates an asset the add-on is the source of; the core refuses (409) one entered by hand or
    /// supplied by another add-on. Scope `catalog:write`.
    pub async fn update_asset(&self, id: Uuid, details: &SuppliedDetails) -> Result<Asset, Error> {
        let response = self
            .http
            .put(self.url(&format!("/assets/{id}")))
            .bearer_auth(&self.token)
            .json(details)
            .send()
            .await?;
        Self::read(response).await
    }

    /// Retires an asset the add-on is the source of, when it leaves the add-on's system; retiring
    /// it again changes nothing. Scope `catalog:write`.
    pub async fn retire_asset(&self, id: Uuid) -> Result<(), Error> {
        let response = self
            .http
            .post(self.url(&format!("/assets/{id}/retire")))
            .bearer_auth(&self.token)
            .send()
            .await?;
        Self::read_nothing(response).await
    }

    /// Proposes `content` on `target`, with a `rationale` the agent reads: one sentence per
    /// language listed by [`Client::whoami`] (see [`crate::proposals::in_english`] for a single
    /// English sentence). It replaces the open proposal of the same kind there. Returns the
    /// proposal's id. The core answers 409 when the proposal no longer applies. Scope
    /// `proposals:write`.
    pub async fn propose(
        &self,
        target: Target,
        content: &Content,
        rationale: &Rationale,
    ) -> Result<Uuid, Error> {
        let (message_id, ticket_number) = match target {
            Target::Message(id) => (Some(id), None),
            Target::Ticket(number) => (None, Some(number)),
        };
        let response = self
            .http
            .post(self.url("/proposals"))
            .bearer_auth(&self.token)
            .json(&NewProposal {
                message_id,
                ticket_number,
                content,
                rationale,
            })
            .send()
            .await?;
        Ok(Self::read::<Created>(response).await?.id)
    }

    /// Opens a ticket without a requester for an active customer, with a priority and the
    /// customer's active assets and services; replies on it go to the customer's key requesters.
    /// Returns its number. The core answers 409, with a sentence naming it, when an asset or
    /// service does not fit the customer or is retired, and 404 when one does not exist. Scope
    /// `tickets:write`.
    pub async fn open_ticket(&self, ticket: &NewTicket) -> Result<i64, Error> {
        let response = self
            .http
            .post(self.url("/tickets"))
            .bearer_auth(&self.token)
            .json(ticket)
            .send()
            .await?;
        Ok(Self::read::<Opened>(response).await?.number)
    }

    /// Adds an internal note to any ticket, closed ones too: agents see it with the add-on as its
    /// author, and it is never sent outside the help desk. Scope `tickets:write`.
    pub async fn add_note(&self, number: i64, body: &str) -> Result<(), Error> {
        let response = self
            .http
            .post(self.url(&format!("/tickets/{number}/notes")))
            .bearer_auth(&self.token)
            .json(&NewNote { body })
            .send()
            .await?;
        Self::read_nothing(response).await
    }
}
