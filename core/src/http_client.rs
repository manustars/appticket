//! The HTTP client the core calls other servers with: add-ons and the identity provider.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use reqwest::redirect::Policy;

/// A client that trusts the public web PKI, gives up after ten seconds and follows redirects as
/// `redirects` says.
pub fn http_client(redirects: Policy) -> anyhow::Result<reqwest::Client> {
    let roots = tokio_rustls::rustls::RootCertStore::from_iter(
        webpki_roots::TLS_SERVER_ROOTS.iter().cloned(),
    );
    let tls = tokio_rustls::rustls::ClientConfig::builder_with_provider(Arc::new(
        tokio_rustls::rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_root_certificates(roots)
    .with_no_client_auth();
    reqwest::Client::builder()
        .tls_backend_preconfigured(tls)
        .redirect(redirects)
        .timeout(Duration::from_secs(10))
        .build()
        .context("building the HTTP client")
}
