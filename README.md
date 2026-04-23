# Areal Finance — Contracts

Five Solana on-chain programs that form the [Areal Finance](https://areal.finance) protocol. All built on the [Arlex](https://github.com/ArealFinance/arlex) framework (Pinocchio-based, no Anchor).

| Program | Instructions | Purpose |
|---|---|---|
| [`ownership-token`](./ownership-token) | 8 | Tokenized ownership of an asset; revenue distribution; treasury |
| [`futarchy`](./futarchy) | 8 | Per-OT governance with proposals executed via CPI |
| [`rwt-engine`](./rwt-engine) | 11 | Reward token minting, NAV bookkeeping, vault management |
| [`native-dex`](./native-dex) | 12 | StandardCurve + concentrated-liquidity AMM, swaps, LP |
| [`yield-distribution`](./yield-distribution) | 10 | Merkle-proof claims, USDC → RWT conversion |

## CPI graph

```
┌──────────────────────┐        ┌──────────────────────┐
│  ownership-token     │◄─CPI───│  futarchy            │
└──────────┬───────────┘        └──────────────────────┘
           │ CPI (claim)
           ▼
┌──────────────────────┐        ┌──────────────────────┐
│  yield-distribution  │◄─CPI───│  rwt-engine          │
│                      │───CPI─►│                      │
└──────────┬───────────┘        └──────────┬───────────┘
           │ CPI (convert, compound)       │ CPI (vault_swap)
           ▼                                ▼
                ┌──────────────────────┐
                │  native-dex          │
                └──────────────────────┘
```

## Build

```bash
cargo build-sbf              # all 5 programs
```

Artifacts land in `target/deploy/*.so`. See [ArealFinance/areal](https://github.com/ArealFinance/areal) for the full protocol (dashboard, bots, integration tests).

## Dependencies

- [Arlex v0.1.0](https://github.com/ArealFinance/arlex) — Solana framework
- [Solana Agave 3.1.11](https://github.com/anza-xyz/agave) — `cargo-build-sbf`
- Rust 1.94.1 (toolchain `1.89.0` for SBF)

## Repository layout

```
contracts/
├── Cargo.toml                  # workspace root
├── futarchy/
├── native-dex/
├── ownership-token/
├── rwt-engine/
└── yield-distribution/
```

## License

Apache-2.0 — see [LICENSE](./LICENSE) and [NOTICE](./NOTICE).
