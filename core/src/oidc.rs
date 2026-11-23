//! Signing in through the installation's OpenID Connect identity provider: authorization code
//! with PKCE, state and nonce checked, the ID token validated against the provider's keys
//! (docs/domain-rules.md, "Signing in"; ADR 0008).

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use openidconnect::core::{
    CoreAuthDisplay, CoreAuthPrompt, CoreErrorResponseType, CoreGenderClaim, CoreJsonWebKey,
    CoreJweContentEncryptionAlgorithm, CoreJwsSigningAlgorithm, CoreProviderMetadata,
    CoreResponseType, CoreRevocableToken, CoreRevocationErrorResponse,
    CoreTokenIntrospectionResponse, CoreTokenType,
};
use openidconnect::{
    AdditionalClaims, AuthenticationFlow, AuthorizationCode, ClientId, ClientSecret, CsrfToken,
    EmptyExtraTokenFields, EndpointMaybeSet, EndpointNotSet, EndpointSet, IdTokenFields, IssuerUrl,
    Nonce, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, StandardErrorResponse,
    StandardTokenResponse, TokenResponse,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::IntoParams;

use crate::auth;
use crate::identities::{AuthorizedGroup, Identity};
use crate::sign_in_limits::AddressKey;

/// Where the provider sends the browser back to, under the core's public URL.
pub const CALLBACK_PATH: &str = "/api/session/identity-provider/callback";

/// The identity provider's settings, from the deployment. Without an issuer, a client id and a
/// client secret there is no identity provider and nothing changes.
#[derive(clap::Args, Debug, Clone)]
pub struct Settings {
    /// The OpenID Connect issuer, such as https://login.example.com/realms/staff: the provider's
    /// configuration is read from <issuer>/.well-known/openid-configuration. https:// unless the
    /// provider is on this machine.
    #[arg(long = "oidc-issuer", env = "HD_OIDC_ISSUER")]
    pub issuer: Option<String>,
    /// The client id the provider knows the help desk by.
    #[arg(long = "oidc-client-id", env = "HD_OIDC_CLIENT_ID")]
    pub client_id: Option<String>,
    /// The client's secret. HD_OIDC_CLIENT_SECRET_FILE reads it from a file instead.
    #[arg(
        long = "oidc-client-secret",
        env = "HD_OIDC_CLIENT_SECRET",
        hide_env_values = true
    )]
    pub client_secret: Option<String>,
    /// A file holding the client's secret, such as a mounted Secret.
    #[arg(long = "oidc-client-secret-file", env = "HD_OIDC_CLIENT_SECRET_FILE")]
    pub client_secret_file: Option<PathBuf>,
    /// What the sign-in button names: "Sign in with <label>".
    #[arg(long = "oidc-label", env = "HD_OIDC_LABEL")]
    pub label: Option<String>,
    /// How many hours a session opened through the identity provider lasts (1 to 24).
    #[arg(
        long = "oidc-session-hours",
        env = "HD_OIDC_SESSION_HOURS",
        default_value_t = 12
    )]
    pub session_hours: i64,
    /// The authorized group: only those the provider says are in it sign in, checked at every
    /// sign-in, and someone in it who is not an agent yet becomes one. Unset, only existing
    /// agents sign in.
    #[arg(long = "oidc-authorized-group", env = "HD_OIDC_AUTHORIZED_GROUP")]
    pub authorized_group: Option<String>,
    /// The ID token claim that lists the person's groups (or roles) the authorized group is
    /// looked for in.
    #[arg(
        long = "oidc-group-claim",
        env = "HD_OIDC_GROUP_CLAIM",
        default_value = "groups"
    )]
    pub group_claim: String,
}

/// Where the authorized group is looked for: a claim of the ID token, and the value it must hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupClaim {
    pub claim: String,
    pub value: String,
}

impl GroupClaim {
    /// Whether the ID token's claims put the person in the authorized group: the claim is the
    /// value, or a list that holds it, compared exactly. Anything else is not.
    fn check(&self, claims: &OtherClaims) -> AuthorizedGroup {
        let member = match claims.0.get(&self.claim) {
            Some(serde_json::Value::String(value)) => *value == self.value,
            Some(serde_json::Value::Array(values)) => values
                .iter()
                .any(|value| value.as_str() == Some(self.value.as_str())),
            _ => false,
        };
        if member {
            AuthorizedGroup::Member
        } else {
            AuthorizedGroup::NotMember
        }
    }
}

