# Integration & Emergency-Withdrawal Notes

A plain-English record of how this wallet was made to (a) run on a **real, drive-backed
libcma ledger** and (b) support **on-chain emergency withdrawal** — plus every tool we had
to change, *why*, and how to make it painless for the next person who runs this.

If you're new to Cartesi, read the 60-second primer first.

---

## 60-second primer (for beginners)

- **Cartesi Machine** — a small, deterministic virtual computer that boots Linux and runs
  your app. "Deterministic" means: same inputs → exactly the same result, every time. That's
  what lets a blockchain trust its output.
- **Flash drive** — a virtual hard disk attached to the machine. The app sees it as a Linux
  device like `/dev/pmem1`. Whatever is on the drive is part of the machine's "fingerprint"
  (a cryptographic hash of the whole machine state).
- **libcma** — a library (C++ core + a Rust wrapper) that keeps a **ledger** of who owns how
  much of which token. This wallet uses it as its bookkeeping.
- **Accounts drive** — the specific flash drive where libcma stores that ledger. Because the
  drive is part of the machine fingerprint, the balances on it can be **proven on-chain**.
- **Emergency withdrawal** — if the operator running the app disappears, a user can take their
  balance out **directly from the smart contracts**, by proving "the accounts drive said I had
  X" — no live server required. That's the whole point of this exercise.

---

## Part 1 — Why we vendored a copy of `libcma_binding_rust`

### What "vendoring" means
"Vendoring" = copying a dependency's source **into your own repository** instead of pulling it
from the internet at build time. We put a copy under
[`vendor/libcma_binding_rust/`](vendor/libcma_binding_rust) and point `Cargo.toml` at that
local folder.

### Why we had to
The wallet originally depended on libcma over **git**:

```toml
libcma_binding_rust = { git = "…", default-features = false, features = ["native"] }
```

Two problems made a plain git dependency unworkable **for a real on-machine build**:

1. **The `native` feature is a mock.** libcma ships two modes:
   - `native` — a pure-Rust *fake* ledger, only good for compiling/testing on your laptop.
   - `riscv64` — the *real* C++ ledger, which is what actually runs inside the Cartesi machine.

   The wallet was pinned to `native`, so even the "production" build used the fake ledger.
   Balances lived in RAM and were never written to the accounts drive — useless for
   emergency withdrawal.

2. **The `riscv64` feature needs a prebuilt C++ library that nobody had built.** Its build
   script (`build.rs`) expects to find a compiled archive at
   `third_party/machine-asset-tools/build/riscv64/libcma.a`. That file is **not** produced by
   `cargo` when you use a git dependency — and libcma's own README literally labels this
   feature *"Cross-build later"* (i.e. it was never finished). So there was nothing to link.

We solved it by **building `libcma.a` ourselves** (see Part 2) and **vendoring** the binding
together with that prebuilt archive and the C headers it needs, so the Docker build can link
the real ledger without any network access or guesswork.

### How to make this clean with git/cargo (for other users)
Vendoring works, but it's the "get it running today" answer, not the "nice for everyone"
answer. The proper fixes, in order of preference:

| Approach | What it is | Effort | Who fixes it |
| --- | --- | --- | --- |
| **A. Self-building `riscv64` feature (best)** | Change libcma's `build.rs` so that, when the `riscv64` feature is on, it **compiles `libcma.a` from its bundled C++ source** at build time (using the GCC-14 cross-toolchain the Dockerfile already installs). Then a normal `git`/`crates.io` dependency "just works". | Medium | libcma maintainer (you) |
| **B. A `libcma-sys` crate** | Follow the standard Rust `*-sys` pattern: a crate that carries the C/C++ source and a `build.rs` that compiles it. Publish to crates.io. Then `libcma_binding_rust = "x.y"` needs zero manual steps. | Medium | libcma maintainer |
| **C. Download a prebuilt `libcma.a`** | `build.rs` downloads a per-release prebuilt archive (one per target) from GitHub Releases and links it. No compiler needed by the user. | Low–Medium | libcma maintainer |
| **D. Keep vendoring (what we did)** | Commit the prebuilt archive + headers into the app repo. | Done | app author |

**Recommended:** Option **A** or **B**, published by the libcma project. Once that exists,
this wallet's `Cargo.toml` can drop the `vendor/` folder and go back to:

