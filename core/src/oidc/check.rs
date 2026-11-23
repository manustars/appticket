//! `hd sign-in check`: whether the configured identity provider answers and accepts the client,
//! before any agent tries to sign in.
//!
//! The client is checked with a code the provider never issued, sent with the client's id and
//! secret exactly as a sign-in sends them: a provider authenticates the client before it looks at
//! the code, so it answers `invalid_grant` to a client it accepts, and `invalid_client` (or, as
//! Keycloak does for a wrong secret, `unauthorized_client`) to one it does not. The client
//! credentials grant would tell the same only for a client allowed to use it, which a sign-in
//! client usually is not.

use openidconnect::core::CoreErrorResponseType;
use openidconnect::{AuthorizationCode, DiscoveryError, RequestTokenError};

use super::Provider;

/// What the check found: the provider answers and accepts the client.
#[derive(Debug)]
pub struct Checked {
    /// The issuer, as the provider's discovery document names it.
    pub issuer: String,
    /// Where the core exchanges a sign-in's code.
    pub token_endpoint: String,
}

/// What keeps agents from signing in through the configured identity provider. None of them
/// holds the client secret.
#[derive(Debug)]
pub enum CheckFailure {
    /// Nothing answered at `url`: a wrong host or port, the provider down, a firewall, or a
    /// certificate the core does not trust. `cause` says which.
    Unreachable { url: String, cause: String },
    /// Something answered at the issuer, but not with an OpenID Connect discovery document and
    /// keys: most often HD_OIDC_ISSUER names no issuer of the provider.
    NoDiscovery { issuer: String, answer: String },
    /// The discovery document names another issuer than HD_OIDC_ISSUER, which must be the
    /// provider's own, character for character.
    WrongIssuer { issuer: String, answer: String },
    /// The provider's configuration lacks what a sign-in needs, such as a token endpoint.
    Unusable { issuer: String, answer: String },
    /// The token endpoint refused the client id or its secret.
    ClientRejected {
        client_id: String,
        /// The provider's answer: its error code, and its description when it gives one.
        answer: String,
    },
    /// The token endpoint answered in a way that says neither that it accepts the client nor
    /// that it refuses it.
    Inconclusive { client_id: String, answer: String },
}

impl std::fmt::Display for CheckFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CheckFailure::Unreachable { url, cause } => write!(
                f,
                "the identity provider cannot be reached at {url} ({cause}): check \
                 HD_OIDC_ISSUER, and that the core can reach the provider and trusts its \
                 certificate"
            ),
            CheckFailure::NoDiscovery { issuer, answer } => write!(
                f,
                "no OpenID Connect configuration at the issuer {issuer} ({answer}): check \
                 HD_OIDC_ISSUER"
            ),
            CheckFailure::WrongIssuer { issuer, answer } => write!(
                f,
                "wrong issuer: the provider at {issuer} names another issuer ({answer}): \
                 HD_OIDC_ISSUER must be the issuer exactly as the provider names it, trailing \
                 slash included"
            ),
            CheckFailure::Unusable { issuer, answer } => write!(
                f,
                "the identity provider at {issuer} cannot be used for signing in ({answer})"
            ),
            CheckFailure::ClientRejected { client_id, answer } => write!(
                f,
                "the identity provider rejected the client {client_id} ({answer}): check \
                 HD_OIDC_CLIENT_ID and the client secret, and that the client may use the \
                 authorization code flow"
            ),
            CheckFailure::Inconclusive { client_id, answer } => write!(
                f,
                "the identity provider's answer does not say whether it accepts the client \
                 {client_id} ({answer}): try signing in through it"
            ),
        }
    }
}

impl std::error::Error for CheckFailure {}