/// A configured identity provider.
#[derive(Debug, Clone)]
pub struct Config {
    pub issuer: IssuerUrl,
    pub client_id: ClientId,
    pub client_secret: ClientSecret,
    /// The callback under the core's public URL.
    pub redirect: RedirectUrl,
    pub label: String,
    /// How long a session opened through the provider lasts.
    pub session: time::Duration,
    /// The authorized group, when the installation names one.
    pub authorized_group: Option<GroupClaim>,
}

impl Config {
    /// The provider the settings configure, if any. `public_url` is the core's (HD_PUBLIC_URL),
    /// which the provider sends agents back to.
    pub fn from_settings(
        settings: Settings,
        public_url: Option<&str>,
    ) -> anyhow::Result<Option<Config>> {
        let given =
            |value: Option<String>| value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
        let issuer = given(settings.issuer);
        let client_id = given(settings.client_id);
        let secret = given(settings.client_secret);
        let secret_file = settings.client_secret_file;
        let authorized_group = given(settings.authorized_group);
        if issuer.is_none()
            && client_id.is_none()
            && secret.is_none()
            && secret_file.is_none()
            && authorized_group.is_none()
        {
            return Ok(None);
        }
        let mut missing = Vec::new();
        if issuer.is_none() {
            missing.push("HD_OIDC_ISSUER");
        }
        if client_id.is_none() {
            missing.push("HD_OIDC_CLIENT_ID");
        }
        if secret.is_none() && secret_file.is_none() {
            missing.push("HD_OIDC_CLIENT_SECRET (or HD_OIDC_CLIENT_SECRET_FILE)");
        }
        let public_url = given(public_url.map(str::to_owned));
        if public_url.is_none() {
            missing.push("HD_PUBLIC_URL");
        }
        if !missing.is_empty() {
            bail!(
                "the OpenID Connect identity provider is configured in part: set {} too",
                missing.join(", ")
            );
        }
        let client_secret = match (secret, secret_file) {
            (Some(_), Some(_)) => {
                bail!("set HD_OIDC_CLIENT_SECRET or HD_OIDC_CLIENT_SECRET_FILE, not both")
            }
            (Some(secret), None) => secret,
            (None, Some(file)) => std::fs::read_to_string(&file)
                .with_context(|| format!("reading HD_OIDC_CLIENT_SECRET_FILE {}", file.display()))?
                .trim()
                .to_owned(),
            (None, None) => unreachable!("checked above"),
        };
        if client_secret.is_empty() {
            bail!("the OpenID Connect client secret is empty");
        }
        let issuer = issuer.unwrap_or_default();
        secure_unless_local(&issuer, "HD_OIDC_ISSUER")?;
        let public_url = public_url.unwrap_or_default();
        secure_unless_local(&public_url, "HD_PUBLIC_URL")?;
        if !(1..=24).contains(&settings.session_hours) {
            bail!("HD_OIDC_SESSION_HOURS must be between 1 and 24");
        }
        let authorized_group = match authorized_group {
            None => None,
            Some(value) => {
                let claim = settings.group_claim.trim();
                if claim.is_empty() {
                    bail!(
                        "HD_OIDC_GROUP_CLAIM is empty: name the claim the authorized group is in"
                    );
                }
                Some(GroupClaim {
                    claim: claim.to_owned(),
                    value,
                })
            }
        };
        let redirect = format!("{}{CALLBACK_PATH}", public_url.trim_end_matches('/'));
        Ok(Some(Config {
            issuer: IssuerUrl::new(issuer).context("HD_OIDC_ISSUER is not a URL")?,
            client_id: ClientId::new(client_id.unwrap_or_default()),
            client_secret: ClientSecret::new(client_secret),
            redirect: RedirectUrl::new(redirect).context("HD_PUBLIC_URL is not a URL")?,
            label: given(settings.label).unwrap_or_else(|| "OpenID Connect".to_owned()),
            session: time::Duration::hours(settings.session_hours),
            authorized_group,
        }))
    }
}

/// The configured identity provider, as the core talks to it.
#[derive(Debug)]
pub struct Provider {
    config: Config,
    http: reqwest::Client,
}

