use hd_addon_sdk::Client;
use hd_zabbix::config::Config;
use hd_zabbix::sync::Sync;
use hd_zabbix::zabbix::Zabbix;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::from_env()?;
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
    let sync = Sync {
        core: Client::new(&config.core_url, &config.core_token)?,
        zabbix,
        system: config.system.clone(),
        kinds: config.kinds.clone(),
    };
    tracing::info!(
        system = %config.system,
        interval_seconds = config.interval.as_secs(),
        "reading Zabbix's hosts now and at each interval"
    );
    let mut ticks = tokio::time::interval(config.interval);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = ticks.tick() => match sync.run().await {
                Ok(round) => tracing::info!(?round, "round done"),
                Err(e) => tracing::error!(error = format!("{e:#}"), "round stopped; nothing was retired"),
            },
        }
    }
    Ok(())
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
