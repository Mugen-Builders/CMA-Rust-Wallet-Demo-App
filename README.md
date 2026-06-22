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

**Contents:** [Supported assets](#supported-assets) · [Behaviour](#behaviour) · [Layout](#layout) · [Inspect endpoints](#inspect-endpoints) · [Accounts drive & `cartesi.toml`](#accounts-drive--cartesitoml) · [libcma drive-backed ledger](#libcma-drive-backed-ledger) · [Setup](#setup-run-once) · [Test 1 — libcma ledger](#test-1--libcma-ledger-non-destructive) · [Test 2 — emergency withdrawal](#test-2--emergency-withdrawal-terminal)

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
   git. Its `build.rs` **cross-compiles the real C++ `libcma.a` from source** during the Docker
   build (it fetches `nlohmann/json`, runs `make`, and uses the RISC-V GCC 14 cross toolchain),
   so the machine links the real ledger with no prebuilt archive committed here. The Dockerfile's
   cross-build stage therefore installs `g++-14-riscv64-linux-gnu` (and `wget`).
2. **Target-conditional dependency** ([`Cargo.toml`](Cargo.toml)) — the `riscv64` feature (real
   libcma) for the machine target, the `native` feature (mock) for host type-checks.
3. **`init_from_file`** ([`src/main.rs`](src/main.rs), `WalletApp::open_ledger`) — on riscv64 the
   ledger opens the drive:

   ```rust
   ledger.init_from_file("/dev/pmem1", LedgerFileConfig {
       mode: LedgerMemoryMode::CreateOnly, offset: 0, memory_length: 4 * 1024 * 1024,
       max_accounts: 4096, max_assets: 256, max_balances: 4096,
   })?;
   ```
   `build.rs` also links `stdc++`, and the Dockerfile installs `libstdc++6` in the rootfs.

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
- **Cartesi CLI** to build the machine. The published `@cartesi/cli` `2.0.0-alpha.34` emits the
  old flash-drive option `filename`, but cartesi-machine `0.20.0` wants `data_filename` (upstream
  commit `1c9388f`, not yet released). Patch the installed CLI bundle (`dist/index.js`), changing
  the flash-drive `` `filename:${X}` `` → `` `data_filename:${X}` ``, and invoke that. In this
  workspace the patched copy is `~/.local/bin/cartesi-patched`.
- **`cartesi-rollups-cli`** `2.0.0-alpha.12` on PATH (extract from the rollups-node `.deb`) — for deploy/deposit.
- `jq`, `curl`, `python3`, `openssl`.

### Build the machine

```sh
cartesi-patched build     # the data_filename-patched Cartesi CLI
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

**4. Deploy the `GenericWithdrawalOutputBuilder` and record it in `withdrawal.json`.** It is a
byte-exact match for libcma's 128-byte account record. It needs OpenZeppelin 5.5.0, so it's
compiled in a throwaway forge project. Redeploy this after every `run_devnet.sh up` (an anvil
reset wipes it).

```sh
PK=0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80
B=$(mktemp -d); cp contracts/GenericWithdrawalOutputBuilder.sol "$B/"
( cd "$B" && git init -q \
  && git clone --depth 1 --branch v5.5.0 https://github.com/OpenZeppelin/openzeppelin-contracts lib/openzeppelin-contracts >/dev/null 2>&1 \
  && echo '@openzeppelin-contracts-5.5.0/=lib/openzeppelin-contracts/contracts/' > remappings.txt \
  && mkdir -p src && mv GenericWithdrawalOutputBuilder.sol src/ \
  && forge create src/GenericWithdrawalOutputBuilder.sol:GenericWithdrawalOutputBuilder \
       --rpc-url http://localhost:8545 --private-key $PK --broadcast ) | tee /tmp/builder.log
BUILDER=$(grep -oiE 'Deployed to: 0x[0-9a-f]{40}' /tmp/builder.log | grep -oiE '0x[0-9a-f]{40}')
python3 - "$BUILDER" <<'PY'
import json,sys; p="withdrawal.json"; d=json.load(open(p))
d["withdrawal_output_builder"]=sys.argv[1]; json.dump(d,open(p,"w"),indent=2); print("builder ->",sys.argv[1])
PY
```

**5. Deploy the wallet with the withdrawal config, and save its contract address.** Use the
template path `.cartesi/image` — **not** `../.cartesi/image`: the advancer resolves the registered
URI relative to its workdir (`/var/lib/cartesi-rollups-node`), and a `../` prefix escapes the mount
and fails with `unable to read '../.cartesi/image/config.json'`. (`accounts_drive_start_index` in
`withdrawal.json` = `0x90000000000000 / 2^(5+12+2)` = `77309411328`.)

```sh
cartesi-rollups-cli deploy application cma-rust-wallet .cartesi/image \
  --withdrawal-config-file withdrawal.json --epoch-length 1 \
  --salt "$(openssl rand -hex 32)" --enable=false | tee /tmp/deploy.log
grep -oiE 'application address:[[:space:]]*0x[0-9a-f]{40}' /tmp/deploy.log | grep -oiE '0x[0-9a-f]{40}' > .app_addr
```

**6. Snapshot every epoch (so the accounts drive is captured) and enable the app.**

```sh
cartesi-rollups-cli app execution-parameters set cma-rust-wallet snapshot_policy EVERY_EPOCH
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

- ✅ libcma's account record (`cma_ledger_account_balance_t`, 128 bytes:
  `type` + `owner` + `token_address` + `token_id` + `amount`) is a **byte-exact match** for the
  **`GenericWithdrawalOutputBuilder`** in [`devnet/contracts/`](devnet/contracts), deployed in Setup.
- ✅ Withdrawal-config params: `log2_leaves_per_account = 2` (128 = 4×32 bytes),
  `log2_max_num_of_accounts = 12` (max_balances 4096), guardian = Anvil account 0, builder = the
  deployed `GenericWithdrawalOutputBuilder` (all in [`devnet/withdrawal.json`](devnet/withdrawal.json)).
- ✅ **Ledger persistence with zero libcma changes.** libcma `mmap`s `/dev/pmem1` (`MAP_SHARED`);
  on the non-DAX Cartesi pmem device those writes only dirty the **page cache** and never reach
  the drive PMA the snapshot captures (the erc20 dApp avoids this by using `write()` via `dd`, not
  mmap). The wallet calls **`libc::sync()` before each yield** ([`src/main.rs`](src/main.rs), top
  of the rollup loop), flushing the ledger to the drive — so after a deposit the accounts-drive
  snapshot contains the libcma record (`type=2`, owner, token, `amount`).
- ✅ **`account-driver-reader` ported to cartesi-machine 0.20** — `cm_load_new` takes
  `CM_SHARING_NONE`, `cm_get_proof` takes `log2_root_size` (`CM_HASH_TREE_LOG2_ROOT_SIZE`) and
  returns a JSON proof. The ported host tool reads the ledger and generates the full Merkle proof
  for an account up to the machine root:
  ```sh
  account-driver-reader --mem-length 4194304 --n-accounts 4096 --n-assets 256 --n-balances 4096 \
    --dump-full-proof <snapshot-dir> 0x90000000000000 <accounts-drive.bin> <owner> <token>
  # -> {"log2_target_size":7,"log2_root_size":64,"target_address":...,"target_hash":...,"sibling_hashes":[...],"root_hash":...}
  ```

**Prereqs:** the **`account-driver-reader`** host binary built from
[`machine-asset-tools`](https://github.com/Mugen-Builders/machine-asset-tools) against
cartesi-machine 0.20 (the 0.20 port is described in [INTEGRATION_NOTES.md](INTEGRATION_NOTES.md)),
plus the proof-transform script [`transform_proof.py`](devnet/transform_proof.py) (ships in `devnet/`).

### Run it

First set the shared variables (run from `devnet/`). Point `READER` at your local
`account-driver-reader` build.

```sh
cd devnet
export PATH="$HOME/.foundry/bin:$PATH"
source host.env
mkdir -p artifacts

APP=cma-rust-wallet
TOKEN=0x88A2120B7068E78692C8fd12E751d610B6377E4d
ACC0=0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266
READER=../../machine-asset-tools/build/tools/account-driver-reader
```

**1. Foreclose the app.** Guardian-only — signed by the `CARTESI_AUTH_*` key (anvil acct0).

```sh
cartesi-rollups-cli foreclose $APP --yes
```

**2. Generate the full Merkle proof** from the latest settled-epoch snapshot.

```sh
SNAP=$(ls -dt node-snapshots/${APP}_epoch*_input0 | head -1)
LD_LIBRARY_PATH=/usr/local/lib "$READER" \
  --mem-length 4194304 --n-accounts 4096 --n-assets 256 --n-balances 4096 \
  --dump-full-proof "$SNAP" 0x90000000000000 "$SNAP/0090000000000000-400000.bin" $ACC0 $TOKEN \
  > artifacts/full-proof.json
```

**3. Split the proof into the CLI's two files.** `transform_proof.py` self-verifies that folding
the whole sibling chain reproduces the reader's `root_hash` before writing anything.

```sh
python3 transform_proof.py artifacts/full-proof.json "$SNAP/0090000000000000-400000.bin" artifacts/
```

**4. Anchor the accounts-drive root on-chain** (one-time per foreclosed app; expect `status 1`).

```sh
cartesi-rollups-cli prove-drive-root $APP --proof-file artifacts/drive-root-proof.json --yes
```

**5. Withdraw the account.** The `--yes` flag does **not** skip the withdraw confirm, so feed it a `y`.

```sh
printf 'y\n' | script -qec \
  "cartesi-rollups-cli withdraw $APP --proof-file artifacts/withdraw-proof.json" /dev/null
```

**6. Verify on-chain.** The app contract should read `0` and the user should be back to `1000000`.

```sh
APP_ADDR=$(cat .app_addr)
echo "app contract: $(cast call $TOKEN 'balanceOf(address)(uint256)' $APP_ADDR --rpc-url http://localhost:8545)"
echo "user (acct0): $(cast call $TOKEN 'balanceOf(address)(uint256)' $ACC0     --rpc-url http://localhost:8545)"
```

When step 6 prints **app contract 0 / user 1000000**, the deposited tokens were recovered straight
from the contracts using only the accounts-drive Merkle proof — no operator, no live node.