impl Provider {
    pub fn new(config: Config) -> anyhow::Result<Arc<Provider>> {
        // The provider's answers are used as they come: following a redirect could lead the
        // core's requests elsewhere.
        let http = crate::http_client::http_client(reqwest::redirect::Policy::none())?;
        Ok(Arc::new(Provider { config, http }))
    }

    /// What the sign-in button names.
    pub fn label(&self) -> &str {
        &self.config.label
    }

    /// How long a session opened through the provider lasts.
    pub fn session(&self) -> time::Duration {
        self.config.session
    }

    /// The provider's endpoints and keys, read from its discovery document every time: a sign-in
    /// is rare enough, and a key the provider rotated is seen at once.
    async fn client(&self) -> anyhow::Result<Client> {
        let http = |request| send(self.http.clone(), request);
        let metadata = CoreProviderMetadata::discover_async(self.config.issuer.clone(), &http)
            .await
            .context("reading the identity provider's configuration")?;
        Ok(Client::from_provider_metadata(
            metadata,
            self.config.client_id.clone(),
            Some(self.config.client_secret.clone()),
        )
        .set_redirect_uri(self.config.redirect.clone()))
    }

    /// Sets a sign-in out from `address`: where to send the browser, and the state it must
    /// bring back; or, with too many sign-ins under way from that address, how long until one
    /// ends. The provider is asked only for a sign-in allowed to set out.
    pub async fn begin(&self, db: &PgPool, address: IpAddr) -> anyhow::Result<Begin> {
        forget_ended_sign_ins(db).await;
        let address = AddressKey::new(address).0;
        let state = CsrfToken::new_random();
        let nonce = Nonce::new_random();
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let state_hash = auth::token_hash(state.secret());
        let mut tx = db.begin().await?;
        // One address at a time, so that sign-ins set out at once are all counted.
        sqlx::query("SELECT pg_advisory_xact_lock($1, hashtext($2))")
            .bind(ADDRESS_LOCK_CLASS)
            .bind(&address)
            .execute(&mut *tx)
            .await?;
        let (under_way, first_ends_in): (i64, Option<f64>) = sqlx::query_as(
            "SELECT count(*), extract(epoch FROM min(expires_at) - now())::float8
             FROM identity_provider_sign_ins WHERE address = $1 AND expires_at > now()",
        )
        .bind(&address)
        .fetch_one(&mut *tx)
        .await?;
        if under_way >= UNDER_WAY_PER_ADDRESS {
            let wait = first_ends_in.unwrap_or_default().max(1.0);
            return Ok(Begin::TooMany {
                retry_after: Duration::from_secs_f64(wait),
            });
        }
        sqlx::query(
            "INSERT INTO identity_provider_sign_ins
                 (state_hash, nonce, pkce_verifier, address, expires_at)
             VALUES ($1, $2, $3, $4, now() + make_interval(secs => $5))",
        )
        .bind(&state_hash)
        .bind(nonce.secret())
        .bind(verifier.secret())
        .bind(&address)
        .bind(SIGN_IN_TIME.as_secs_f64())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        let client = match self.client().await {
            Ok(client) => client,
            Err(error) => {
                // Nothing will come back: it does not count as under way.
                let _ = sqlx::query("DELETE FROM identity_provider_sign_ins WHERE state_hash = $1")
                    .bind(&state_hash)
                    .execute(db)
                    .await;
                return Err(error);
            }
        };
        let state_kept = state.secret().clone();
        let (url, _, _) = client
            .authorize_url(
                AuthenticationFlow::<CoreResponseType>::AuthorizationCode,
                move || state,
                move || nonce,
            )
            .add_scope(Scope::new("email".to_owned()))
            .add_scope(Scope::new("profile".to_owned()))
            .set_pkce_challenge(challenge)
            .url();
        Ok(Begin::Redirect(ProviderRedirect {
            url,
            state: state_kept,
        }))
    }

