-- +goose NO TRANSACTION
-- +goose Up
-- Send-path benchmark. Each round sends one transaction per configured
-- path at the same instant, all signed by one fee payer, so the copies write-lock the
-- same account and race each other through the leader's scheduler. Rows are final
-- when written: the sender writes a round's attempts only after every copy has
-- landed or its blockhash has expired, then writes the round row last. A round is
-- complete only when its `benchmark_send_rounds` row has status 'complete';
-- attempts without one belong to a round that was interrupted and must be ignored.
-- Retries can insert identical rows, so both tables replace on their sorting key:
-- query them with FINAL. Rows are small (one per path per round), so both tables
-- keep 400 days and no minute rollups are needed.
CREATE TABLE benchmark_send_rounds
(
    round_id UUID,
    region LowCardinality(String),
    cloud_provider LowCardinality(String),
    started_utc DateTime64(9, 'UTC'),
    -- complete | skipped_budget | skipped_balance | skipped_error
    status LowCardinality(String),
    reason String CODEC(ZSTD(3)),
    -- The reference RPC's processed slot just before the copies were sent.
    sent_slot Nullable(UInt64),
    paths UInt16,
    cu_limit UInt32,
    cu_price_micro_lamports UInt64,
    -- An extra account every copy write-locks without using it; empty when unset.
    lock_account String,
    sender_version LowCardinality(String)
)
ENGINE = ReplacingMergeTree
PARTITION BY toYYYYMM(started_utc)
ORDER BY (region, cloud_provider, started_utc, round_id)
TTL toDateTime(started_utc) + INTERVAL 400 DAY DELETE;

CREATE TABLE benchmark_send_attempts
(
    round_id UUID,
    region LowCardinality(String),
    cloud_provider LowCardinality(String),
    -- The configured path name, shown as the provider on dashboards.
    provider LowCardinality(String),
    -- sendTransaction | sendBundle
    method LowCardinality(String),
    signature String,
    -- The round's start, repeated here so time filters need no join.
    started_utc DateTime64(9, 'UTC'),
    sent_utc DateTime64(9, 'UTC'),
    -- Request start to response, NULL when no response arrived.
    response_ns Nullable(UInt64),
    -- accepted | rpc_error | http_error | timeout | transport
    send_status LowCardinality(String),
    send_error String CODEC(ZSTD(3)),
    -- landed (included, succeeded) | failed (included, error) | dropped (never included)
    outcome LowCardinality(String),
    landed_slot Nullable(UInt64),
    -- Position in the block, from getBlock's signature order; NULL when dropped.
    tx_index Nullable(UInt32),
    block_tx_count Nullable(UInt32),
    -- landed_slot minus the round's sent_slot.
    slot_delta Nullable(Int64),
    -- 1 = this round's first copy on chain, ordered by (landed_slot, tx_index).
    rank Nullable(UInt16),
    slots_behind_first Nullable(UInt64),
    -- Lamports actually paid: fee for any included copy, tip only when it succeeded.
    fee_lamports UInt64,
    tip_lamports UInt64,
    chain_error String CODEC(ZSTD(3))
)
ENGINE = ReplacingMergeTree
PARTITION BY toYYYYMM(started_utc)
ORDER BY (region, cloud_provider, started_utc, round_id, provider)
TTL toDateTime(started_utc) + INTERVAL 400 DAY DELETE;

-- +goose Down
SELECT throwIf(1, 'Forward-only migration: create a corrective migration instead of rolling back');
