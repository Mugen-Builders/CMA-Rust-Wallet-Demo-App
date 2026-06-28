# Complete Emergency-Withdrawal Flow

A deep, end-to-end walkthrough of how this wallet makes balances recoverable straight from the
smart contracts — starting at the flash drive declared in `cartesi.toml`, through how the drive is
structured inside the Cartesi machine, how the node snapshots it, how each withdrawal-config
parameter shapes the proof, and how the stock on-chain `UsdWithdrawalOutputBuilder` turns a raw
ledger record into a token transfer.

Because this wallet uses libcma's **single-asset** ledger, every account is the **standard 32-byte
accounts-drive record**, so the whole flow runs on the **default** Cartesi tooling: the stock
`UsdWithdrawalOutputBuilder` and `cartesi-rollups-machine-tool` — no custom builder, no
proof-transform script.

This is the conceptual companion to the runnable steps in the [README](README.md#test-2--emergency-withdrawal-terminal).

---

## 1. The drive starts in `cartesi.toml`

```toml
[drives.accounts]
builder = "empty"
format  = "raw"     # no filesystem — libcma lays its own structure on the bytes
size    = 4194304   # 4 MiB
mount   = false     # the app opens the block device directly
user    = "dapp"
```

At build time the Cartesi CLI tells `cartesi-machine` to attach a second flash drive. A flash
drive is a **PMA** (physical-memory-array) region mapped into the machine's 64-bit address space
at a fixed base. For the accounts drive that base is **`0x90000000000000`**, so:

- Inside the guest it appears as the block device **`/dev/pmem1`**.
- Those same bytes occupy a **known, fixed slice** of the machine's address space.

Why that matters: the Cartesi machine hashes its *entire* state as one big Merkle tree over the
address space (8-byte words at the leaves, folding up to a single 32-byte **machine state root**).
Because the accounts drive sits at a fixed address, its 4 MiB of bytes form a **fixed subtree** of
that global tree. Anything written there is provable against the machine state root — and that root
is what gets committed on-chain. `mount=false` + `format=raw` means there is no filesystem in the
way: libcma opens the raw device and writes its own table directly onto the bytes.

---

## 2. How balances get onto the drive — and into the snapshot

### The ledger layout

On boot, [`src/main.rs`](src/main.rs) `WalletApp::open_ledger` calls
`Ledger::init_single_from_file("/dev/pmem1", …, LedgerAsset::Erc20(token))`. libcma **`mmap`s** the
device and lays a fixed array of **32-byte balance records** at the **start** of the drive — this is
the standard Cartesi accounts-drive leaf. Each record (`cma_ledger_single_balance_t`) is:

```
balance(8, little-endian) | owner(20) | padding(4)   -> 32 bytes
```

A real record dumped from a snapshot (a 1000-token deposit to anvil acct0):

```
e803000000000000 f39fd6e51aad88f6f4ce6ab8827279cfffb92266 00000000
^balance=0x3e8=1000 (LE u64)  ^owner (20 bytes)            ^padding
```

The token is **not** in the record — there is exactly one asset (fixed at drive creation), so the
on-chain builder is bound to that token at deploy time (see §6). libcma keeps the records array as
the drive's **static prefix** (`max_accounts × 32 B = 4096 × 32 B = 128 KiB`); its heap index maps
live in the space *after* that prefix, which is deliberately kept **outside** the proven region (§3).

### The flush (the keystone fix)

On the **non-DAX** Cartesi pmem device, `mmap(MAP_SHARED)` writes only dirty the guest **page
cache** — they do **not** reach the drive's PMA bytes that the machine actually hashes. So the
wallet calls **`libc::sync()` at the top of the rollup loop, before every yield**. That pushes the
dirty pages down to the real `/dev/pmem1` PMA. Without it, the drive looks empty to the prover even
though `ledger_getBalance` would still report the right number from the page cache.

### Who builds the snapshot — and the misconception to untangle

The machine state that everyone proves against is the one the node **settles** on-chain at the end of
each epoch (the claimed machine state root). To recover the actual bytes of that state off-chain, the
default `cartesi-rollups-machine-tool replay` **re-runs the settled epoch's inputs from the node DB**
against the machine template, reconstructing the full machine state — including
`0090000000000000-400000.bin`, the raw accounts drive — into `replay-snapshot/`. (No `EVERY_EPOCH`
snapshot policy is needed; replay rebuilds the state on demand.)

So there are **two distinct steps**, often conflated:

```
app calls sync()                     replay rebuilds the settled-epoch machine
  -> balances land in the     ===>     from the node DB (the drive is part of
     /dev/pmem1 PMA *inside*           that state)
     the machine
```

libcma never "flushes to the snapshot." It flushes to the **drive**; the settled machine state
**contains the drive**. That same machine state root is what the node's claim settles on-chain —
which is exactly why the drive bytes become provable later.

---

## 3. How the drive maps to a Merkle subtree

The geometry below is fixed by two of the withdrawal-config parameters
(`log2_leaves_per_account` and `log2_max_num_of_accounts`) plus the machine's 32-byte data block
(`2^5`):

```
                              machine state root            level 64
                                     ▲
              47 siblings ( prove-drive-root proof )         ...        ← anchored by prove-drive-root
                                     ▲
                       accounts-drive merkle root           level 17    (= 5 + 12 + 0)
                                     ▲
              12 siblings ( account_root_siblings )          ...        ← carried by withdraw
                                     ▲
                       one 32-byte account leaf             level 5     (= 5 + 0)
```

- **Account leaf** = `2^(5 + log2_leaves_per_account)` = `2^5` = **32 bytes** → `log2_target_size = 5`.
- **Accounts region** = `2^(5 + log2_max_num_of_accounts + log2_leaves_per_account)` = `2^17` = 128 KiB
  (the records prefix at the start of the 4 MiB drive) → its root sits at **level 17**.
- **Machine root** is at level 64, so the full leaf→root path has `64 - 5 = 59` siblings, split at
  level 17 (index `17 - 5 = 12`): **12** `account_root_siblings` below the accounts-drive root and
  **47** above it for `prove-drive-root`.

Note the region is **exactly** the 128 KiB records prefix, not the whole 4 MiB drive: libcma's heap
index maps sit immediately after the records and are left **outside** the proven subtree. (Their
`account_to_id` nodes store 20-byte owner addresses, which would otherwise false-match the prover's
by-address account scan — which is why `log2_max_num_of_accounts` is `12`, sizing the region to the
records, not `17` for the whole drive as a simple non-libcma dApp would use.)

---

## 4. The withdrawal-config parameters

These live in [`devnet/withdrawal.json`](devnet/withdrawal.json) and are baked into the
**application contract** at deploy time:

| Param | Value | Effect on the flow |
|---|---|---|
| `log2_leaves_per_account` | `0` | Account record = `2^0 = 1` leaf × the 32-byte (`2^5`) block = **32 bytes**. Fixes the record size and `log2_target_size = 5`. |
| `log2_max_num_of_accounts` | `12` | Up to `2^12 = 4096` accounts → the **accounts subtree is 12 levels deep** → a withdraw proof carries **12 `account_root_siblings`**. Also sizes the proven region to the 128 KiB records prefix (keeping libcma's heap out of it). |
| `accounts_drive_start_index` | `309237645312` | = `0x90000000000000 / 2^(5+12+0)` = drive base ÷ accounts-region size (`2^17`). Tells the contract **where** the accounts subtree sits in the machine tree, so it knows the left/right path when folding the drive-root proof up to the machine state root. |
| `guardian` | anvil acct0 | The only address allowed to **foreclose** the application. |
| `withdrawal_output_builder` | the stock `UsdWithdrawalOutputBuilder` (per-token) | The on-chain contract that **decodes a 32-byte record into an ERC-20 transfer** of its bound token (see §6). |

The first two define the tree geometry; `accounts_drive_start_index` positions that geometry inside
the machine; `guardian` gates the emergency switch; `withdrawal_output_builder` is the on-chain
decoder.

---

## 5. The flow: foreclose → prove-drive-root → withdraw

1. **Foreclose** (guardian only). Flips the app contract into emergency mode; it stops accepting
   inputs. The last **settled** epoch's machine state root is now the reference everyone proves
   against.

2. **Generate the proofs off-chain.** `cartesi-rollups-machine-tool` first `replay`s the settled
   epoch into a `replay-snapshot/`, then `prove accounts-drive` reads that snapshot and writes the
   **two proof files directly** — `drive-root-proof.json` (the 47-sibling drive-root proof +
   `accounts_drive_merkle_root`) and `withdraw-proof.json` (the account bytes + `account_index` +
   12 `account_root_siblings`). The `--accounts-drive-*` flags must match `withdrawal.json`.

3. **`prove-drive-root`** (permissionless, once per app). You submit `accounts_drive_merkle_root` +
   the **47-sibling** drive-root proof. The contract folds the root up through those 47 siblings
   (direction from `accounts_drive_start_index`) and checks it equals the **settled machine state
   root**. On match it **anchors** `accounts_drive_merkle_root` on-chain.

4. **`withdraw`** (per account). You submit the **32-byte account**, its `account_index`, and the
   **12 `account_root_siblings`**. The contract:
   - recomputes the account leaf hash from the bytes and folds up the 12 siblings (direction from
     `account_index`) → must equal the **anchored** `accounts_drive_merkle_root`;
   - `STATICCALL`s the builder (§6) to turn the record into a transfer output;
   - executes that output → tokens move from the app contract to the `owner` in the record.

---

## 6. The stock `UsdWithdrawalOutputBuilder`

The `UsdWithdrawalOutputBuilder` is the on-chain **decoder** that bridges libcma's single-asset
record and an executable transfer — and, crucially, it **ships in the protocol contracts**; this
repo deploys the stock one rather than a custom contract. In `withdraw`, the app contract calls
`buildWithdrawalOutput(appContract, account)` on it via `STATICCALL`. The builder reads the 32-byte
record and returns a single output:

```
balance(8, little-endian) | owner(20) | padding(4)   →   ERC-20 transfer( token, owner, balance )
```

The `token` is **not** in the record — it is bound to the builder at deploy time (the builder is
created per-token), which is why the wallet's `WALLET_TOKEN_ADDRESS` and the builder's token must be
the same ERC-20. The decoder is a **byte-exact match** for libcma's 32-byte
`cma_ledger_single_balance_t`, which is the whole reason single-asset works with the default tooling.

### When is it registered, and is it per-user?

- **Registered at app deploy time**, not per withdrawal. `cartesi-rollups-cli deploy
  --withdrawal-config-file` passes `withdrawal_output_builder` into the application factory, which
  stores it in the **application contract's** withdrawal config. The **node never needs it** — this
  is purely an on-chain relationship between the app contract and the builder. (The
  `CARTESI_DEVNET_WITHDRAWAL_OUTPUT_BUILDER_ADDRESS` in the compose file is only a devnet
  convenience, not the authoritative wiring.)
- **One deployment for the whole app — not per user.** The builder is a **stateless `view`/`pure`
  decoder**; each `withdraw` call hands it that user's account bytes and it decodes whichever record
  it's given. A single instance therefore serves every account of its bound token. (In this repo
  `run_devnet.sh up` redeploys it each run only because an anvil reset wipes the chain — not because
  it's user-specific.)

---

## 7. No proof "transform" step (the default tool emits both files)

The multi-asset variant needed a `transform_proof.py` script to split a single full proof from a
custom reader into the two files the CLI wants. **Single-asset doesn't:** `cartesi-rollups-machine-tool
prove accounts-drive` writes both files directly, already in the CLI's hex format:

| File (`--out-*`) | Contents | Consumed by |
|---|---|---|
| `drive-root-proof.json` | `{ accounts_drive_merkle_root, proof[47] }` | `prove-drive-root` |
| `withdraw-proof.json` | `{ account, account_index, account_root_siblings[12] }` | `withdraw` |

The tool internally does what the old script did by hand — it derives the level-17 accounts-drive
root (the split at index `5 + log2_max_num_of_accounts + log2_leaves_per_account = 12`), keccak-folds
the account leaf with its 12 siblings to get `accounts_drive_merkle_root`, and emits hex — but as a
maintained part of the default toolchain. The repo's old `transform_proof.py` has been **deleted**.

---

## 8. The whole path, in one line

```
cartesi.toml accounts drive (0x90000000000000, /dev/pmem1)
  -> libcma writes 32-byte single-asset records on deposit (128 KiB records prefix)
  -> app sync() flushes them to the drive PMA
  -> node settles the epoch's machine state root on-chain (drive included)
  -> foreclose (guardian)
  -> machine-tool replay -> rebuild the settled-epoch machine from the node DB
  -> machine-tool prove accounts-drive -> { drive-root-proof.json (47), withdraw-proof.json (12) }
  -> prove-drive-root  anchors accounts_drive_merkle_root against the settled state root
  -> withdraw          verifies the account in that root, STATICCALLs the stock UsdWithdrawalOutputBuilder
  -> tokens move from the app contract back to the owner — no live node, default tooling only
```
