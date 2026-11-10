# Providers

What each send provider offers, and which paths the example configuration measures. Checked against the providers' docs in November 2026, not against live endpoints.

## The default set

One path per provider, in a mode that needs no provider-specific code and no large tip:

- **The seven RPC providers in the RPC benchmark**: Alchemy, Chainstack, FluxRPC, Helius, QuickNode, Solana Vibe Station and Triton. Each sends `sendTransaction` on its standard endpoint, untipped. The RPC benchmark's provider accounts can serve these paths, so each provider is measured on the same plan as in the RPC benchmark.
- **Jito**: a one-transaction `sendBundle` to the block engine, tipping 20,000 lamports. Every bundle relay below forwards to it.

A provider can ask for a different mode at sign-off, still one path each. The tracker sends JSON-RPC `sendTransaction` or a Jito-style `sendBundle`, so any mode that takes either is a config entry: a URL, and a tip amount and accounts if it needs a tip.

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

Fees are 5,000 lamports base plus 2,000 of priority per copy that lands. The default set costs about 3.3 SOL a month per region. Most landing services require 0.001 SOL, so they are left out until the provider agrees benchmark terms.

All paths share one round interval. Sending a tipped path only every few rounds would make its first-on-chain share cover different rounds from the others', so the shares could no longer be compared.

## RPC providers

The standard endpoint is not the same product everywhere. Whether `sendTransaction` uses staked connections depends on the provider's plan or host, and two providers route it through MEV protection by default. Record each path's plan and settings with its results.

| Provider | Standard `sendTransaction` | Other send options | Settle before measuring |
| --- | --- | --- | --- |
| Alchemy | Staked, per Alchemy's product pages; plans not named. MEV protection is on by default, through private mempools. | `sendBundle`, a Jito relay: 1,000-lamport minimum tip, paid plans only | Whether MEV protection can be turned off |
| Chainstack | Not documented as staked | Warp: same `sendTransaction` body, no tip, staked; $0.15 a transaction, about $6,500 a month per region; Growth plan and up | Which of the two Chainstack wants measured, given Warp's per-transaction price |
| FluxRPC | Routing documented loosely; staking not stated | None documented | Nothing |
| Helius | Staked on paid plans, not on free ones | Sender (below); `sendBundle`, a Jito relay with a 5,000-lamport minimum tip | That the benchmark's plan is paid |
| QuickNode | Staked on every plan for transactions at the recommended priority fee or above. MEV protection (Blink) is on by default for new endpoints and can be turned off per endpoint. | Lil' JIT add-on, a Jito relay, from $89 a month; Transaction Supercharger, Enterprise | Turn MEV protection off on the benchmark's endpoint |
| Solana Vibe Station | `.rpc` hosts: staking not documented. `.swqos` hosts: staked, $300 to $1,000 a month. | Lightspeed and `sendBundle`: 0.001 SOL minimum tip | The RPC benchmark uses `elite.rpc`; which host to measure is Solana Vibe Station's call |
| Triton | Staked (Jet) on every plan | `/sendtx`, which takes the raw or base64 transaction without a JSON-RPC envelope (needs code) | Nothing |

The benchmark pays 2,000 lamports of priority on 1,000 compute units: 2,000,000 micro-lamports per unit. Check that this meets QuickNode's recommended level.

## Landing services

Left out of the default set, mostly for tip cost:

