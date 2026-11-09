# ClickHouse schema

[Goose](https://github.com/pressly/goose) owns schema changes; `migrations/` is the source of truth. The sender checks at startup that the tables and columns it writes exist. It never creates, migrates or drops tables.

## Tables

`benchmark_send_rounds` holds one row per round, and `benchmark_send_attempts` one row per copy, i.e. per path per round. Rows are final when written. The sender inserts a round's attempts only after every copy has landed or its blockhash has expired, and inserts the round row last. So:

- Count a round only when its round row has status `complete`. Attempts without one belong to a round that was interrupted.
- Read both tables with `FINAL`. A retried insert can repeat rows, and both tables replace on their sorting key.

Skipped rounds have a round row with status `skipped_budget`, `skipped_balance` or `skipped_error` and no attempts, so gaps read as skipped rounds rather than outages.

Both tables keep 400 days. Rows are small (one per path per round), so there are no minute rollups.

## Install and apply

These migrations are validated with Goose v3.28.0:

```sh
go install github.com/pressly/goose/v3/cmd/goose@v3.28.0
export GOOSE_DRIVER=clickhouse
export GOOSE_MIGRATION_DIR=sql/migrations
export GOOSE_DBSTRING='clickhouse://migration_user:password@localhost:9000/benchmark'
goose -table send_benchmark_goose_db_version up
```

Always pass `-table send_benchmark_goose_db_version`. The tables are prefixed `benchmark_send_`, so they can share a database with the gRPC benchmark's `benchmark_` tables. That benchmark keeps its own Goose history in `benchmark_goose_db_version`, and a separate history table keeps the two sets of migrations from colliding.

Migrations are forward-only. Fix a mistake with a new migration, never by editing an applied one or running `down`.

## Users

- **Sender:** INSERT and DESCRIBE on `benchmark_send_rounds` and `benchmark_send_attempts`, nothing else.
- **Dashboards:** SELECT on both tables.
- **Migrations:** DDL on the database, plus read and write on `send_benchmark_goose_db_version`.