    /// Checks the browser's return from the provider and the ID token it leads to: who signed
    /// in. `state_cookie` is the state the browser kept from [`Provider::begin`].
    pub async fn finish(
        &self,
        db: &PgPool,
        state_cookie: Option<&str>,
        query: &CallbackQuery,
    ) -> anyhow::Result<Identity> {
        if let Some(error) = &query.error {
            bail!("the identity provider answered {error}");
        }
        let (Some(code), Some(state)) = (&query.code, &query.state) else {
            bail!("the identity provider's answer has no code or state");
        };
        if state_cookie != Some(state.as_str()) {
            bail!("the state is not the one this browser set out with");
        }
        let pending: Option<(String, String)> = sqlx::query_as(
            "DELETE FROM identity_provider_sign_ins
             WHERE state_hash = $1 AND expires_at > now()
             RETURNING nonce, pkce_verifier",
        )
        .bind(auth::token_hash(state))
        .fetch_optional(db)
        .await?;
        let Some((nonce, verifier)) = pending else {
            bail!("the sign-in is unknown, used or expired");
        };
        let client = self.client().await?;
        let http = |request| send(self.http.clone(), request);
        let tokens = client
            .exchange_code(AuthorizationCode::new(code.clone()))?
            .set_pkce_verifier(PkceCodeVerifier::new(verifier))
            .request_async(&http)
            .await
            .context("exchanging the code at the identity provider")?;
        let id_token = tokens
            .id_token()
            .context("the identity provider sent no ID token")?;
        let claims = id_token
            .claims(&client.id_token_verifier(), &Nonce::new(nonce))
            .context("checking the ID token")?;
        let name = claims
            .name()
            .and_then(|name| name.get(None))
            .map(|name| name.to_string())
            .or_else(|| {
                let given = claims.given_name().and_then(|n| n.get(None));
                let family = claims.family_name().and_then(|n| n.get(None));
                let parts: Vec<String> =
                    [given.map(|n| n.to_string()), family.map(|n| n.to_string())]
                        .into_iter()
                        .flatten()
                        .collect();
                (!parts.is_empty()).then(|| parts.join(" "))
            });
        Ok(Identity {
            issuer: self.config.issuer.as_str().to_owned(),
            subject: claims.subject().as_str().to_owned(),
            email: claims.email().map(|email| email.as_str().to_owned()),
            email_verified: claims.email_verified() == Some(true),
            name,
            authorized_group: match &self.config.authorized_group {
                None => AuthorizedGroup::NotConfigured,
                Some(group) => group.check(claims.additional_claims()),
            },
        })
    }
}

/// How long the browser has to come back from the provider.
const SIGN_IN_TIME: Duration = Duration::from_secs(10 * 60);
/// How many sign-ins may be under way from one address at once.
const UNDER_WAY_PER_ADDRESS: i64 = 20;
/// The advisory lock class of an address setting sign-ins out (sign_in_limits uses 1 and 2).
const ADDRESS_LOCK_CLASS: i32 = 3;

/// What setting a sign-in out gives.
pub enum Begin {
    Redirect(ProviderRedirect),
    /// Too many sign-ins under way from the address; the first ends after `retry_after`.
    TooMany {
        retry_after: Duration,
    },
}

/// Deletes sign-ins that ended, without waiting for a row another request holds. A failure is
/// only logged: the next sign-in tries again.
async fn forget_ended_sign_ins(db: &PgPool) {
    let deleted = sqlx::query(
        "DELETE FROM identity_provider_sign_ins WHERE state_hash IN (
             SELECT state_hash FROM identity_provider_sign_ins WHERE expires_at <= now()
             LIMIT 1000 FOR UPDATE SKIP LOCKED)",
    )
    .execute(db)
    .await;
    if let Err(error) = deleted {
        tracing::warn!(%error, "deleting ended sign-ins through the identity provider");
    }
}

/// Where to send the browser to sign in at the provider, and the state it must bring back.
pub struct ProviderRedirect {
    /// The provider's page to send the browser to.
    pub url: url::Url,
    /// The state the browser keeps until it comes back.
    pub state: String,
}

/// What the provider sends the browser back with.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct CallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    /// The provider's refusal, such as `access_denied`.
    pub error: Option<String>,
}

/// The ID token's claims the OpenID Connect library does not name, such as the one the
/// authorized group is in.
#[derive(Debug, Deserialize, Serialize)]
struct OtherClaims(HashMap<String, serde_json::Value>);

impl AdditionalClaims for OtherClaims {}

type IdTokenFieldsWithOtherClaims = IdTokenFields<
    OtherClaims,
    EmptyExtraTokenFields,
    CoreGenderClaim,
    CoreJweContentEncryptionAlgorithm,
    CoreJwsSigningAlgorithm,
>;

