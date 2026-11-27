use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use hd_addon_sdk::{Client, Verifier};
use hd_zabbix::check::{self, CheckArgs};
use hd_zabbix::config::Config;
use hd_zabbix::details::{self, Details};
use hd_zabbix::sync::Syncer;
use hd_zabbix::webhook::{self, Alerts};
use hd_zabbix::zabbix::Zabbix;
use tracing_subscriber::EnvFilter;

/// The Zabbix add-on: without a command, it brings the hosts of the host groups customers have
/// into the help desk's catalog, now and at each interval.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Tells which rule an alert meets and with what outcome, or that none does.
    Check(CheckArgs),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = Config::from_env()?;
    if let Some(Command::Check(args)) = cli.command {
        print!("{}", check::run(&config, args).await?);
        return Ok(());
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("HD_ZABBIX_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        // Colours only on a terminal: container logs stay plain text.
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .init();

    let zabbix = Zabbix::new(
        &config.zabbix_url,
        &config.zabbix_token,
        config.zabbix_timeout,
    )?;
    match zabbix.version().await {
        Ok(version) => tracing::info!(%version, url = %config.zabbix_url, "Zabbix answers"),
        Err(e) => tracing::warn!(error = %e, "Zabbix does not answer yet; trying every round"),
    }
    let sync = Syncer::new(
        Client::new(&config.core_url, &config.core_token)?,
        zabbix,
        config.system.clone(),
        config.kinds.clone(),
    );
    tracing::info!(
        system = %config.system,
        interval_seconds = config.interval.as_secs(),
        "reading Zabbix's hosts now and at each interval"
    );
    // The core's reads of live details are answered on their own, beside the rounds, with
    // calls to Zabbix short enough for the core's 2 seconds.
    let event_secret = config.event_secret.clone().context(
        "set HD_ZABBIX_EVENT_SECRET or HD_ZABBIX_EVENT_SECRET_FILE: the core signs its reads of \
         live details with the event secret `hd addon add` printed",
    )?;
    let details = Details::new(
        Zabbix::new(&config.zabbix_url, &config.zabbix_token, details::DEADLINE)?,
        &config.frontend_url,
    );
    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("listening on {}", config.listen))?;
    tracing::info!(listen = %config.listen, "answering the core's reads of live details");
    let verifier = Verifier::new(event_secret);
    let mut app = details::router(Arc::new(details), verifier.clone());
    let alerts = match &config.webhook_secret {
        Some(secret) => {
            let alerts = Arc::new(
                Alerts::new(
                    Client::new(&config.core_url, &config.core_token)?,
                    Zabbix::new(&config.zabbix_url, &config.zabbix_token, webhook::DEADLINE)?,
                    config.system.clone(),
                    config.rules.clone(),
                )
                .with_ui_url(&config.ui_url)
                .with_limit(config.safety_limit)
                .with_media_type(&config.webhook_media_type),
            );
            app = app
                .merge(webhook::router(alerts.clone(), secret.clone()))
                // What agents do to the tickets of problems goes back to Zabbix.
                .merge(webhook::back::router(alerts.clone(), verifier));
            tracing::info!(listen = %config.listen, path = webhook::PATH,
                tickets = config.safety_limit.tickets,
                minutes = config.safety_limit.minutes(),
                "receiving Zabbix's alerts through its webhook, and reconciling them after each \
                 round, under the safety limit");
            Some(alerts)
        }
        None => {
            app = app.merge(webhook::back::ignoring(verifier));
            tracing::warn!(
                "no [webhook] in the configuration: Zabbix's alerts are not received, and open \
                 no tickets"
            );
            None
        }
    };
    // It stops when told to, after the rounds: ending before then is a failure.
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let mut server = tokio::spawn(
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .into_future(),
    );
    let mut ticks = tokio::time::interval(config.interval);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            served = &mut server => {
                served
                    .context("the server of live details failed")?
                    .context("answering the core's reads of live details")?;
                anyhow::bail!("stopped answering the core's reads of live details");
            }
            _ = ticks.tick() => {
                match sync.run().await {
                    Ok(round) => tracing::info!(?round, "round done"),
                    Err(e) => tracing::error!(error = format!("{e:#}"), "round stopped; nothing was retired"),
                }
                reconcile(alerts.as_deref()).await;
            }
            // A problem found the wait for the safety ticket ended: the problems held back get
            // their tickets now.
            _ = wait_ended(alerts.as_deref()) => reconcile(alerts.as_deref()).await,
        }
    }
    let _ = stop.send(());
    server
        .await
        .context("the server of live details failed")?
        .context("answering the core's reads of live details")?;
    Ok(())
}

/// The reconciliation of Zabbix's alerts, when the add-on receives them.
async fn reconcile(alerts: Option<&Alerts>) {
    let Some(alerts) = alerts else {
        return;
    };
    match alerts.reconcile().await {
        Ok(reconciled) => tracing::info!(?reconciled, "alerts reconciled"),
        Err(e) => tracing::error!(
            error = format!("{e:#}"),
            "reconciliation stopped; nothing was opened or resolved since"
        ),
    }
}

/// Until the wait for a safety ticket ends through a problem of the webhook; never without one.
async fn wait_ended(alerts: Option<&Alerts>) {
    match alerts {
        Some(alerts) => alerts.hold.ended.notified().await,
        None => std::future::pending().await,
    }
}

/// Ctrl-C, or SIGTERM where there is one: a container's stop signal.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = terminate.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
