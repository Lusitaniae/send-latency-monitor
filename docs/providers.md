# Providers

What each send provider offers, and why the example configuration measures only four paths. Checked against the providers' docs in November 2026, not against live endpoints.

## The default set

The example configuration measures paths that need no provider-specific code and no large tip:

- **Triton**: `sendTransaction`. Staked routing is the default since Cascade was retired in February 2026.
- **Helius Sender**: `sendTransaction` on the `?swqos_only=true` tier, which accepts a 5,000-lamport tip.
- **Jito**: a one-transaction `sendBundle`, tipping 20,000 lamports.
- **A plain RPC**: `sendTransaction`, as the baseline.

The tracker sends JSON-RPC `sendTransaction` or a Jito-style `sendBundle`. Any other provider that takes either is a config entry: a URL, a tip account list and a tip amount.

## Cost per path

At one round a minute, about 43,200 rounds a month, each path costs per region:

| Tip | Tips a month | With fees |
| --- | --- | --- |
| none | 0 | ~0.3 SOL |
| 5,000 lamports | 0.22 SOL | ~0.5 SOL |
| 20,000 lamports | 0.86 SOL | ~1.2 SOL |
| 0.00001 SOL | 0.43 SOL | ~0.7 SOL |
| 0.0001 SOL | 4.3 SOL | ~4.6 SOL |
| 0.001 SOL | 43 SOL | ~43.5 SOL |

Fees are 5,000 lamports base plus 2,000 of priority per copy that lands. Most landing services require 0.001 SOL: about 43 SOL a month per path per region, against about 2.3 SOL for the whole default set. Those paths are left out until the provider agrees benchmark terms.

All paths share one round interval. Sending a tipped path only every few rounds would make its first-on-chain share cover different rounds from the others', so the shares could no longer be compared.

## Other providers

| Provider | Works with the tracker today | Minimum tip | Faster mode (needs code) | Access |
| --- | --- | --- | --- | --- |
| Helius Sender, default | `sendTransaction` | 0.001 SOL, and at least 5,000 lamports of priority (our 2,000 is below) | none | key optional, 1 req/s without |
| Harmonic | `sendBundle` | none: the priority fee is the bid | none documented | none |
| Temporal (Nozomi) | `sendTransaction` | 0.001 SOL; lower tips are dropped | `sendTransaction2`, text body | key |
| 0slot | `sendTransaction` | 0.001 SOL, 0.0001 on Advanced | `/txn` text, `/txb` binary | key |
| Astralane | `sendTransaction` on `/iris` | 0.001 SOL on Free; 0.0001 on VIP1–2; 0.00001 on VIP3 | `/iris2` text, `/irisb` binary | key |
| Stellium | `sendTransaction` | 0.001 SOL | none documented | key; hostnames through Telegram |
| BlockRazor | no: custom JSON body | 100,000 lamports | `/v2/sendTransaction` text, binary | key |
| bloXroute | no: custom JSON body | 0.001 SOL (its pages disagree) | `submit-plain-text` | paid plan, from $250 a month |

Triton also has `/sendtx`, which takes the raw or base64 transaction without a JSON-RPC envelope. Harmonic accepts one-transaction bundles only while they mostly succeed; confirm with Harmonic before adding it. Several providers also offer MEV-protected modes, which trade speed for protection and are out of scope.

The "faster mode" column would need one tracker option: send the transaction as base64 text or raw bytes, compute the signature locally, and count any 2xx response as accepted. It is not built. Providers measured over JSON-RPC while recommending a faster mode can fairly say they were measured on a slower path.

## Before adding a provider

Ask the provider for:

- the endpoint and mode it wants measured, one per provider;
- a benchmark key, if its endpoints need one;
- tip terms the benchmark can afford, or a tier with a lower minimum;
- sign-off on rankings that name it.

The fee payer is public, so a dedicated key does not reveal anything the provider could not already see.

## Sources

- Triton: [sending transactions](https://docs.triton.one/solana/sending-transactions/quickstart), [sendtx](https://docs.triton.one/solana/rpc-methods/sending-transactions/sendtx.md), [Cascade retirement](https://blog.triton.one/swqos-for-everyone-why-we-retired-cascade-marketplace/)
- Helius: [Sender](https://www.helius.dev/docs/sending-transactions/sender), [SWQoS-only](https://www.helius.dev/docs/sending-transactions/sender-swqos-only.md)
- Jito: [low-latency send](https://docs.jito.wtf/lowlatencytxnsend/)
- Harmonic: [bundles](https://docs.harmonic.gg/searchers/harmonic-bundles)
- Temporal: [Nozomi endpoints](https://use.temporal.xyz/nozomi/endpoints.md), [tipping](https://use.temporal.xyz/nozomi/tipping-and-faq)
- 0slot: [docs](https://0slot.trade/docs.html)
- Astralane: [endpoints](https://astralane.gitbook.io/docs/low-latency/endpoints-and-configs), [fee tiers](https://astralane.gitbook.io/docs/low-latency/send-txn-fee-tiers.md)
- Stellium: [fast landing](https://docs.stellium.dev/products/fast-tx-landing.md)
- BlockRazor: [send transaction](https://docs.blockrazor.io/transaction-submission/transaction-sending/solana/send-transaction.md)
- bloXroute: [best landing performance](https://docs.bloxroute.com/solana/trader-api/best-performance-for-landing-transactions.md)
