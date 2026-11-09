# Methodology

The benchmark ranks transaction send paths (RPC `sendTransaction`, landing services, Jito bundles) by where their transactions land in the block. Unlike the read-only [RPC](https://github.com/solana-foundation/rpc-latency-monitor) and [gRPC](https://github.com/solana-foundation/grpc-latency-monitor) monitors, it holds a funded keypair and spends SOL on every round.

## Method

Each round:

1. Warms every path's connection, so no copy pays for a TLS handshake the others skipped.
2. Reads a confirmed blockhash and the processed slot from the reference RPC.
3. Builds one transaction per path: compute budget, a self-transfer, and the path's tip when it has one. Every copy uses the same blockhash, compute unit limit and priority fee, and the same fee payer.
4. Sends all copies at once, in a shuffled order so no path is always started first. `sendTransaction` goes out with `skipPreflight: true` and `maxRetries: 0` unless the path overrides them, so the path's own routing is measured, not a simulation or an RPC rebroadcast.
5. Polls `getSignatureStatuses` on the reference RPC until every copy is confirmed, or the blockhash has expired, or `settle_timeout` passes.
6. Reads each landing slot's transaction order with `getBlock` (`transactionDetails: signatures`) and ranks the copies by `(slot, index)`.

The fee payer is writable in every copy, so the copies of a round contend for one write lock and the leader orders them one after another. Their order in the block is the race. Nobody else writes that account, so there is no local fee market for it, the fee can stay fixed and low, and the benchmark puts no load on accounts other people use.

All outcomes come from the reference RPC, which must not be one of the paths.

## What it measures, and what it does not

It measures which path gets a transaction into the leader's block first, how often each path lands, how often its copy slips to a later slot than the round's first copy, and how fast each path answers.

It does not measure:

- **Contention with other writers.** Every copy in a round is ours. A backrun on a busy pool queues behind other bots that write the same accounts, and nothing here reproduces that queue.
- **Time to land in milliseconds.** Results are slots and block positions. Response time is the send request alone.
- **Large transactions.** Probes are 230 to 290 bytes and request 1,000 compute units.
- **Leader coverage.** Bundles only land in slots led by validators running a block engine client. A bundle path's landing rate partly reflects that client's share of leader slots.

## Metrics

From `benchmark_send_attempts`, counting only rounds whose `benchmark_send_rounds` row has status `complete` (both tables read with `FINAL`):

| Metric | Definition |
|---|---|
| Landed | `outcome = 'landed'`: included and succeeded |
| Failed | `outcome = 'failed'`: included with an error; the fee is paid, a tip in the transaction is not |
| Dropped | `outcome = 'dropped'`: never included before the blockhash expired |
| First on chain | `rank = 1`: the round's earliest copy by `(landed_slot, tx_index)` |
| Head to head | for paths A and B, rounds where both were included and A's `(landed_slot, tx_index)` is lower |
| Later slot | `slots_behind_first > 0`: included after the slot of the round's first copy |
| Slots to land | `slot_delta`: landing slot minus the reference's processed slot at send |
| Block index | `tx_index`, counting vote transactions; compare paths within a region and period |
| Cost | `fee_lamports + tip_lamports`, what was actually paid |

About 800 rounds tell a 55/45 head-to-head split apart from an even one. At one round a minute, publish rankings per region per day, not per hour.

## Cost and budget

Cost per day = rounds a day × (copies × (5,000 + priority fee) + tips of copies that succeed). A dropped copy costs nothing; a failed copy pays its fee but not its tip.

With the example configuration (4 paths, 2,000 lamports of priority, 5,000- and 20,000-lamport tips, one round a minute) the worst case is 53,000 lamports a round, about 0.076 SOL a day per region.

Controls:

- **Daily budget.** A round starts only if its worst case still fits in `budget.daily_lamports` for the UTC day. The ledger (`state_dir/spend.json`) reserves the worst case before sending and settles to what was paid. Both steps are written to disk, so a crash leaves the worst case counted. Keep `state_dir` across restarts.
- **Minimum balance.** Rounds stop while the fee payer holds less than `budget.min_balance_lamports`.
- **Ceilings in code.** Configuration is rejected above 10,000,000 lamports of tip or 1,000,000 lamports of priority per copy. Raising either is a code change.
- **Wallet balance.** Fund the fee payer with about a week of budget at a time. Its balance is the hard ceiling whatever the software does.

- **ClickHouse outages.** Once a write fails, new rounds are skipped (`skipped_error`, "ClickHouse writes are failing") until a write succeeds again, so the sender does not keep spending on rows it cannot record. Rounds already sent keep retrying their writes for about ten minutes. Rounds sent before the first failure is noticed (up to `settle_timeout` plus 30 seconds) are written once ClickHouse is back.

Skipped rounds are recorded with status `skipped_budget`, `skipped_balance` or `skipped_error`, so a gap in the data reads as a skipped round, not an outage.

## Tips

Tip each path at the lowest level that actually lands, and record it in the configuration. Jito bundles compete in an auction: a tip below the current tip floor is accepted and then never included. Check the floor before setting `tip.lamports`, and expect to revise it.

Tip accounts in the example match the signed registry in [`solana-foundation/rpc-latency-monitor`](https://github.com/solana-foundation/rpc-latency-monitor/blob/main/sender/tip-accounts.json). Update them from there when a provider rotates.

## Known limits

- **The fee payer is public.** A path can recognize the benchmark's transactions and treat them specially. Nothing here prevents that. Rotating payers would make it harder; if paths sign with different payers, set `rounds.lock_account` so the copies still contend for one lock.
- **One vantage point per instance.** Rankings depend on where the sender runs relative to each path's endpoint. Run one instance per region and compare within a region.
- **`settle_timeout` must outlast a blockhash** (about 60 to 90 seconds). Shorter values record late landings as dropped; the sender warns at startup.

## Operation

```sh
cp config.example.yaml config.local.yaml    # paths, budget, region
# Environment: SEND_BENCH_KEYPAIR, BENCH_REFERENCE_RPC_URL, CLICKHOUSE_SEND_USER,
# CLICKHOUSE_SEND_PASSWORD and each path's url_env (see .env.example).
goose -table send_benchmark_goose_db_version up   # the two send tables (sql/README.md)
cargo run --release -- --config config.local.yaml --check
cargo run --release -- --config config.local.yaml
```

`--check` validates the configuration, keypair, reference RPC and ClickHouse schema, prints the worst case per round and per day, and exits without sending.

Give the sender its own ClickHouse user with INSERT and DESCRIBE on `benchmark_send_rounds` and `benchmark_send_attempts` only. Dashboards need SELECT on both.

On SIGINT or SIGTERM the sender stops starting rounds, lets rounds already sent settle, and records them, for up to `settle_timeout` plus 60 seconds.

## Example query

Head to head over the last day in one region:

```sql
WITH complete AS (
    SELECT round_id FROM benchmark_send_rounds FINAL
    WHERE region = 'fra' AND status = 'complete' AND started_utc >= now() - INTERVAL 1 DAY
),
included AS (
    SELECT round_id, provider, landed_slot, tx_index FROM benchmark_send_attempts FINAL
    WHERE region = 'fra' AND started_utc >= now() - INTERVAL 1 DAY
      AND round_id IN (SELECT round_id FROM complete)
      AND outcome != 'dropped' AND tx_index IS NOT NULL
)
SELECT a.provider, b.provider AS other,
       countIf((a.landed_slot, a.tx_index) < (b.landed_slot, b.tx_index)) AS ahead,
       count() AS both_included
FROM included AS a INNER JOIN included AS b USING (round_id)
WHERE a.provider != b.provider
GROUP BY a.provider, other
ORDER BY a.provider, other;
```
