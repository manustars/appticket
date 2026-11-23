//! Signing in through the installation's OpenID Connect identity provider: authorization code
//! with PKCE, state and nonce checked, the ID token validated against the provider's keys
//! (docs/domain-rules.md, "Signing in"; ADR 0008).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use openidconnect::core::{CoreClient, CoreProviderMetadata, CoreResponseType};
use openidconnect::{
    AuthenticationFlow, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointMaybeSet,
    EndpointNotSet, EndpointSet, IssuerUrl, Nonce, PkceCodeChallenge, PkceCodeVerifier,
    RedirectUrl, Scope, TokenResponse,
};
use serde::Deserialize;
use sqlx::PgPool;
use utoipa::IntoParams;

use crate::auth;
use crate::identities::Identity;

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
    /// The address agents open the help desk at, such as https://helpdesk.example.com: the
    /// provider sends them back to it after they sign in.
    #[arg(long = "public-url", env = "HD_PUBLIC_URL")]
    pub public_url: Option<String>,
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
}

impl Config {
    /// The provider the settings configure, if any.
    pub fn from_settings(settings: Settings) -> anyhow::Result<Option<Config>> {
        let given =
            |value: Option<String>| value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
        let issuer = given(settings.issuer);
        let client_id = given(settings.client_id);
        let secret = given(settings.client_secret);
        let secret_file = settings.client_secret_file;
        if issuer.is_none() && client_id.is_none() && secret.is_none() && secret_file.is_none() {
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
        let public_url = given(settings.public_url);
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
        let redirect = format!("{}{CALLBACK_PATH}", public_url.trim_end_matches('/'));
        Ok(Some(Config {
            issuer: IssuerUrl::new(issuer).context("HD_OIDC_ISSUER is not a URL")?,
            client_id: ClientId::new(client_id.unwrap_or_default()),
            client_secret: ClientSecret::new(client_secret),
            redirect: RedirectUrl::new(redirect).context("HD_PUBLIC_URL is not a URL")?,
            label: given(settings.label).unwrap_or_else(|| "OpenID Connect".to_owned()),
            session: time::Duration::hours(settings.session_hours),
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
        Ok(CoreClient::from_provider_metadata(
            metadata,
            self.config.client_id.clone(),
            Some(self.config.client_secret.clone()),
        )
        .set_redirect_uri(self.config.redirect.clone()))
    }

    /// Sets a sign-in out: where to send the browser, and the state it must bring back.
    pub async fn begin(&self, db: &PgPool) -> anyhow::Result<Begun> {
        let client = self.client().await?;
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let (url, state, nonce) = client
            .authorize_url(
                AuthenticationFlow::<CoreResponseType>::AuthorizationCode,
                CsrfToken::new_random,
                Nonce::new_random,
            )
            .add_scope(Scope::new("email".to_owned()))
            .add_scope(Scope::new("profile".to_owned()))
            .set_pkce_challenge(challenge)
            .url();
        sqlx::query("DELETE FROM identity_provider_sign_ins WHERE expires_at <= now()")
            .execute(db)
            .await?;
        sqlx::query(
            "INSERT INTO identity_provider_sign_ins (state_hash, nonce, pkce_verifier, expires_at)
             VALUES ($1, $2, $3, now() + make_interval(secs => $4))",
        )
        .bind(auth::token_hash(state.secret()))
        .bind(nonce.secret())
        .bind(verifier.secret())
        .bind(SIGN_IN_TIME.as_secs_f64())
        .execute(db)
        .await?;
        Ok(Begun {
            url,
            state: state.secret().clone(),
        })
    }

    /// Checks the browser's return from the provider and the ID token it leads to: who signed
    /// in. `kept_state` is the state the browser kept from [`Provider::begin`].
    pub async fn finish(
        &self,
        db: &PgPool,
        kept_state: Option<&str>,
        back: &Return,
    ) -> anyhow::Result<Identity> {
        if let Some(error) = &back.error {
            bail!("the identity provider answered {error}");
        }
        let (Some(code), Some(state)) = (&back.code, &back.state) else {
            bail!("the identity provider's answer has no code or state");
        };
        if kept_state != Some(state.as_str()) {
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
        })
    }
}

/// How long the browser has to come back from the provider.
const SIGN_IN_TIME: Duration = Duration::from_secs(10 * 60);

/// A sign-in set out.
pub struct Begun {
    /// The provider's page to send the browser to.
    pub url: url::Url,
    /// The state the browser keeps until it comes back.
    pub state: String,
}

/// What the provider sends the browser back with.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct Return {
    pub code: Option<String>,
    pub state: Option<String>,
    /// The provider's refusal, such as `access_denied`.
    pub error: Option<String>,
}

type Client = openidconnect::core::CoreClient<
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
            public_url: Some("https://helpdesk.example.com/".to_owned()),
        }
    }

    #[test]
    fn without_an_issuer_client_or_secret_there_is_no_provider() {
        let none = Settings {
            issuer: None,
            client_id: None,
            client_secret: None,
            ..settings()
        };
        assert!(Config::from_settings(none).unwrap().is_none());
    }

    #[test]
    fn a_configured_provider_redirects_back_to_the_public_url() {
        let config = Config::from_settings(settings()).unwrap().unwrap();
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
        let error = Config::from_settings(Settings {
            client_id: None,
            ..settings()
        })
        .unwrap_err();
        assert!(format!("{error}").contains("HD_OIDC_CLIENT_ID"), "{error}");
        let error = Config::from_settings(Settings {
            public_url: None,
            ..settings()
        })
        .unwrap_err();
        assert!(format!("{error}").contains("HD_PUBLIC_URL"), "{error}");
    }

    #[test]
    fn plain_http_is_accepted_only_on_this_machine() {
        assert!(
            Config::from_settings(Settings {
                issuer: Some("http://login.example.com/realms/staff".to_owned()),
                ..settings()
            })
            .is_err()
        );
        assert!(
            Config::from_settings(Settings {
                public_url: Some("http://helpdesk.example.com".to_owned()),
                ..settings()
            })
            .is_err()
        );
        let local = Settings {
            issuer: Some("http://127.0.0.1:55080/realms/helpdesk".to_owned()),
            public_url: Some("http://localhost:5173".to_owned()),
            ..settings()
        };
        assert!(Config::from_settings(local).unwrap().is_some());
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
        let config = Config::from_settings(from_file).unwrap().unwrap();
        assert_eq!(config.client_secret.secret(), "secret-from-a-file");
        let both = Settings {
            client_secret_file: Some(file),
            ..settings()
        };
        assert!(Config::from_settings(both).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_session_lasts_between_one_hour_and_a_day() {
        for hours in [0, 25] {
            assert!(
                Config::from_settings(Settings {
                    session_hours: hours,
                    ..settings()
                })
                .is_err()
            );
        }
        let config = Config::from_settings(Settings {
            session_hours: 8,
            label: Some(" Northgate ".to_owned()),
            ..settings()
        })
        .unwrap()
        .unwrap();
        assert_eq!(config.session, time::Duration::hours(8));
        assert_eq!(config.label, "Northgate");
    }
}