| Provider | Works with the tracker today | Minimum tip | Faster mode (needs code) | Access |
| --- | --- | --- | --- | --- |
| Helius Sender, SWQoS-only | `sendTransaction` with `?swqos_only=true` | 5,000 lamports | none | key optional, 1 req/s without |
| Helius Sender, default | `sendTransaction` | 0.001 SOL, and at least 5,000 lamports of priority (the benchmark's 2,000 is below) | none | as above |
| Harmonic | `sendBundle` | none: the priority fee is the bid | none documented | none |
| Temporal (Nozomi) | `sendTransaction` | 0.001 SOL; lower tips are dropped | `sendTransaction2`, text body | key |
| 0slot | `sendTransaction` | 0.001 SOL, 0.0001 on Advanced | `/txn` text, `/txb` binary | key |
| Astralane | `sendTransaction` on `/iris` | 0.001 SOL on Free; 0.0001 on VIP1–2; 0.00001 on VIP3 | `/iris2` text, `/irisb` binary | key |
| Stellium | `sendTransaction` | 0.001 SOL | none documented | key; hostnames through Telegram |
| BlockRazor | no: custom JSON body | 100,000 lamports | `/v2/sendTransaction` text, binary | key |
| bloXroute | no: custom JSON body | 0.001 SOL (its pages disagree) | `submit-plain-text` | paid plan, from $250 a month |

Helius Sender's SWQoS-only tier fits the budget. It is out of the default set only because Helius already has its RPC path; Helius can choose Sender instead. Its tip accounts are in the [signed registry](https://github.com/solana-foundation/rpc-latency-monitor/blob/main/sender/tip-accounts.json). Harmonic accepts one-transaction bundles only while they mostly succeed; confirm with Harmonic before adding it.

The "faster mode" column would need one tracker option: send the transaction as base64 text or raw bytes, compute the signature locally, and count any 2xx response as accepted. It is not built. Providers measured over JSON-RPC while recommending a faster mode can fairly say they were measured on a slower path.

## Before adding a provider

Ask the provider for:

- the endpoint and mode it wants measured, one per provider;
- a benchmark key, if its endpoints need one;
- tip terms the benchmark can afford, or a tier with a lower minimum;
- whether MEV protection or another default setting should be on;
- sign-off on rankings that name it.

The fee payer is public, so a dedicated key does not reveal anything the provider could not already see.

## Sources

- Alchemy: [sendTransaction](https://www.alchemy.com/docs/chains/solana/solana-api-endpoints/send-transaction), [Solana RPC](https://www.alchemy.com/overviews/solana-rpc), [Jito bundles](https://www.alchemy.com/docs/chains/solana/solana-jito-bundles-and-transactions), [MEV protection](https://www.alchemy.com/docs/reference/mev-protection)
- Chainstack: [landing transactions](https://docs.chainstack.com/docs/solana-how-to-land-transactions), [Warp](https://docs.chainstack.com/docs/warp-transactions), [pricing](https://chainstack.com/pricing/)
- FluxRPC: [why FluxRPC](https://fluxrpc.com/docs/why-fluxrpc), [methods](https://fluxrpc.com/docs/rpc)
- Helius: [staked connections](https://www.helius.dev/staked-connections), [Sender](https://www.helius.dev/docs/sending-transactions/sender), [SWQoS-only](https://www.helius.dev/docs/sending-transactions/sender-swqos-only.md)
- QuickNode: [transactions](https://www.quicknode.com/docs/solana/transactions), [endpoints and MEV protection](https://www.quicknode.com/docs/solana/endpoints), [routing options](https://www.quicknode.com/blog/comparing-quicknodes-solana-transaction-routing-solutions), [Lil' JIT](https://www.quicknode.com/add-ons/lil-jit-jito-bundles-and-transactions)
- Solana Vibe Station: [RPC](https://docs.solanavibestation.com/services/solana-node-services/rpc-services.md), [staked RPC](https://docs.solanavibestation.com/services/solana-node-services/staked-rpc-services.md), [Lightspeed](https://docs.solanavibestation.com/services/solana-node-services/lightspeed-transactions.md)
- Triton: [sending transactions](https://docs.triton.one/solana/sending-transactions/quickstart), [sendtx](https://docs.triton.one/solana/rpc-methods/sending-transactions/sendtx.md), [Cascade retirement](https://blog.triton.one/swqos-for-everyone-why-we-retired-cascade-marketplace/)
- Jito: [low-latency send](https://docs.jito.wtf/lowlatencytxnsend/)
- Harmonic: [bundles](https://docs.harmonic.gg/searchers/harmonic-bundles)
- Temporal: [Nozomi endpoints](https://use.temporal.xyz/nozomi/endpoints.md), [tipping](https://use.temporal.xyz/nozomi/tipping-and-faq)
- 0slot: [docs](https://0slot.trade/docs.html)
- Astralane: [endpoints](https://astralane.gitbook.io/docs/low-latency/endpoints-and-configs), [fee tiers](https://astralane.gitbook.io/docs/low-latency/send-txn-fee-tiers.md)
- Stellium: [fast landing](https://docs.stellium.dev/products/fast-tx-landing.md)
- BlockRazor: [send transaction](https://docs.blockrazor.io/transaction-submission/transaction-sending/solana/send-transaction.md)
- bloXroute: [best landing performance](https://docs.bloxroute.com/solana/trader-api/best-performance-for-landing-transactions.md)
