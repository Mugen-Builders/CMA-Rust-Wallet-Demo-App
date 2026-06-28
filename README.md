<br>
<p align="center">
    <img src="https://github.com/user-attachments/assets/080bb0be-060c-4813-85b4-6d9bf25af01f" align="center" width="20%">
</p>
<br>
<div align="center">
	<i>Cartesi Rollups LIBCMA Rust Wallet Demo</i>
</div>
<br>


# cma-rust-wallet

A Cartesi Rollups demo application showing how to build an asset wallet on top
of the [`libcma_binding_rust`](https://github.com/Mugen-Builders/libcma_binding_rust) library.

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

## What you can run

Two flows that share a single [Setup](#setup-run-once):

| Flow | What it does |
| --- | --- |
| **[Test 1 — libcma ledger](#test-1--libcma-ledger-non-destructive)** | Deposit tokens and read balances back from the drive-backed libcma ledger. **Non-destructive** — stop here or continue. |
| **[Test 2 — emergency withdrawal](#test-2--emergency-withdrawal-terminal)** | Recover funds straight from the contracts by proving the accounts drive — no live node. **Terminal** (it forecloses the app); run after Test 1 on the same instance. |

## **Contents:** 
- [Supported assets](#supported-assets) 
- [Behaviour](#behaviour) 
- [Layout](#layout) 
- [Inspect endpoints](#inspect-endpoints) 
- [Accounts drive & `cartesi.toml`](#accounts-drive--cartesitoml) 
- [libcma drive-backed ledger](#libcma-drive-backed-ledger) 
- [Setup](#setup-run-once) 
- [Test 1 — libcma ledger](#test-1--libcma-ledger-non-destructive) 
- [Test 2 — emergency withdrawal](#test-2--emergency-withdrawal-terminal)
- [Teardown](#teardown)

## Supported assets

A **single ERC-20** — for **deposits** and **withdrawals**, plus internal
**transfers** between registered accounts. The token is fixed when the accounts
drive is first created (asset id 0 in libcma's single-asset ledger) and immutable
thereafter; it is read from the `WALLET_TOKEN_ADDRESS` env var, defaulting to the
bundled devnet's TestFungibleToken (`0x88A2120B7068E78692C8fd12E751d610B6377E4d`).
Deposits of any other asset are rejected.

Why single-asset? libcma's single-asset ledger stores each account as a **32-byte
record** (`balance | owner | padding`) — the standard Cartesi accounts-drive leaf.
That makes emergency withdrawal work with the **default** `cartesi-rollups-machine-tool`
and the stock `UsdWithdrawalOutputBuilder`; no custom 128-byte builder and no
proof-transform script (which the multi-asset variant on `main` needs).

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

## Layout

Everything lives in a single, top-to-bottom annotated `src/main.rs`, organised
into numbered sections so it reads like a walkthrough:

1. The single asset (the fixed ERC-20 → libcma asset id 0)
2. Portal configuration (resolving the ERC-20 portal)
3. Application state (`WalletApp`: the ledger + user registry + history)
4. Withdrawal voucher building (`cma_encode_voucher`)
5. Advance handling (deposits, withdrawals, transfers, registration)
6. Inspect handling (balances, supply, users, history)
7. Small helpers
8. Rollup run loop

## Inspect endpoints

Inspect payloads are UTF-8 JSON. Ledger queries are decoded by libcma; the rest
are application endpoints.

| Method | Params | Returns |
| --- | --- | --- |
| `ledger_getBalance` (libcma) | `["0x<account>", "0x<token>"?]` | account balance of the configured ERC-20 |
| `ledger_getTotalSupply` (libcma) | `["0x<token>"?]` | total supply of the configured ERC-20 |
| `wallet_getUser` | `["0x<address>"]` | registration profile, or `null` |
| `wallet_listUsers` | _none_ | all registered users |
| `wallet_getHistory` | `["0x<address>"]` (optional) | activity log, optionally filtered |

There is only one asset, so the `token` argument is optional: omit it (or pass the
configured token) and the query answers for that ERC-20; a query naming a different
non-zero token is rejected.

## Accounts drive & `cartesi.toml`

Every balance lives in libcma's ledger, and on the Cartesi machine that ledger is backed
by a dedicated **accounts drive** — a raw flash drive whose contents are committed to the
machine state hash, so balances are provable on-chain (the basis for emergency withdrawal).

The machine is described by [`cartesi.toml`](cartesi.toml), with **two drives**:

- `root` — OS + the wallet binary, built from the [`Dockerfile`](Dockerfile) → `/dev/pmem0`.
- `accounts` — a raw, unformatted **4 MiB** flash drive → `/dev/pmem1`, which libcma opens
  as the balance ledger:

  ```toml
  [drives.accounts]
  builder = "empty"
  format  = "raw"     # unformatted (the CLI equivalent of cartesi-machine mke2fs:false)
  size    = 4194304   # BYTES; multiple of the 4096-byte page size. (A "4Mi" string would
                      # be parsed as 4 bytes — the CLI size parser ignores the suffix.)
  mount   = false     # the app opens the block device directly
  user    = "dapp"
  ```

## libcma drive-backed ledger

The wallet uses the **real riscv64 libcma** on the machine (not the host `native` mock),
backed by the accounts drive. Three pieces make that work:

1. **GitHub dependency, no vendoring** — [`Cargo.toml`](Cargo.toml) pulls
   [`libcma_binding_rust`](https://github.com/Mugen-Builders/libcma_binding_rust) straight from
   git, pinned to **`branch = "feat/single-asset"`** (the single-asset ledger API). Its `build.rs`
   **cross-compiles the real C++ `libcma.a` from source** during the Docker build (it fetches
   `nlohmann/json`, runs `make`, and uses the RISC-V GCC 14 cross toolchain), so the machine links
   the real ledger with no prebuilt archive committed here. The Dockerfile's cross-build stage
   therefore installs `g++-14-riscv64-linux-gnu` (and `wget`).
2. **Target-conditional dependency** ([`Cargo.toml`](Cargo.toml)) — the `riscv64` feature (real
   libcma) for the machine target, the `native` feature (mock) for host type-checks.
3. **`init_single_from_file`** ([`src/main.rs`](src/main.rs), `WalletApp::open_ledger`) — on riscv64
   the ledger opens the drive as a single-asset ledger fixed to the configured ERC-20:

   ```rust
   ledger.init_single_from_file("/dev/pmem1", LedgerSingleFileConfig {
       offset: 0, memory_length: 4 * 1024 * 1024, max_accounts: 4096,
   }, LedgerAsset::Erc20(token))?;
   ```
   `build.rs` also links `stdc++`, and the Dockerfile installs `libstdc++6` in the rootfs.

   **Drive geometry.** libcma writes a `max_accounts × 32 B` array of balance records at the
   **start** of the drive — `4096 × 32 B = 128 KiB = 2^17 bytes`, the **proven records prefix** —
   then keeps its heap index maps in the space after it. So the accounts-drive Merkle proof covers
   only the first 128 KiB: `log2_leaves_per_account = 0` (a 32-byte leaf), `log2_max_num_of_accounts
   = 12` (2^12 = 4096 records), `accounts_drive_start_index = 0x90000000000000 / 2^17 = 309237645312`.
   The heap is deliberately **outside** that region — its `account_to_id` map nodes hold 20-byte
   owner addresses that would otherwise false-match the prover's by-address account scan. (This is
   why we don't span the whole 4 MiB drive like the simple non-libcma reference dApp does.)

The Dockerfile bakes the **devnet portal addresses** into the image ENV
(`ERC20_PORTAL_ADDRESS=0x22E5…`, …). The wallet resolves a deposit's caller to a portal, so
these must match the chain you deploy to.

## Setup (run once)

Both tests run against **one** deployed, funded instance — set it up once here, then run
[Test 1](#test-1--libcma-ledger-non-destructive) and/or
[Test 2](#test-2--emergency-withdrawal-terminal). The app is deployed **with a withdrawal
config**; that doesn't change normal operation — it only enables Test 2's foreclosure.

### Prerequisites

- **Docker** + **Docker Compose**. On Docker Desktop for Linux, if builds fail with DNS
  timeouts, set engine DNS: `~/.docker/daemon.json` → `{ "dns": ["8.8.8.8","1.1.1.1"] }`,
  then `systemctl --user restart docker-desktop`.
- **Foundry** (`anvil`/`cast`/`forge`): `curl -L https://foundry.paradigm.xyz | bash && foundryup -i v1.4.3`.
- **Cartesi CLI** `@cartesi/cli` **≥ 2.0.0-alpha.35** to build the machine (`cartesi --version`).
  That version emits the `data_filename` flash-drive option cartesi-machine `0.20.0` expects
  (upstream commit `1c9388f`). The older `2.0.0-alpha.34` emitted the legacy `filename` and failed
  with `unknown option filename`, which required patching the CLI bundle — no longer necessary.
- **`cartesi-rollups-cli`** `2.0.0-alpha.12` on PATH (extract from the rollups-node `.deb`) — for deploy/deposit.
- `jq`, `curl`, `python3`, `openssl`.

### Build the machine

```sh
cartesi build             # @cartesi/cli >= 2.0.0-alpha.35
```
Produces `.cartesi/image/` (root + accounts drives). Verify the 4 MiB accounts drive:
```sh
jq -r '.config.flash_drive[] | "len=\(.length)"' .cartesi/image/config.json   # expect one 4194304
```
Host type-check (uses the libcma `native` mock): `cargo check --target "$(rustc -vV | sed -n 's/host: //p')"`.

### Start the devnet & node, deploy the app

All node/devnet tooling lives in [`devnet/`](devnet). Run each step from `devnet/`.

**1. Start anvil + the Cartesi v3 contracts.** `run_devnet.sh` loads them from the release
tarball — nothing to clone or compile.

```sh
cd devnet
export PATH="$HOME/.foundry/bin:$PATH"
./run_devnet.sh up
```

**2. Stage the machine image where the node can see it.** `cartesi build` wrote the image to the
project root (`../.cartesi/image`), but the node container mounts `devnet/.cartesi/image`, so copy
it in.

```sh
rm -rf .cartesi/image && mkdir -p .cartesi && cp -r ../.cartesi/image .cartesi/image
```

**3. Bring up the split-services node and load the host env.**

```sh
docker compose -f compose.local.yaml up -d
source host.env
```

**4. The withdrawal output builder is already deployed.** Single-asset records are the standard
32-byte accounts-drive leaf, so we use the **stock per-token `UsdWithdrawalOutputBuilder`** — no
custom contract. `run_devnet.sh up` (step 1) already deployed it deterministically for the devnet
token at `0x0745787835A019cd4dae8EDB541Fbc0647793d63`, and that address is baked into
[`devnet/withdrawal.json`](devnet/withdrawal.json). Nothing to do here. (Verify if you like:
`cast code 0x0745787835A019cd4dae8EDB541Fbc0647793d63 --rpc-url http://localhost:8545` ≠ `0x`.)

**5. Deploy the wallet with the withdrawal config, and save its contract address.** Use the
template path `.cartesi/image` — **not** `../.cartesi/image`: the advancer resolves the registered
URI relative to its workdir (`/var/lib/cartesi-rollups-node`), and a `../` prefix escapes the mount
and fails with `unable to read '../.cartesi/image/config.json'`. (`accounts_drive_start_index` in
`withdrawal.json` = `0x90000000000000 / 2^(5+12+0)` = `0x90000000000000 / 131072` = `309237645312`.)

```sh
cartesi-rollups-cli deploy application cma-rust-wallet .cartesi/image \
  --withdrawal-config-file withdrawal.json --epoch-length 1 \
  --salt "$(openssl rand -hex 32)" --enable=false | tee /tmp/deploy.log
grep -oiE 'application address:[[:space:]]*0x[0-9a-f]{40}' /tmp/deploy.log | grep -oiE '0x[0-9a-f]{40}' > .app_addr
```

**6. Enable the app.** With `--epoch-length 1` each input settles its own epoch, and the
emergency-withdrawal flow **replays the node DB** into a snapshot on demand (Test 2, step 3) — so no
`EVERY_EPOCH` snapshot policy is required; the proof is reconstructed from the settled epoch.

```sh
cartesi-rollups-cli app status cma-rust-wallet enabled --yes
```

When you're finished with both tests, see [Teardown](#teardown) to stop everything.

## Test 1 — libcma ledger (non-destructive)

With [Setup](#setup-run-once) done, deposit tokens and read the balance back from the
drive-backed libcma ledger.

```sh
TOKEN=0x88A2120B7068E78692C8fd12E751d610B6377E4d
ACC0=0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266
PK=0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80

cast send --rpc-url http://localhost:8545 --private-key $PK $TOKEN "mint(uint256)" 1000000
cartesi-rollups-cli deposit erc20 cma-rust-wallet \
  --portal 0x22E57511C30CcE6CDaa742E13CE3b774fDC663b1 --token $TOKEN --amount 1000 --approve --yes

# read it back from the libcma drive ledger (allow a few seconds to process)
curl -s http://localhost:10012/inspect/cma-rust-wallet \
  --data-binary "{\"method\":\"ledger_getBalance\",\"params\":[\"$ACC0\",\"$TOKEN\"]}" \
  | python3 -c 'import sys,json,binascii;d=json.load(sys.stdin);print(binascii.unhexlify(d["reports"][0]["payload"][2:]).decode())'
# -> {"query":"balance",...,"balance":"1000"}
```

You can also exercise transfers, registration, and balance/supply/history queries via the
[Inspect endpoints](#inspect-endpoints). Stop here, or continue to
[Test 2](#test-2--emergency-withdrawal-terminal) to recover these funds with no live node.

## Test 2 — emergency withdrawal (terminal)

The accounts drive makes balances recoverable on-chain even if the node stops — this is what
that buys you, end to end. **It forecloses the app** (which then stops accepting inputs), so run
it after [Test 1](#test-1--libcma-ledger-non-destructive) on the same funded instance.

How it works:

- ✅ libcma's single-asset account record is the **standard 32-byte accounts-drive leaf**
  (`balance` (8, little-endian) + `owner` (20) + `padding` (4)), a **byte-exact match** for the
  stock per-token **`UsdWithdrawalOutputBuilder`** deployed in Setup. No custom builder, no
  proof-transform script — that is the payoff of going single-asset.
- ✅ Withdrawal-config params (in [`devnet/withdrawal.json`](devnet/withdrawal.json)):
  `log2_leaves_per_account = 0` (a 32-byte leaf), `log2_max_num_of_accounts = 12` (the 4096-record
  prefix), `accounts_drive_start_index = 309237645312`, guardian = Anvil account 0, builder = the
  stock `UsdWithdrawalOutputBuilder`. These **must match** the `machine-tool prove accounts-drive`
  flags below.
- ✅ **Ledger persistence with zero libcma changes.** libcma `mmap`s `/dev/pmem1` (`MAP_SHARED`);
  on the non-DAX Cartesi pmem device those writes only dirty the **page cache** and never reach
  the drive PMA the snapshot captures. The wallet calls **`libc::sync()` before each yield**
  ([`src/main.rs`](src/main.rs), top of the rollup loop), flushing the ledger to the drive — so the
  accounts-drive captured by the node contains the libcma balance records after every deposit.
- ✅ **Default proof generation** — the stock **`cartesi-rollups-machine-tool`** (the `machine-tool`
  service in [`devnet/compose.local.yaml`](devnet/compose.local.yaml), `tools` profile) replays the
  node DB into a snapshot and emits the two proof files directly. No ported `account-driver-reader`,
  no `transform_proof.py`.

### Interaction Steps

First set the shared variables (run from `devnet/`).

```sh
cd devnet
export PATH="$HOME/.foundry/bin:$PATH"
source host.env
mkdir -p artifacts

APP=cma-rust-wallet
TOKEN=0x88A2120B7068E78692C8fd12E751d610B6377E4d
ACC0=0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266
```

**1. Foreclose the app.** Guardian-only — signed by the `CARTESI_AUTH_*` key (anvil acct0).

```sh
cartesi-rollups-cli foreclose $APP --yes
```

**2. Find the latest settled epoch** (the reference state everyone proves against).

```sh
EPOCH=$(cartesi-rollups-cli read epochs $APP --status CLAIM_ACCEPTED --limit 1 --descending \
  | jq -r '.data[0].index')
echo "settled epoch: $EPOCH"
```

**3. Replay the node DB into a machine snapshot** up to that epoch (no live node needed).

```sh
docker compose -f compose.local.yaml run --rm machine-tool replay \
  --template /var/lib/cartesi-rollups-node/snapshot \
  --application $APP --to-epoch "$EPOCH" \
  --store /artifacts/replay-snapshot
```

**4. Prove the accounts drive + this account's balance.** The `--accounts-drive-*` flags **must
match `withdrawal.json`** (the single-asset geometry derived above).

```sh
docker compose -f compose.local.yaml run --rm machine-tool prove accounts-drive \
  --snapshot /artifacts/replay-snapshot \
  --accounts-drive-start-index 309237645312 \
  --log2-max-num-of-accounts 12 --log2-leaves-per-account 0 \
  --account $ACC0 \
  --out-drive-root-proof /artifacts/drive-root-proof.json \
  --out-withdraw-proof  /artifacts/withdraw-proof.json
```

**5. Anchor the accounts-drive root on-chain** (one-time per foreclosed app; expect `status 1`),
then **withdraw the account**.

```sh
cartesi-rollups-cli prove-drive-root $APP --proof-file artifacts/drive-root-proof.json --yes
cartesi-rollups-cli withdraw         $APP --proof-file artifacts/withdraw-proof.json   --yes
```

**6. Verify on-chain.** The app contract should read `0` and the user should be back to `1000000`.

```sh
APP_ADDR=$(cat .app_addr)
echo "app contract: $(cast call $TOKEN 'balanceOf(address)(uint256)' $APP_ADDR --rpc-url http://localhost:8545)"
echo "user (acct0): $(cast call $TOKEN 'balanceOf(address)(uint256)' $ACC0     --rpc-url http://localhost:8545)"
```

When step 6 prints **app contract 0 / user 1000000**, the deposited tokens were recovered straight
from the contracts using only the accounts-drive Merkle proof — no operator, no live node, and only
the **default** Cartesi tooling.

## Teardown

Stop everything that the [Setup](#setup-run-once) started, from `devnet/`.

**1. Stop the node and remove its data.** `down -v` also drops the Postgres volume, so the app
registration and processed inputs are cleared for a clean next run.

```sh
cd devnet
docker compose -f compose.local.yaml down -v
```

**2. Stop the anvil devnet.**

```sh
./run_devnet.sh down
```

**3. (Optional) Remove the per-run files** left in `devnet/` — the staged machine image, the
node's bind-mounted data/snapshots, the deployed-builder/app-address notes, and the generated
proofs.

```sh
rm -rf .cartesi/image node-data node-snapshots artifacts .app_addr
```

The build artifacts at the project root (`.cartesi/image`) and the anvil-state cache
(`devnet/anvil-state/`) are kept — they let you skip the rebuild/redownload next time. Delete them
too if you want a completely fresh start.
