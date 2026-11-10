//! Send-path benchmark. Each round sends one transaction per configured path at the
//! same instant, all signed by one fee payer, and records where each copy landed.
//! See docs/methodology.md.

mod budget;
mod config;
mod round;
mod rpc;
mod storage;
mod tx;

use std::{path::PathBuf, str::FromStr, sync::Arc, time::Duration};

use anyhow::{bail, Context, Result};
use clap::Parser;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    config: PathBuf,
    /// Check the configuration, keypair, endpoints and ClickHouse schema, print the
    /// worst-case daily spend, and exit without sending anything.
    #[arg(long)]
    check: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    match dotenvy::from_path(".env") {
        Ok(()) => {}
        Err(dotenvy::Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {}
        // Parse errors can quote credentials; do not include them.
        Err(_) => bail!("failed to load .env; check file access and syntax"),
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();
    let config = config::Config::from_file(&args.config)?;
    let ctx = Arc::new(context(config)?);
    storage::validate_schema(&ctx.clickhouse).await?;
    if ctx.config.rounds.settle_timeout < Duration::from_secs(80) {
        tracing::warn!(
            "rounds.settle_timeout is shorter than a blockhash's lifetime (about 60-90s): \
             a copy that lands late will be recorded as dropped"
        );
    }

    let per_round = ctx.config.worst_round_lamports();
    let per_day = 86_400 / ctx.config.rounds.interval.as_secs().max(1) * per_round;
    tracing::info!(
        payer = %ctx.payer.pubkey(),
        region = %ctx.config.location.region,
        paths = ctx.paths.len(),
        worst_round_lamports = per_round,
        worst_day_lamports = per_day.min(ctx.config.budget.daily_lamports),
        daily_budget_lamports = ctx.config.budget.daily_lamports,
        "configured"
    );
    if args.check {
        let balance = ctx.reference.balance(&ctx.payer.pubkey()).await?;
        ctx.reference.latest_blockhash().await?;
        let spent = ctx
            .ledger
            .lock()
            .expect("ledger lock")
            .spent(&budget::utc_day(time::OffsetDateTime::now_utc()));
        println!(
            "ok: {} paths, worst case {per_round} lamports a round, at most {} a day, \
             {spent} spent or reserved today, payer {} holds {balance}",
            ctx.paths.len(),
            per_day.min(ctx.config.budget.daily_lamports),
            ctx.payer.pubkey()
        );
        return Ok(());
    }
    run(ctx).await
}

fn context(config: config::Config) -> Result<round::Ctx> {
    let payer = tx::parse_keypair(&config::env(
        &config.wallet.keypair_env,
        "fee payer keypair",
    )?)?;
    let reference = rpc::Reference::new(
        config::env_url(&config.reference_rpc.url_env, "reference RPC URL")?,
        config.reference_rpc.request_timeout,
    )?;
    let paths = config
        .paths
        .iter()
        .map(|p| {
            let tip = p
                .tip
                .as_ref()
                .map(|t| {
                    let accounts = t
                        .accounts
                        .iter()
                        .map(|a| Pubkey::from_str(a))
                        .collect::<Result<Vec<_>, _>>()
                        .context("tip account")?;
                    anyhow::Ok((accounts, t.lamports))
                })
                .transpose()?;
            Ok(round::PathEntry {
                path: rpc::Path::new(p, config.rounds.send_timeout)?,
                tip,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let lock_account = config
        .rounds
        .lock_account
        .as_deref()
        .map(Pubkey::from_str)
        .transpose()
        .context("lock_account")?;
    let ledger = budget::Ledger::open(&config.state_dir, config.budget.daily_lamports)?;
    Ok(round::Ctx {
        clickhouse: storage::client(&config.clickhouse)?,
        payer,
        reference,
        paths,
        ledger: std::sync::Mutex::new(ledger),
        lock_account,
        storage_healthy: std::sync::atomic::AtomicBool::new(true),
        config,
    })
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("installing SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = ctrl_c.await;
}

async fn run(ctx: Arc<round::Ctx>) -> Result<()> {
    let mut settling = tokio::task::JoinSet::new();
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);
    loop {
        let delay = round::next_delay(ctx.config.rounds.interval, ctx.config.rounds.jitter);
        tokio::select! {
            _ = &mut shutdown => break,
            _ = tokio::time::sleep(delay) => {}
        }
        while settling.try_join_next().is_some() {}
        if let Some(task) = round::start(ctx.clone()).await {
            settling.spawn(async move {
                if let Err(error) = task.await {
                    tracing::error!(%error, "settle task failed");
                }
            });
        }
    }
    // Rounds already sent still cost money: let them settle and be recorded.
    let grace = ctx.config.rounds.settle_timeout + Duration::from_secs(60);
    tracing::info!(
        pending = settling.len(),
        "stopping; settling rounds in flight"
    );
    if tokio::time::timeout(grace, async {
        while settling.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        tracing::error!("rounds still settling at shutdown were not recorded");
        settling.abort_all();
    }
    Ok(())
}