```toml
[target.'cfg(target_arch = "riscv64")'.dependencies]
libcma_binding_rust = { version = "x.y", features = ["riscv64"] }   # or git = "…"

[target.'cfg(not(target_arch = "riscv64"))'.dependencies]
libcma_binding_rust = { version = "x.y", features = ["native"] }
```

with **no manual archive-building at all**.

---

## Part 2 — Every tool/service we changed (and how to fix it properly)

Here is everything we had to touch to get from "doesn't build" to "tokens recovered on-chain".

### 1. `libcma.a` — the real C++ ledger, cross-compiled for RISC-V
- **Source:** [`machine-asset-tools`](https://github.com/Mugen-Builders/machine-asset-tools) (the C++ core behind libcma).
- **What was failing:** It is normally built inside an **emulated** RISC-V Docker container
  (`make docker`), which is slow and kept hitting Docker DNS errors. It also needs **GCC 14**
  (the source refuses older compilers), the **Boost** and **nlohmann/json** libraries, and the
  `-fhardened` flag that only newer GCC understands.
- **What we did:** Cross-compiled it the fast way — on a normal x86 machine using the RISC-V
  *cross* toolchain (`g++-14-riscv64-linux-gnu`), inside a quick `docker run` container (where
  the network works). We supplied the missing `nlohmann/json.hpp` and overrode the compiler to
  GCC 14. Output: a 2.1 MB `libcma.a` for riscv64, copied into the vendored binding.
- **Best going forward:** Fold this into libcma's `build.rs` (Part 1, Option A) so users never
  run these commands. The exact recipe is in the wallet [README](README.md#rebuilding-libcmaa).

### 2. `libcma_binding_rust` — the Rust wrapper
- **Source:** [Mugen-Builders/libcma_binding_rust](https://github.com/Mugen-Builders/libcma_binding_rust) (your own library).
- **What was failing:** `native`-only (mock); `riscv64` feature unfinished; `build.rs` couldn't
  find `libcma.a`; and because libcma is **C++**, the final link was missing the C++ runtime.
- **What we did:** Vendored it; switched the wallet to the `riscv64` feature on the machine and
  `native` on the host (target-conditional dependency); added one line to its `build.rs` to link
  the C++ standard library (`cargo:rustc-link-lib=dylib=stdc++`).
- **Best going forward:** Publish the self-building `riscv64` feature (Part 1) **and** keep the
  `stdc++` link line upstream, so no one has to patch `build.rs`.

### 3. The Cartesi CLI (`@cartesi/cli`)
- **Source:** published on npm (`@cartesi/cli@2.0.0-alpha.34`).
- **What was failing:** When assembling the machine it tells `cartesi-machine` to attach a drive
  with the option `filename:…`. The installed `cartesi-machine` **0.20.0** renamed that option to
  `data_filename:…` (upstream commit `1c9388f`). The published CLI predates the rename, so every
  build died with *"unknown option filename"*. (Pinning the SDK can't help — the CLI runs the
  host `cartesi-machine` first.)
- **What we did:** Made a copy of the CLI's bundled JavaScript and changed that one word
  (`filename` → `data_filename`); we run the patched copy as `~/.local/bin/cartesi-patched`.
- **Best going forward:** Use a CLI newer than alpha.34 once it's published (`npm i -g @cartesi/cli@latest`)
  — the fix is already in the CLI's source repo, just unreleased. Until then, the one-word patch is
  the workaround. An external user should **not** be expected to patch a bundle by hand; the
  cleanest interim fix is to **build the CLI from its source repo** (which has the rename) and ship
  that, or wait for the release.

### 4. Docker Desktop DNS
- **Source:** Docker Desktop on Linux (the machine's container engine).
- **What was failing:** Its internal DNS resolver (`192.168.65.7` / `127.0.0.53`) timed out
  intermittently, so image pulls and `apt`/`ADD` steps inside builds failed.
- **What we did:** Set a reliable DNS in `~/.docker/daemon.json`
  (`{ "dns": ["8.8.8.8","1.1.1.1"] }`) and restarted Docker Desktop.
- **Best going forward:** Document this as a prerequisite (already in the README). It's an
  environment quirk, not a project bug.

### 5. `account-driver-reader` — the proof generator
- **Source:** `machine-asset-tools/tools/account-driver-reader.cpp` (libcma's own tool).
- **What was failing:** It was written for cartesi-machine **0.19**. On **0.20** the C API
  changed: `cm_load_new` gained a "sharing mode" argument and `cm_get_proof` changed shape
  (it now also takes a root-size and returns the proof as JSON text). It wouldn't even compile.
- **What we did:** Ported it — added `CM_SHARING_NONE` to `cm_load_new` and
  `CM_HASH_TREE_LOG2_ROOT_SIZE` to `cm_get_proof` at the three call sites. It now builds against
  0.20 and links the installed `libcartesi`.
- **Best going forward:** Upstream this port to machine-asset-tools so it tracks 0.20+. The
  release note even says proof generation will eventually move behind a proper JSON-RPC API in
  the node — at which point this standalone tool may not be needed at all.

### 6. The on-chain withdrawal output builder
- **Source:** the protocol contracts only shipped `UsdWithdrawalOutputBuilder` (a simple
  single-asset format that does **not** match libcma's multi-asset account record).
- **What was failing:** There was no on-chain contract that could read libcma's 128-byte account
  record and turn it into a token transfer.
- **What we did:** Found and deployed
  [`GenericWithdrawalOutputBuilder`](devnet/contracts/GenericWithdrawalOutputBuilder.sol)
  (from `lynoferraz/aux-cr-contracts`) — its decoder is a **byte-exact match** for libcma's
  record (type + owner + token + tokenId + amount), and it emits ETH/ERC-20/721/1155 transfers.
- **Best going forward:** Get this builder into the official contracts/devnet so it's deployed
  by default (right now we deploy it by hand after each devnet reset).

### 7. The wallet app itself (`src/main.rs`, `Dockerfile`, `cartesi.toml`)
- **What we changed:**
  - **`cartesi.toml`** (new) — declares the two drives (app + raw accounts drive).
  - **`src/main.rs`** — opens the ledger from the drive (`init_from_file("/dev/pmem1", …)`) on
    the machine, and calls **`libc::sync()` before every yield** (the keystone fix — see Part 3).
  - **`Dockerfile`** — installs `libstdc++6` in the machine's filesystem (libcma is C++), and
    bakes in the **devnet portal addresses** (the wallet recognises a deposit by *who sent it*,
    so its configured portal must match the chain it runs on).
- **Best going forward:** Make the portal addresses configurable at deploy/runtime instead of
  baked at build time, so the same image works on any network.

### 8. The proof "transform"
- **What it is:** libcma's proof tool and the node's `cartesi-rollups-cli` speak **different proof
  dialects**. We wrote a small Python transform that splits libcma's single full proof into the
  two files the CLI wants, converts the hashes from base64 to hex, and computes the
  accounts-drive root.
- **Best going forward:** Ship this transform as a small script/subcommand (ideally inside
  `account-driver-reader` itself, emitting the CLI's format directly), so no one re-derives it.

---

## Part 3 — The emergency-withdrawal lifecycle, explained simply

Think of it as a **bank vault with a self-service emergency exit**.

### The everyday picture
1. You **deposit** tokens through a "portal" contract. The tokens move into the application's
   smart contract, and the app's libcma ledger writes a record on the accounts drive:
   *"this address owns 1000 of this token."*
2. Normally, you'd ask the running app to send your tokens back (it produces a voucher). That
   needs the operator's server to be alive and honest.

### The emergency picture (what we built)
If the operator vanishes, you don't need them. Here's the chain of events, in human terms:

1. **Foreclose** — a designated "guardian" flips the app into emergency mode. (Think: pulling the
   fire alarm. After this, the app stops taking new inputs and the emergency exit opens.)
2. **Take a snapshot of the vault's contents** — we use the machine's last agreed-upon state
   (the "settled epoch"), which includes the accounts drive with everyone's balances.
3. **Generate a proof** — a tool reads the accounts drive and produces a **mathematical receipt**
   that says: *"at this agreed-upon state, this exact account record was present."* This receipt
   is a Merkle proof — a short chain of hashes that the smart contract can check without seeing
   the whole drive.
4. **Anchor the drive's fingerprint on-chain** (`prove-drive-root`) — submit the accounts-drive
   root hash once, and the contract confirms it matches the agreed state.
5. **Withdraw** — submit your account's receipt. The contract checks it against the anchored
   fingerprint, reads your balance from your record, and **sends your tokens directly to you** —
   no server involved.

The crucial detail that made this possible: libcma keeps its ledger in *mapped memory*, and on
the Cartesi disk those writes were sitting in a cache and **never reached the actual drive**.
So the "vault" looked empty to the proof tool. The fix was to have the app run a **`sync()`** (a
"flush everything to disk now" command) right before it pauses — exactly how the older example
app got its data onto the drive. After that one line, the drive contained the balances and the
whole proof chain worked.

### How a user runs/tests it (step by step)

> Prerequisites: Docker, Foundry (`anvil`/`cast`/`forge`), the patched Cartesi CLI, and
> `cartesi-rollups-cli`. See the [README prerequisites](README.md#prerequisites).

```sh
# 0) Build the machine (once)
cartesi-patched build

# 1) Start a local blockchain + the node (in devnet/)
cd devnet
./run_devnet.sh up
docker compose -f compose.local.yaml up -d

# 2) Deploy the GenericWithdrawalOutputBuilder, put its address in withdrawal.json,
#    then deploy the wallet WITH the withdrawal config
#    (see devnet/withdrawal.json and the README for the exact commands)

# 3) Mint test tokens and deposit
cast send --rpc-url http://localhost:8545 --private-key <anvil-key> <token> "mint(uint256)" 1000000
cartesi-rollups-cli deposit erc20 cma-wallet-wd --portal <erc20-portal> --token <token> --amount 1000 --approve --yes

# 4) Foreclose (as the guardian)
cartesi-rollups-cli foreclose cma-wallet-wd --yes

# 5) Generate the proof from the latest snapshot
account-driver-reader --mem-length 4194304 --n-accounts 4096 --n-assets 256 --n-balances 4096 \
  --dump-full-proof <snapshot-dir> 0x90000000000000 <accounts-drive.bin> <owner> <token> > full-proof.json

# 6) Transform it into the two proof files, then submit
cartesi-rollups-cli prove-drive-root cma-wallet-wd --proof-file drive-root-proof.json --yes
cartesi-rollups-cli withdraw         cma-wallet-wd --proof-file withdraw-proof.json   # confirm the prompt

# 7) Check your tokens came back
cast call <token> "balanceOf(address)(uint256)" <your-address> --rpc-url http://localhost:8545
```

When it works, the application contract's token balance drops to **0** and your wallet's balance
goes **up by the amount you deposited** — recovered with no live node.

---

## Part 4 — Way forward: making this easy for everyone

Right now this works, but it took many manual steps. To make it a one-command experience:

1. **Fix libcma upstream so `riscv64` self-builds (or publish `libcma-sys`).** Removes the entire
   "vendor a prebuilt archive" dance. *(Biggest win.)*
2. **Release a Cartesi CLI ≥ the `data_filename` fix.** Removes the bundle patch; users just
   `npm i -g @cartesi/cli@latest`.
3. **Upstream the `account-driver-reader` 0.20 port** and have it **emit the CLI's proof format
   directly** (or fold the transform into a small script). Removes the Python step and the
   base64/split/keccak guesswork.
4. **Get `GenericWithdrawalOutputBuilder` into the default devnet/contracts.** Removes the manual
   `forge create` after every chain reset.
5. **Make the wallet's portal addresses configurable at runtime**, so one built image works on any
   network (no rebuild to change chains).
6. **Add a single `Makefile`/script** in `devnet/` that runs the whole happy path
   (`up → deploy builder → deploy app → deposit → foreclose → prove → withdraw → verify`) so a
   newcomer can type one command and watch tokens get recovered.
7. **Pin every version** (CLI, SDK, node, contracts, libcma, Boost, GCC) in one place, since this
   whole flow is sensitive to version drift between the machine (0.20), the CLI (alpha.34), the
   node (alpha.12), and the contracts (v3-alpha).

The single most valuable cleanup is **#1** (libcma self-building) plus **#6** (a one-command
script) — together they turn a multi-hour expedition into "clone, run one script, see it work."
