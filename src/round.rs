//! One round: reserve budget, warm connections, build one copy per path, send them
//! all at once, then settle every copy against the reference node and write rows.

use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use anyhow::Result;
use rand::{seq::SliceRandom, Rng};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_signer::Signer;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    budget::{self, Ledger, Reservation},
    config::{Config, MAX_PATHS},
    rpc::{Path, Reference, SendStatus, Status},
    storage::{self, AttemptRow, RoundRow},
    tx,
};

pub struct PathEntry {
    pub path: Path,
    pub tip: Option<(Vec<Pubkey>, u64)>,
}

pub struct Ctx {
    pub config: Config,
    pub payer: Keypair,
    pub reference: Reference,
    pub paths: Vec<PathEntry>,
    pub clickhouse: clickhouse::Client,
    pub ledger: Mutex<Ledger>,
    pub lock_account: Option<Pubkey>,
    /// False while the latest ClickHouse write failed: rounds wait rather than spend
    /// SOL on results that cannot be recorded.
    pub storage_healthy: AtomicBool,
}

/// How long block order is retried for a slot whose copy is already confirmed.
const BLOCK_RETRIES: usize = 10;
const BLOCK_RETRY_DELAY: Duration = Duration::from_millis(500);
/// How often the blockhash's expiry is checked while copies are pending.
const HEIGHT_CHECK: Duration = Duration::from_secs(2);

struct Sent {
    provider: String,
    method: &'static str,
    signature: Signature,
    sent_utc: OffsetDateTime,
    response: Option<Duration>,
    status: SendStatus,
    tip: u64,
}

fn now_utc() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}

impl Ctx {
    fn round_row(
        &self,
        round_id: Uuid,
        started: OffsetDateTime,
        status: &str,
        reason: String,
        sent_slot: Option<u64>,
    ) -> RoundRow {
        RoundRow {
            round_id,
            region: self.config.location.region.clone(),
            cloud_provider: self.config.location.cloud_provider.clone(),
            started_utc: started,
            status: status.into(),
            reason,
            sent_slot,
            paths: u16::try_from(self.paths.len()).unwrap_or(u16::MAX),
            cu_limit: self.config.rounds.cu_limit,
            cu_price_micro_lamports: self.config.cu_price_micro_lamports(),
            lock_account: self.lock_account.map(|k| k.to_string()).unwrap_or_default(),
            sender_version: env!("CARGO_PKG_VERSION").into(),
        }
    }

    /// Record a skipped round without holding up the round loop: the write retries
    /// in the background, and its success is what resumes rounds after an outage.
    fn skip(
        self: &Arc<Self>,
        round_id: Uuid,
        started: OffsetDateTime,
        status: &str,
        reason: String,
    ) {
        tracing::warn!(round = %round_id, status, %reason, "round skipped");
        let row = self.round_row(round_id, started, status, reason, None);
        let ctx = self.clone();
        tokio::spawn(async move {
            if let Err(error) =
                storage::write_round(&ctx.clickhouse, &row, &[], &ctx.storage_healthy).await
            {
                tracing::error!(%error, "could not record a skipped round");
            }
        });
    }

    fn reserve(&self, today: &str) -> Result<Option<Reservation>> {
        let worst = self.config.worst_round_lamports();
        self.ledger
            .lock()
            .expect("ledger lock")
            .reserve(today, worst)
    }

    fn settle_budget(&self, reservation: Reservation, paid: u64) {
        if let Err(error) = self
            .ledger
            .lock()
            .expect("ledger lock")
            .settle(reservation, paid)
        {
            tracing::error!(%error, "could not update the spend ledger; the worst case stays counted");
        }
    }
}

