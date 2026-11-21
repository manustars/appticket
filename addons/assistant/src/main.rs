use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use hd_addon_sdk::{Client, Verifier};
use hd_assistant::config::Config;
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
        tracing::warn!(
            provider = %model.provider,
            url = %model.url,
            "customers' mail is sent to a model service outside this machine for the \
             Assistant to read it; sending it to a third party is a data-protection decision \
             for the help desk to make (HD_ASSISTANT_MODEL_PROVIDER=ollama, or \
             openai-compatible with a server on this machine, keeps it here)"
        );
    }

    let core = Client::new(&config.core_url, &config.token)?;
    let assistant = Arc::new(Assistant::new(core, Model::new(config.model.clone())?));
    // Rationales are written in the agents' languages, from `whoami`; they are read again every
    // few minutes, and before the first event if the core does not answer now.
    match assistant.refresh_languages().await {
        Ok(languages) => tracing::info!(
            ?languages,
            "connected to the core; rationales in these languages"
        ),
        Err(e) => tracing::warn!(error = %e, "the core does not answer yet; events will wait"),
    }

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