impl Provider {
    /// Reads the provider's configuration and keys as a sign-in does, then asks its token
    /// endpoint to exchange a code it never issued, to learn whether it accepts the client.
    pub async fn check(&self) -> Result<Checked, CheckFailure> {
        let issuer = self.config.issuer.as_str().to_owned();
        let client = match self.client().await {
            Ok(client) => client,
            Err(error) => return Err(discovery_failure(issuer, &error)),
        };
        let Some(token_endpoint) = client.token_uri().map(|url| url.as_str().to_owned()) else {
            return Err(CheckFailure::Unusable {
                issuer,
                answer: "its discovery document names no token endpoint".to_owned(),
            });
        };
        // The library keeps the provider's error but not the HTTP status; 401 also says that the
        // client was refused when the body says nothing the library understands.
        let status = std::sync::Mutex::new(None);
        let http = |request| {
            let status = &status;
            async move {
                let response = super::send(self.http.clone(), request).await;
                if let Ok(response) = &response {
                    *status.lock().expect("not poisoned") = Some(response.status());
                }
                response
            }
        };
        let never_issued =
            AuthorizationCode::new(format!("hd-sign-in-check-{}", uuid::Uuid::new_v4()));
        let request = match client.exchange_code(never_issued) {
            Ok(request) => request,
            Err(error) => {
                return Err(CheckFailure::Unusable {
                    issuer,
                    answer: chain(&error),
                });
            }
        };
        let answer = request.request_async(&http).await;
        let unauthorized =
            *status.lock().expect("not poisoned") == Some(reqwest::StatusCode::UNAUTHORIZED);
        let client_id = self.config.client_id.as_str().to_owned();
        let accepted = Checked {
            issuer,
            token_endpoint: token_endpoint.clone(),
        };
        match answer {
            // Tokens for a code it never issued would be odd, but the client was accepted.
            Ok(_) => Ok(accepted),
            Err(RequestTokenError::ServerResponse(response)) => {
                let mut said = response.error().as_ref().to_owned();
                if let Some(description) = response.error_description() {
                    said = format!("{said}: {description}");
                }
                match response.error() {
                    CoreErrorResponseType::InvalidGrant if !unauthorized => Ok(accepted),
                    CoreErrorResponseType::InvalidClient
                    | CoreErrorResponseType::UnauthorizedClient => {
                        Err(CheckFailure::ClientRejected {
                            client_id,
                            answer: said,
                        })
                    }
                    _ if unauthorized => Err(CheckFailure::ClientRejected {
                        client_id,
                        answer: format!("401 Unauthorized, {said}"),
                    }),
                    _ => Err(CheckFailure::Inconclusive {
                        client_id,
                        answer: said,
                    }),
                }
            }
            Err(other) if unauthorized => Err(CheckFailure::ClientRejected {
                client_id,
                answer: format!("401 Unauthorized, {}", chain(&other)),
            }),
            Err(RequestTokenError::Request(error)) => Err(CheckFailure::Unreachable {
                url: token_endpoint,
                cause: chain(&error),
            }),
            Err(other) => Err(CheckFailure::Inconclusive {
                client_id,
                answer: chain(&other),
            }),
        }
    }
}

/// Why reading the provider's configuration and keys, as a sign-in does, failed.
fn discovery_failure(issuer: String, error: &anyhow::Error) -> CheckFailure {
    match error.downcast_ref::<DiscoveryError<reqwest::Error>>() {
        Some(DiscoveryError::Request(error)) => CheckFailure::Unreachable {
            url: issuer,
            cause: chain(error),
        },
        // The document, or the keys it points to, answered with another status, or not as JSON.
        Some(DiscoveryError::Response(_, _, answer)) => CheckFailure::NoDiscovery {
            issuer,
            answer: answer.clone(),
        },
        Some(error @ DiscoveryError::Parse(_)) => CheckFailure::NoDiscovery {
            issuer,
            answer: chain(error),
        },
        // The only check discovery makes of the document: that it names the issuer asked for.
        Some(DiscoveryError::Validation(answer)) => CheckFailure::WrongIssuer {
            issuer,
            answer: answer.clone(),
        },
        _ => CheckFailure::Unusable {
            issuer,
            answer: format!("{error:#}"),
        },
    }
}

/// An error and the errors that caused it, as one line: the reason a request failed is often
/// only in the last of them.
fn chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut said = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        said = format!("{said}: {cause}");
        source = cause.source();
    }
    said
}
#[cfg(test)]
mod tests {
    //! Against the development Keycloak (`dev/keycloak`, started by compose.yaml and in CI), but
    //! for an identity provider that cannot be reached.

    use super::*;
    use crate::oidc::{Config, Settings};

