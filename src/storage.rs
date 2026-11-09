//! ClickHouse rows for `benchmark_send_rounds` and `benchmark_send_attempts`
//! (sql/migrations/00001_send_benchmark.sql). Attempts are written before their
//! round row: a round row with status `complete` is what makes them count.

use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use anyhow::{Context, Result};
use clickhouse::{Client, Compression, Row, RowOwned, RowWrite};
use serde::Serialize;
use time::OffsetDateTime;
use uuid::Uuid;

pub const ROUNDS: &str = "benchmark_send_rounds";
pub const ATTEMPTS: &str = "benchmark_send_attempts";

#[derive(Row, Serialize, Debug, Clone)]
pub struct RoundRow {
    #[serde(with = "clickhouse::serde::uuid")]
    pub round_id: Uuid,
    pub region: String,
    pub cloud_provider: String,
    #[serde(with = "clickhouse::serde::time::datetime64::nanos")]
    pub started_utc: OffsetDateTime,
    pub status: String,
    pub reason: String,
    pub sent_slot: Option<u64>,
    pub paths: u16,
    pub cu_limit: u32,
    pub cu_price_micro_lamports: u64,
    pub lock_account: String,
    pub sender_version: String,
}

#[derive(Row, Serialize, Debug, Clone, PartialEq)]
pub struct AttemptRow {
    #[serde(with = "clickhouse::serde::uuid")]
    pub round_id: Uuid,
    pub region: String,
    pub cloud_provider: String,
    pub provider: String,
    pub method: String,
    pub signature: String,
    #[serde(with = "clickhouse::serde::time::datetime64::nanos")]
    pub started_utc: OffsetDateTime,
    #[serde(with = "clickhouse::serde::time::datetime64::nanos")]
    pub sent_utc: OffsetDateTime,
    pub response_ns: Option<u64>,
    pub send_status: String,
    pub send_error: String,
    pub outcome: String,
    pub landed_slot: Option<u64>,
    pub tx_index: Option<u32>,
    pub block_tx_count: Option<u32>,
    pub slot_delta: Option<i64>,
    pub rank: Option<u16>,
    pub slots_behind_first: Option<u64>,
    pub fee_lamports: u64,
    pub tip_lamports: u64,
    pub chain_error: String,
}

pub fn client(config: &crate::config::ClickHouse) -> Result<Client> {
    Ok(Client::default()
        .with_url(&config.url)
        .with_database(&config.database)
        .with_user(crate::config::env(&config.user_env, "ClickHouse user")?)
        .with_password(crate::config::env(
            &config.password_env,
            "ClickHouse password",
        )?))
}

/// Check both tables and their columns before any money is spent.
pub async fn validate_schema(client: &Client) -> Result<()> {
    let client = client
        .clone()
        .with_compression(Compression::None)
        .with_validation(true);
    tokio::time::timeout(Duration::from_secs(30), async {
        check::<RoundRow>(&client, ROUNDS).await?;
        check::<AttemptRow>(&client, ATTEMPTS).await
    })
    .await
    .context("ClickHouse schema check timed out")?
}

async fn check<T: RowOwned + RowWrite>(client: &Client, table: &str) -> Result<()> {
    // Preparing an insert runs DESCRIBE and checks the row's columns; nothing is written.
    let insert = client.insert::<T>(table).await.with_context(|| {
        format!(
            "ClickHouse schema check failed for {table}; apply the Goose migrations \
             (sql/README.md) and grant INSERT and DESCRIBE on it"
        )
    })?;
    drop(insert);
    Ok(())
}

async fn insert<T: RowOwned + RowWrite>(client: &Client, table: &str, rows: &[T]) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut insert = client
        .insert::<T>(table)
        .await?
        .with_timeouts(Some(Duration::from_secs(10)), Some(Duration::from_secs(30)));
    for row in rows {
        insert.write(row).await?;
    }
    insert.end().await?;
    Ok(())
}

/// Write a round: attempts first, then the round row. Retries with backoff for
/// about ten minutes; identical retried rows are replaced on merge (FINAL).
/// `healthy` follows the latest attempt, so new rounds can wait while writes fail.
pub async fn write_round(
    client: &Client,
    round: &RoundRow,
    attempts: &[AttemptRow],
    healthy: &AtomicBool,
) -> Result<()> {
    let mut delay = Duration::from_secs(1);
    let mut last = None;
    for _ in 0..10 {
        let result = async {
            insert(client, ATTEMPTS, attempts).await?;
            insert(client, ROUNDS, std::slice::from_ref(round)).await
        }
        .await;
        match result {
            Ok(()) => {
                healthy.store(true, Ordering::Relaxed);
                return Ok(());
            }
            Err(error) => {
                healthy.store(false, Ordering::Relaxed);
                tracing::warn!(round = %round.round_id, %error, "ClickHouse write failed; retrying");
                last = Some(error);
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(120));
            }
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("no attempts made")))
        .with_context(|| format!("writing round {} to ClickHouse", round.round_id))
}