/// Start a round. Returns the task that settles and records it, or `None` if the
/// round was skipped (already recorded).
pub async fn start(ctx: Arc<Ctx>, seq: u64) -> Option<tokio::task::JoinHandle<()>> {
    let round_id = Uuid::now_v7();
    let started = now_utc();

    if !ctx.storage_healthy.load(Ordering::Relaxed) {
        // Recording this skip is also the probe: when it succeeds, rounds resume.
        ctx.skip(
            round_id,
            started,
            "skipped_error",
            "ClickHouse writes are failing".into(),
        );
        return None;
    }

    match ctx.reference.balance(&ctx.payer.pubkey()).await {
        Ok(balance) if balance < ctx.config.budget.min_balance_lamports => {
            let reason = format!(
                "fee payer holds {balance} lamports, below min_balance_lamports {}",
                ctx.config.budget.min_balance_lamports
            );
            ctx.skip(round_id, started, "skipped_balance", reason);
            return None;
        }
        Ok(_) => {}
        Err(error) => {
            ctx.skip(round_id, started, "skipped_error", error.to_string());
            return None;
        }
    }

    let reservation = match ctx.reserve(&budget::utc_day(started)) {
        Ok(Some(r)) => r,
        Ok(None) => {
            let reason = format!(
                "daily budget of {} lamports reached",
                ctx.config.budget.daily_lamports
            );
            ctx.skip(round_id, started, "skipped_budget", reason);
            return None;
        }
        Err(error) => {
            ctx.skip(
                round_id,
                started,
                "skipped_error",
                format!("ledger: {error:#}"),
            );
            return None;
        }
    };

    // Every path gets a live connection before any copy goes out.
    futures::future::join_all(ctx.paths.iter().map(|p| p.path.warm())).await;

    let (blockhash, sent_slot) = tokio::join!(
        ctx.reference.latest_blockhash(),
        ctx.reference.processed_slot()
    );
    let blockhash = match blockhash {
        Ok(b) => b,
        Err(error) => {
            ctx.settle_budget(reservation, 0);
            ctx.skip(round_id, started, "skipped_error", error.to_string());
            return None;
        }
    };
    let sent_slot = sent_slot
        .inspect_err(|error| tracing::warn!(%error, "no processed slot for this round"))
        .ok();

    let round_tx = tx::RoundTx {
        payer: &ctx.payer,
        blockhash: blockhash.hash.clone(),
        cu_limit: ctx.config.rounds.cu_limit,
        cu_price_micro_lamports: ctx.config.cu_price_micro_lamports(),
        lock_account: ctx.lock_account,
    };
    let mut prepared = Vec::with_capacity(ctx.paths.len());
    {
        let mut rng = rand::rng();
        for (i, entry) in ctx.paths.iter().enumerate() {
            let tip = entry.tip.as_ref().map(|(accounts, lamports)| {
                (accounts[rng.random_range(0..accounts.len())], *lamports)
            });
            // Distinct per path and per round, so no two copies share a signature
            // even if two rounds get the same blockhash.
            let copy = i + MAX_PATHS * usize::try_from(seq % 10_000).unwrap_or(0);
            match tx::build(&round_tx, copy, tip) {
                Ok(probe) => prepared.push((i, probe.signature, entry.path.body(&probe.wire), tip)),
                Err(error) => {
                    ctx.settle_budget(reservation, 0);
                    ctx.skip(round_id, started, "skipped_error", format!("{error:#}"));
                    return None;
                }
            }
        }
        // Shuffle the start order so no path is always polled first.
        prepared.shuffle(&mut rng);
    }

    let sends = prepared.into_iter().map(|(i, signature, body, tip)| {
        let ctx = &ctx;
        async move {
            let path = &ctx.paths[i].path;
            let sent_utc = now_utc();
            let (status, response) = path.send(&body).await;
            (
                i,
                Sent {
                    provider: path.name.clone(),
                    method: path.method.as_str(),
                    signature,
                    sent_utc,
                    response,
                    status,
                    tip: tip.map_or(0, |(_, l)| l),
                },
            )
        }
    });
    let mut sent = futures::future::join_all(sends).await;
    sent.sort_by_key(|(i, _)| *i);
    let sent: Vec<Sent> = sent.into_iter().map(|(_, s)| s).collect();
    for s in &sent {
        if s.status != SendStatus::Accepted {
            tracing::info!(round = %round_id, provider = %s.provider, status = s.status.label(), detail = %s.status.detail(), "send not accepted");
        }
    }

    let deadline = Instant::now() + ctx.config.rounds.settle_timeout;
    Some(tokio::spawn(async move {
        let (statuses, known) =
            watch(&ctx, &sent, blockhash.last_valid_block_height, deadline).await;
        let mut blocks = BTreeMap::new();
        for slot in statuses.iter().flatten().map(|s| s.slot) {
            if let std::collections::btree_map::Entry::Vacant(e) = blocks.entry(slot) {
                e.insert(block_order(&ctx.reference, slot).await);
            }
        }
        let rows = attempts(
            &ctx, round_id, started, sent_slot, &sent, &statuses, &blocks,
        );
        let paid: u64 = rows.iter().map(|r| r.fee_lamports + r.tip_lamports).sum();
        let (status, reason) = if known {
            ("complete", String::new())
        } else {
            // Outcomes unknown: keep the worst case counted and leave the round out.
            (
                "skipped_error",
                "reference RPC unavailable while settling".to_string(),
            )
        };
        ctx.settle_budget(reservation, if known { paid } else { u64::MAX });
        let round = ctx.round_row(round_id, started, status, reason, sent_slot);
        let landed = rows.iter().filter(|r| r.outcome != "dropped").count();
        tracing::info!(round = %round_id, status, landed, copies = rows.len(), paid, "round settled");
        if let Err(error) =
            storage::write_round(&ctx.clickhouse, &round, &rows, &ctx.storage_healthy).await
        {
            tracing::error!(%error, "round lost");
        }
    }))
}