    const ISSUER: &str = "http://127.0.0.1:55080/realms/helpdesk";
    const SECRET: &str = "dev-only-client-secret";

    fn provider(issuer: &str, client_id: &str, secret: &str) -> std::sync::Arc<Provider> {
        let settings = Settings {
            issuer: Some(issuer.to_owned()),
            client_id: Some(client_id.to_owned()),
            client_secret: Some(secret.to_owned()),
            client_secret_file: None,
            label: None,
            session_hours: 12,
            authorized_group: None,
            group_claim: "groups".to_owned(),
        };
        let config = Config::from_settings(settings, Some("http://127.0.0.1:8080"))
            .unwrap()
            .unwrap();
        Provider::new(config).unwrap()
    }

    #[tokio::test]
    #[ignore = "needs the development Keycloak (podman compose up -d keycloak)"]
    async fn keycloak_answers_and_accepts_the_client() {
        let checked = provider(ISSUER, "hd-core", SECRET).check().await.unwrap();
        assert_eq!(checked.issuer, ISSUER);
        assert_eq!(
            checked.token_endpoint,
            "http://127.0.0.1:55080/realms/helpdesk/protocol/openid-connect/token"
        );
    }

    #[tokio::test]
    async fn a_provider_nothing_answers_for_is_unreachable() {
        // A port nothing listens on any more.
        let closed = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let issuer = format!("http://127.0.0.1:{closed}/realms/helpdesk");
        let failure = provider(&issuer, "hd-core", SECRET)
            .check()
            .await
            .unwrap_err();
        assert!(
            matches!(failure, CheckFailure::Unreachable { .. }),
            "{failure:?}"
        );
        let said = failure.to_string();
        assert!(said.contains(&issuer), "{said}");
        // The reason is the last of the causes.
        assert!(said.to_lowercase().contains("connect"), "{said}");
        assert!(!format!("{said} {failure:?}").contains(SECRET));
    }

    #[tokio::test]
    #[ignore = "needs the development Keycloak (podman compose up -d keycloak)"]
    async fn an_issuer_keycloak_names_otherwise_is_a_wrong_issuer() {
        // Keycloak names its issuer without the trailing slash, and with the host it is
        // configured with, whatever host it is asked at.
        for issuer in [
            "http://127.0.0.1:55080/realms/helpdesk/",
            "http://localhost:55080/realms/helpdesk",
        ] {
            let failure = provider(issuer, "hd-core", SECRET)
                .check()
                .await
                .unwrap_err();
            assert!(
                matches!(failure, CheckFailure::WrongIssuer { .. }),
                "{failure:?}"
            );
            let said = failure.to_string();
            assert!(said.contains(issuer), "{said}");
            assert!(
                said.contains(ISSUER),
                "the issuer the provider names: {said}"
            );
        }
    }

    #[tokio::test]
    #[ignore = "needs the development Keycloak (podman compose up -d keycloak)"]
    async fn an_issuer_keycloak_has_no_configuration_for_is_no_discovery() {
        let issuer = "http://127.0.0.1:55080/realms/nowhere";
        let failure = provider(issuer, "hd-core", SECRET)
            .check()
            .await
            .unwrap_err();
        assert!(
            matches!(failure, CheckFailure::NoDiscovery { .. }),
            "{failure:?}"
        );
        let said = failure.to_string();
        assert!(said.contains("404"), "{said}");
        assert!(said.contains(issuer), "{said}");
    }

    #[tokio::test]
    #[ignore = "needs the development Keycloak (podman compose up -d keycloak)"]
    async fn keycloak_rejects_a_wrong_secret_or_an_unknown_client_without_showing_the_secret() {
        // Keycloak answers unauthorized_client to a wrong secret, invalid_client to a client it
        // does not know.
        for (client_id, secret) in [
            ("hd-core", "invented-wrong-secret"),
            ("hd-nobody", "invented-wrong-secret"),
        ] {
            let failure = provider(ISSUER, client_id, secret)
                .check()
                .await
                .unwrap_err();
            assert!(
                matches!(failure, CheckFailure::ClientRejected { .. }),
                "{failure:?}"
            );
            let said = format!("{failure} {failure:?}");
            assert!(said.contains(client_id), "{said}");
            assert!(!said.contains(secret), "{said}");
        }
    }
}
