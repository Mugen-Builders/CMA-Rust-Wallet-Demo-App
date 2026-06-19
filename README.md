# cma-rust-wallet

A Cartesi Rollups demo application showing how to build an asset wallet on top
of the [`libcma_binding_rust`](../cma-parsers) library.

The point of the demo is a clean split of responsibilities:

| Concern | Owner |
| --- | --- |
| Decoding portal deposits & user inputs | **libcma** (`cma_decode_advance`) |
| Decoding inspect queries | **libcma** (`cma_decode_inspect`) |
| Tracking every balance (deposit/withdraw/transfer) | **libcma** ledger (`Ledger`) |
| Encoding withdrawal vouchers | **libcma** (`cma_encode_voucher`) |
| User registration | **the application** |
| Activity history & query endpoints | **the application** |

libcma manages all the money; the application manages the users.

## Supported assets

Ether, ERC-20, ERC-721, ERC-1155 single, and ERC-1155 batch — for **deposits**
and **withdrawals**, plus internal **transfers** between registered accounts.

## Behaviour

* **Deposits** arrive from the Cartesi portals. libcma decodes them and the
  ledger is credited. Deposits are always accepted (the funds already moved
  on-chain).
* **Withdrawals / transfers** arrive as user inputs. They are only honored for
  **registered** accounts. A withdrawal debits the ledger and emits an on-chain
  voucher built by `cma_encode_voucher`. A transfer is an internal ledger move.
* **Registration** is application logic: send an advance input whose payload is
  the UTF-8 JSON `{"method":"register","nickname":"alice"}`.

Every advance produces a JSON **report** describing what happened (or why it was
rejected), so the result is observable off-chain.

## Inspect endpoints

Inspect payloads are UTF-8 JSON. Ledger queries are decoded by libcma; the rest
are application endpoints.

| Method | Params | Returns |
| --- | --- | --- |
| `ledger_getBalance` (libcma) | `["0x<account>", "0x<token>"?, "0x<tokenId>"?]` | account balance for an asset |
| `ledger_getTotalSupply` (libcma) | `["0x<token>"?, "0x<tokenId>"?]` | total supply of an asset |
| `wallet_getUser` | `["0x<address>"]` | registration profile, or `null` |
| `wallet_listUsers` | _none_ | all registered users |
| `wallet_getHistory` | `["0x<address>"]` (optional) | activity log, optionally filtered |

Asset resolution convention for queries: zero token → Ether; token with zero
token id → ERC-20; token + token id → token-with-id (ERC-721 / ERC-1155).

## Build

The production binary is built for the Cartesi machine (riscv64) via the
`Dockerfile`:

```bash
cartesi build
```

To type-check on the host (uses libcma's pure-Rust `native` mock ledger):

```bash
cargo check --target "$(rustc -vV | sed -n 's/host: //p')"
```

The committed `.cargo/config.toml` defaults the build target to
`riscv64gc-unknown-linux-gnu`, which is why the host check needs an explicit
`--target`.

## Layout

Everything lives in a single, top-to-bottom annotated `src/main.rs`, organised
into numbered sections so it reads like a walkthrough:

1. Asset model (`AssetKind` → libcma ledger asset types)
2. Application state (`WalletApp`: the ledger + user registry + history)
3. Withdrawal voucher building (`cma_encode_voucher`)
4. Advance handling (deposits, withdrawals, transfers, registration)
5. Inspect handling (balances, supply, users, history)
6. Small helpers
7. Rollup run loop