/// Poll statuses until every copy is confirmed, the blockhash has expired, or the
/// deadline passes. The bool is false when the final statuses could not be read.
async fn watch(
    ctx: &Ctx,
    sent: &[Sent],
    last_valid_block_height: u64,
    deadline: Instant,
) -> (Vec<Option<Status>>, bool) {
    let mut found: Vec<Option<Status>> = vec![None; sent.len()];
    let mut expired = false;
    let mut last_height = Instant::now();
    loop {
        tokio::time::sleep(ctx.config.reference_rpc.status_poll).await;
        let pending: Vec<usize> = (0..sent.len())
            .filter(|&i| !found[i].as_ref().is_some_and(|s| s.confirmed))
            .collect();
        if pending.is_empty() {
            return (found, true);
        }
        let sigs: Vec<Signature> = pending.iter().map(|&i| sent[i].signature).collect();
        let read = match ctx.reference.statuses(&sigs).await {
            Ok(statuses) => {
                for (&i, status) in pending.iter().zip(statuses) {
                    if status.is_some() {
                        found[i] = status;
                    }
                }
                true
            }
            Err(error) => {
                tracing::warn!(%error, "getSignatureStatuses failed");
                false
            }
        };
        if expired || Instant::now() >= deadline {
            // Unconfirmed copies are dropped, but only if this last read succeeded.
            for status in found.iter_mut() {
                if status.as_ref().is_some_and(|s| !s.confirmed) {
                    *status = None;
                }
            }
            return (found, read);
        }
        if last_height.elapsed() >= HEIGHT_CHECK {
            last_height = Instant::now();
            match ctx.reference.block_height().await {
                // One more status read after expiry, then stop.
                Ok(height) if height > last_valid_block_height => expired = true,
                Ok(_) => {}
                Err(error) => tracing::warn!(%error, "getBlockHeight failed"),
            }
        }
    }
}

async fn block_order(reference: &Reference, slot: u64) -> Option<Vec<String>> {
    for _ in 0..BLOCK_RETRIES {
        match reference.block_signatures(slot).await {
            Ok(Some(sigs)) => return Some(sigs),
            Ok(None) => {}
            Err(error) => tracing::warn!(slot, %error, "getBlock failed"),
        }
        tokio::time::sleep(BLOCK_RETRY_DELAY).await;
    }
    tracing::warn!(slot, "block order unavailable; its copies have no index");
    None
}