/// The library's core client, but for the ID token's other claims.
type Client = openidconnect::Client<
    OtherClaims,
    CoreAuthDisplay,
    CoreGenderClaim,
    CoreJweContentEncryptionAlgorithm,
    CoreJsonWebKey,
    CoreAuthPrompt,
    StandardErrorResponse<CoreErrorResponseType>,
    StandardTokenResponse<IdTokenFieldsWithOtherClaims, CoreTokenType>,
    CoreTokenIntrospectionResponse,
    CoreRevocableToken,
    CoreRevocationErrorResponse,
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;

/// Sends the request of the OpenID Connect library through the core's HTTP client.
async fn send(
    http: reqwest::Client,
    request: openidconnect::HttpRequest,
) -> Result<openidconnect::HttpResponse, reqwest::Error> {
    let response = http.execute(request.try_into()?).await?;
    let mut answer = openidconnect::http::Response::builder().status(response.status());
    for (name, value) in response.headers() {
        answer = answer.header(name, value);
    }
    let body = response.bytes().await?.to_vec();
    Ok(answer
        .body(body)
        .expect("a response with the provider's status and headers"))
}

/// Refuses a URL that is not https://, but for a server on this machine.
fn secure_unless_local(url: &str, variable: &str) -> anyhow::Result<()> {
    let parsed = url::Url::parse(url).with_context(|| format!("{variable} is not a URL"))?;
    let local = parsed.host_str().is_some_and(crate::imap::is_local);
    match parsed.scheme() {
        "https" => Ok(()),
        "http" if local => Ok(()),
        _ => bail!("{variable} must be an https:// address (http:// only on this machine)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings {
            issuer: Some("https://login.example.com/realms/staff".to_owned()),
            client_id: Some("helpdesk".to_owned()),
            client_secret: Some("invented-secret".to_owned()),
            client_secret_file: None,
            label: None,
            session_hours: 12,
            authorized_group: None,
            group_claim: "groups".to_owned(),
        }
    }

    #[test]
    fn the_authorized_group_is_read_from_the_groups_claim_unless_another_is_named() {
        let none = Config::from_settings(settings(), PUBLIC_URL)
            .unwrap()
            .unwrap();
        assert_eq!(none.authorized_group, None);
        let blank = Settings {
            authorized_group: Some("  ".to_owned()),
            ..settings()
        };
        let blank = Config::from_settings(blank, PUBLIC_URL).unwrap().unwrap();
        assert_eq!(blank.authorized_group, None);

        let groups = Settings {
            authorized_group: Some(" helpdesk-agents ".to_owned()),
            ..settings()
        };
        let groups = Config::from_settings(groups, PUBLIC_URL).unwrap().unwrap();
        assert_eq!(
            groups.authorized_group,
            Some(GroupClaim {
                claim: "groups".to_owned(),
                value: "helpdesk-agents".to_owned()
            })
        );

        let roles = Settings {
            authorized_group: Some("Helpdesk.Agent".to_owned()),
            group_claim: " roles ".to_owned(),
            ..settings()
        };
        let roles = Config::from_settings(roles, PUBLIC_URL).unwrap().unwrap();
        assert_eq!(
            roles.authorized_group,
            Some(GroupClaim {
                claim: "roles".to_owned(),
                value: "Helpdesk.Agent".to_owned()
            })
        );

        let no_claim = Settings {
            authorized_group: Some("Helpdesk.Agent".to_owned()),
            group_claim: " ".to_owned(),
            ..settings()
        };
        let error = Config::from_settings(no_claim, PUBLIC_URL).unwrap_err();
        assert!(
            format!("{error}").contains("HD_OIDC_GROUP_CLAIM"),
            "{error}"
        );
    }

    #[test]
    fn an_authorized_group_without_a_provider_is_a_partial_configuration() {
        let alone = Settings {
            issuer: None,
            client_id: None,
            client_secret: None,
            authorized_group: Some("helpdesk-agents".to_owned()),
            ..settings()
        };
        let error = Config::from_settings(alone, PUBLIC_URL).unwrap_err();
        assert!(format!("{error}").contains("HD_OIDC_ISSUER"), "{error}");
    }

    fn claims(json: serde_json::Value) -> OtherClaims {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn the_group_claim_holds_the_authorized_group_as_one_of_a_list_or_alone() {
        let rule = GroupClaim {
            claim: "roles".to_owned(),
            value: "Helpdesk.Agent".to_owned(),
        };
        let member = |json| rule.check(&claims(json));
        assert_eq!(
            member(serde_json::json!({"roles": ["Reader", "Helpdesk.Agent"]})),
            AuthorizedGroup::Member
        );
        assert_eq!(
            member(serde_json::json!({"roles": "Helpdesk.Agent"})),
            AuthorizedGroup::Member
        );
        for not in [
            serde_json::json!({"roles": ["Reader"]}),
            serde_json::json!({"roles": []}),
            serde_json::json!({"roles": ["helpdesk.agent"]}),
            serde_json::json!({"roles": "Helpdesk.Agents"}),
            serde_json::json!({"roles": {"Helpdesk.Agent": true}}),
            serde_json::json!({"roles": null}),
            serde_json::json!({"groups": ["Helpdesk.Agent"]}),
            serde_json::json!({}),
        ] {
            assert_eq!(member(not.clone()), AuthorizedGroup::NotMember, "{not}");
        }
    }

    const PUBLIC_URL: Option<&str> = Some("https://helpdesk.example.com/");

    #[test]
    fn without_an_issuer_client_or_secret_there_is_no_provider() {
        let none = Settings {
            issuer: None,
            client_id: None,
            client_secret: None,
            ..settings()
        };
        assert!(Config::from_settings(none, PUBLIC_URL).unwrap().is_none());
    }

    #[test]
    fn a_configured_provider_redirects_back_to_the_public_url() {
        let config = Config::from_settings(settings(), PUBLIC_URL)
            .unwrap()
            .unwrap();
        assert_eq!(
            config.issuer.as_str(),
            "https://login.example.com/realms/staff"
        );
        assert_eq!(
            config.redirect.as_str(),
            "https://helpdesk.example.com/api/session/identity-provider/callback"
        );
        assert_eq!(config.label, "OpenID Connect");
        assert_eq!(config.session, time::Duration::hours(12));
        assert!(!format!("{config:?}").contains("invented-secret"));
    }

    #[test]
    fn a_partial_configuration_is_refused() {
        let error = Config::from_settings(
            Settings {
                client_id: None,
                ..settings()
            },
            PUBLIC_URL,
        )
        .unwrap_err();
        assert!(format!("{error}").contains("HD_OIDC_CLIENT_ID"), "{error}");
        let error = Config::from_settings(settings(), None).unwrap_err();
        assert!(format!("{error}").contains("HD_PUBLIC_URL"), "{error}");
    }

    #[test]
    fn plain_http_is_accepted_only_on_this_machine() {
        assert!(
            Config::from_settings(
                Settings {
                    issuer: Some("http://login.example.com/realms/staff".to_owned()),
                    ..settings()
                },
                PUBLIC_URL
            )
            .is_err()
        );
        assert!(Config::from_settings(settings(), Some("http://helpdesk.example.com")).is_err());
        let local = Settings {
            issuer: Some("http://127.0.0.1:55080/realms/helpdesk".to_owned()),
            ..settings()
        };
        assert!(
            Config::from_settings(local, Some("http://localhost:5173"))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn the_client_secret_comes_from_the_environment_or_a_file() {
        let dir = std::env::temp_dir().join(format!("hd-oidc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("client-secret");
        std::fs::write(&file, "secret-from-a-file\n").unwrap();
        let from_file = Settings {
            client_secret: None,
            client_secret_file: Some(file.clone()),
            ..settings()
        };
        let config = Config::from_settings(from_file, PUBLIC_URL)
            .unwrap()
            .unwrap();
        assert_eq!(config.client_secret.secret(), "secret-from-a-file");
        let both = Settings {
            client_secret_file: Some(file),
            ..settings()
        };
        assert!(Config::from_settings(both, PUBLIC_URL).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_session_lasts_between_one_hour_and_a_day() {
        for hours in [0, 25] {
            assert!(
                Config::from_settings(
                    Settings {
                        session_hours: hours,
                        ..settings()
                    },
                    PUBLIC_URL
                )
                .is_err()
            );
        }
        let config = Config::from_settings(
            Settings {
                session_hours: 8,
                label: Some(" Northgate ".to_owned()),
                ..settings()
            },
            PUBLIC_URL,
        )
        .unwrap()
        .unwrap();
        assert_eq!(config.session, time::Duration::hours(8));
        assert_eq!(config.label, "Northgate");
    }
}
