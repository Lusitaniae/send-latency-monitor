<h1 align="center">send-latency-monitor</h1>

<p align="center">
  <strong>Head-to-head benchmark of Solana transaction send paths.</strong>
</p>

---

`send-latency-monitor` ranks the ways a transaction can reach a leader, such as RPC `sendTransaction`, landing services and Jito bundles, by where their transactions land in the block.

Every round sends one transaction per configured path at the same instant. All copies share a blockhash, a priority fee and a fee payer, so they contend for one write lock and the leader orders them one after another. The sender then reads each copy's slot and position from a trusted RPC node and records, per path:

- whether the copy landed, failed on chain, or was dropped
- whether it was the round's first copy on chain, and which paths it landed ahead of
- its position in the block, and whether it slipped to a later slot than the first copy
- how long the path took to answer the send, and what the copy cost

Results go to ClickHouse, one row per copy, with each copy's signature, so any result can be checked on chain.

It sits next to the [RPC](https://github.com/solana-foundation/rpc-latency-monitor) and [gRPC](https://github.com/solana-foundation/grpc-latency-monitor) latency monitors. Unlike them it is not read-only: it holds a funded keypair and spends SOL on every round, so it has a daily budget, a minimum balance and per-transaction ceilings. See [Cost and budget](docs/methodology.md#cost-and-budget).

## Quick start

Needs Rust, a ClickHouse database, a trusted Solana JSON-RPC endpoint (never one of the paths under test), and a funded fee payer.

```sh
goose -table send_benchmark_goose_db_version up   # create the tables: sql/README.md
cp config.example.yaml config.local.yaml          # paths, tips, budget, region
cp .env.example .env                              # keypair, RPC, ClickHouse, path URLs
cargo run --release -- --config config.local.yaml --check   # validates, sends nothing
cargo run --release -- --config config.local.yaml
```

`--check` validates the configuration, keypair, reference RPC and ClickHouse schema, prints the worst-case spend per round and per day, and exits.

## Docs

[Methodology](docs/methodology.md) · [Providers](docs/providers.md) · [Schema](sql/README.md)

Licensed under [Apache-2.0](LICENSE).