fn attempts(
    ctx: &Ctx,
    round_id: Uuid,
    started: OffsetDateTime,
    sent_slot: Option<u64>,
    sent: &[Sent],
    statuses: &[Option<Status>],
    blocks: &BTreeMap<u64, Option<Vec<String>>>,
) -> Vec<AttemptRow> {
    let fee = ctx.config.fee_lamports();
    let placed: Vec<Placement> = sent
        .iter()
        .zip(statuses)
        .map(|(s, status)| {
            status.as_ref().map(|st| {
                let order = blocks.get(&st.slot).and_then(Option::as_ref);
                let sig = s.signature.to_string();
                Placed {
                    slot: st.slot,
                    index: order.and_then(|o| o.iter().position(|x| *x == sig)),
                    block_len: order.map(Vec::len),
                }
            })
        })
        .collect();
    let ranks = rank(&placed);
    sent.iter()
        .zip(statuses)
        .zip(placed.iter().zip(ranks))
        .map(|((s, status), (place, (rank, behind)))| {
            let outcome = match status {
                None => "dropped",
                Some(st) if st.err.is_none() => "landed",
                Some(_) => "failed",
            };
            AttemptRow {
                round_id,
                region: ctx.config.location.region.clone(),
                cloud_provider: ctx.config.location.cloud_provider.clone(),
                provider: s.provider.clone(),
                method: s.method.into(),
                signature: s.signature.to_string(),
                started_utc: started,
                sent_utc: s.sent_utc,
                response_ns: s
                    .response
                    .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)),
                send_status: s.status.label().into(),
                send_error: s.status.detail(),
                outcome: outcome.into(),
                landed_slot: place.as_ref().map(|p| p.slot),
                tx_index: place
                    .as_ref()
                    .and_then(|p| p.index)
                    .and_then(|i| u32::try_from(i).ok()),
                block_tx_count: place
                    .as_ref()
                    .and_then(|p| p.block_len)
                    .and_then(|n| u32::try_from(n).ok()),
                slot_delta: place.as_ref().zip(sent_slot).and_then(|(p, sent)| {
                    i64::try_from(p.slot)
                        .ok()
                        .zip(i64::try_from(sent).ok())
                        .map(|(a, b)| a - b)
                }),
                rank,
                slots_behind_first: behind,
                fee_lamports: if status.is_some() { fee } else { 0 },
                tip_lamports: if outcome == "landed" { s.tip } else { 0 },
                chain_error: status
                    .as_ref()
                    .and_then(|st| st.err.clone())
                    .unwrap_or_default(),
            }
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Placed {
    slot: u64,
    index: Option<usize>,
    block_len: Option<usize>,
}
type Placement = Option<Placed>;

/// Rank included copies by (slot, index): 1 is first on chain. A copy whose index
/// is unknown has no rank but still has its slot distance from the first copy.
fn rank(placed: &[Placement]) -> Vec<(Option<u16>, Option<u64>)> {
    let first_slot = placed.iter().flatten().map(|p| p.slot).min();
    let mut ordered: Vec<(u64, usize, usize)> = placed
        .iter()
        .enumerate()
        .filter_map(|(i, p)| p.as_ref().and_then(|p| p.index.map(|x| (p.slot, x, i))))
        .collect();
    ordered.sort_unstable();
    let mut ranks = vec![None; placed.len()];
    for (r, &(_, _, i)) in ordered.iter().enumerate() {
        ranks[i] = u16::try_from(r + 1).ok();
    }
    placed
        .iter()
        .zip(ranks)
        .map(|(p, r)| (r, p.as_ref().zip(first_slot).map(|(p, f)| p.slot - f)))
        .collect()
}

/// Wait before the next round: `interval` plus a uniform draw in `[-jitter, jitter]`.
pub fn next_delay(interval: Duration, jitter: Duration) -> Duration {
    if jitter.is_zero() {
        return interval;
    }
    let j = jitter.as_secs_f64();
    let offset = rand::rng().random_range(-j..=j);
    Duration::from_secs_f64((interval.as_secs_f64() + offset).max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(slot: u64, index: Option<usize>) -> Placement {
        Some(Placed {
            slot,
            index,
            block_len: Some(500),
        })
    }

    #[test]
    fn ranks_by_slot_then_index() {
        let placed = vec![
            at(10, Some(170)),
            at(10, Some(163)),
            None,
            at(11, Some(3)),
            at(10, None),
        ];
        let ranks = rank(&placed);
        assert_eq!(ranks[1], (Some(1), Some(0)));
        assert_eq!(ranks[0], (Some(2), Some(0)));
        assert_eq!(ranks[3], (Some(3), Some(1)));
        assert_eq!(ranks[2], (None, None));
        // Index unknown: no rank, but the slot distance is still known.
        assert_eq!(ranks[4], (None, Some(0)));
    }

    #[test]
    fn nothing_included_ranks_nothing() {
        assert_eq!(rank(&[None, None]), vec![(None, None), (None, None)]);
    }

    #[test]
    fn jitter_stays_in_bounds() {
        for _ in 0..1_000 {
            let d = next_delay(Duration::from_secs(60), Duration::from_secs(20));
            assert!(d >= Duration::from_secs(40) && d <= Duration::from_secs(80));
        }
        assert_eq!(
            next_delay(Duration::from_secs(5), Duration::ZERO),
            Duration::from_secs(5)
        );
    }
}
