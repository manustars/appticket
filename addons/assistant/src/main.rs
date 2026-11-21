use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use hd_addon_sdk::{Client, Verifier};
use hd_assistant::config::{Config, Provider};
use hd_assistant::model::Model;
use hd_assistant::{Assistant, server};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::from_env()?;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("HD_ASSISTANT_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let model = &config.model;
    if model.is_external() {
        let service = match model.provider {
            Provider::Anthropic => "the Claude API",
            Provider::OpenAiCompatible => "an external model server",
        };
        tracing::warn!(
            url = %model.url,
            "customers' mail is sent to {service} for the Assistant to read it; sending it \
             outside the help desk is a data-protection decision for the help desk to make \
             (set HD_ASSISTANT_MODEL_PROVIDER=openai-compatible and HD_ASSISTANT_MODEL_URL to \
             use a model on your own servers)"
        );
    }

    let core = Client::new(&config.core_url, &config.token)?;
    match core.whoami().await {
        Ok(me) => tracing::info!(name = %me.name, scopes = ?me.scopes, "connected to the core"),
        Err(e) => tracing::warn!(error = %e, "the core does not answer yet; events will wait"),
    }
    let assistant = Arc::new(Assistant::new(core, Model::new(config.model.clone())?));

    let (sender, receiver) = server::queue();
    let worker = tokio::spawn(server::work(
        assistant,
        receiver,
        vec![
            Duration::from_secs(30),
            Duration::from_secs(120),
            Duration::from_secs(600),
        ],
    ));
    let app = server::events(Verifier::new(config.event_secret.clone()), sender);
    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("listening on {}", config.listen))?;
    tracing::info!(
        listen = %config.listen,
        provider = ?model.provider,
        model = %model.model,
        "the Assistant is waiting for events"
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    worker.abort();
    Ok(())
}
