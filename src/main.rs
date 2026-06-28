//! # libcma single-asset demo wallet — a Cartesi Rollups tutorial app
//!
//! This single file is meant to be read top to bottom. It is the **single-ERC20**
//! variant of the libcma demo wallet: the ledger tracks exactly one ERC-20 token,
//! fixed when the accounts drive is first created and immutable thereafter.
//!
//! Why a single asset? libcma's single-asset ledger stores each account as a
//! **32-byte record** (`balance | owner | padding`) — the standard Cartesi
//! accounts-drive leaf. That means emergency withdrawal works with the **default**
//! `cartesi-rollups-machine-tool` and the stock `UsdWithdrawalOutputBuilder`; no
//! custom 128-byte builder, no proof-transform script. (The multi-asset variant on
//! `main` needs all of those because its record is 128 bytes.)
//!
//! It still shows the three things you use `libcma_binding_rust` for:
//!
//!   1. **Parsing** rollup inputs        → `cma_decode_advance` / `cma_decode_inspect`
//!   2. **Accounting** for the asset      → the single-asset `Ledger` (`cma_ledger_*`)
//!   3. **Building withdrawal vouchers**  → `cma_encode_voucher`
//!
//! The guiding split of responsibilities is unchanged:
//!
//! * **libcma owns the money.** Deposits, withdrawals and transfers are delegated to
//!   the ledger. We never track balances by hand.
//! * **The application owns the users.** Registration, nicknames and the activity
//!   history are plain Rust collections — the part libcma leaves to you.
//!
//! Policy: anyone can deposit (the funds already moved on-chain), but only a
//! *registered* account may withdraw or transfer. Only the one configured ERC-20 is
//! accepted; deposits of any other asset are rejected.

use std::collections::HashMap;

use ethers_core::types::Bytes;
use json::{object, JsonValue};

// --- libcma: parsing, ledger and voucher building ---------------------------
use libcma_binding_rust::ledger::Ledger;
use libcma_binding_rust::parser::{
    cma_decode_advance, cma_decode_inspect, cma_encode_voucher, CmaParserBalance,
    CmaParserErc20VoucherFields, CmaParserError, CmaParserInput, CmaParserInputData,
    CmaParserInputType, CmaParserSupply, CmaParserVoucherType, CmaVoucher, CmaVoucherFieldType,
};
use libcma_binding_rust::{
    AccountType, Address, AssetType, LedgerAccountId, LedgerAssetId, RetrieveOperation, U256,
};

// --- libcmt: the rollup I/O (read inputs, emit vouchers/reports) -------------
use libcmt_binding_rust::cmt_rollup_finish_t;
use libcmt_binding_rust::rollup::{Advance, Rollup};

// ===========================================================================
// 1. The single asset
//
// The whole wallet is denominated in ONE ERC-20 token, chosen at deploy time and
// baked into the accounts drive on first boot (libcma rejects reopening the drive
// with a different token). We read it from the environment so the same binary can
// target any network, defaulting to the bundled devnet's TestFungibleToken.
//
// In the single-asset ledger this token is asset id 0 — there is no other asset to
// identify, so unlike the multi-asset demo there is no `AssetKind` enum to carry.
// ===========================================================================

/// The fixed ERC-20 the ledger denominates everything in.
fn configured_token() -> Address {
    std::env::var("WALLET_TOKEN_ADDRESS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| {
            "0x88A2120B7068E78692C8fd12E751d610B6377E4d"
                .parse()
                .expect("valid default token address")
        })
}

/// Short human label used in reports and history.
fn token_label(token: Address) -> String {
    format!("ERC20({:#x})", token)
}

// ===========================================================================
// 2. Portal configuration
//
// libcma decodes a deposit once you tell it which portal layout to expect, but it
// deliberately does NOT hardcode portal addresses — they differ per network and
// change over time, so choosing them is the application's responsibility. A
// single-asset wallet only ever needs the ERC-20 portal; we read it from the
// environment, defaulting to the cartesi-cli local devnet ERC-20 portal.
// ===========================================================================

struct PortalAddresses {
    erc20: Address,
}

impl PortalAddresses {
    fn from_env() -> Self {
        fn addr(var: &str, default: &str) -> Address {
            std::env::var(var)
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(|| default.parse().expect("valid default portal address"))
        }
        Self {
            erc20: addr("ERC20_PORTAL_ADDRESS", "0x22E57511C30CcE6CDaa742E13CE3b774fDC663b1"),
        }
    }

    /// Map an advance's caller to the deposit layout libcma should expect. The
    /// ERC-20 portal means an ERC-20 deposit; any other caller is a user input we
    /// let libcma auto-decode (a withdrawal/transfer selector, or an application
    /// command).
    fn deposit_req_type(&self, sender: Address) -> CmaParserInputType {
        if sender == self.erc20 {
            CmaParserInputType::CmaParserInputTypeErc20Deposit
        } else {
            CmaParserInputType::CmaParserInputTypeAuto
        }
    }
}

// ===========================================================================
// 3. Application state
//
// The single-asset ledger (libcma) plus the application's own registry and history.
// ===========================================================================

/// Application registration record.
struct User {
    nickname: String,
    registered_at_block: u64,
}

/// One line in the activity log — pure application bookkeeping. The authoritative
/// balances always come from the ledger.
struct Activity {
    kind: &'static str, // "deposit" | "withdrawal" | "transfer_out" | "transfer_in"
    account: Address,
    amount: String,
    counterparty: Option<Address>,
    block_number: u64,
}

struct WalletApp {
    ledger: Ledger,
    token: Address, // the single ERC-20 this ledger denominates everything in
    portals: PortalAddresses,
    users: HashMap<Address, User>,
    history: Vec<Activity>,
    // Context for the input currently being processed, refreshed each advance.
    app_address: Address, // our own rollup address; carried into vouchers
    block_number: u64,
}

impl WalletApp {
    fn new() -> Result<Self, String> {
        let token = configured_token();
        Ok(Self {
            ledger: Self::open_ledger(token)?,
            token,
            portals: PortalAddresses::from_env(),
            users: HashMap::new(),
            history: Vec::new(),
            app_address: Address::zero(),
            block_number: 0,
        })
    }

    /// Open the libcma single-asset ledger.
    ///
    /// On the Cartesi machine (riscv64) the ledger is backed by the raw `accounts` flash
    /// drive at `/dev/pmem1`, so every balance persists in the machine state and is provable
    /// on-chain for emergency withdrawal. On the host (the libcma `native` mock) it is purely
    /// in-memory, which is all the host type-check / tests need.
    ///
    /// The drive geometry is chosen so the **proven region equals the records prefix**:
    /// `max_accounts = 4096` → a 4096 × 32 B = 128 KiB records array at offset 0, with
    /// libcma's heap maps living *after* it. The accounts-drive Merkle proof therefore covers
    /// exactly the 32-byte balance records (`log2_max_num_of_accounts = 12`,
    /// `log2_leaves_per_account = 0`) and never the heap — see `cartesi.toml` / `devnet/withdrawal.json`.
    #[allow(unused_variables)]
    fn open_ledger(token: Address) -> Result<Ledger, String> {
        #[allow(unused_mut)]
        let mut ledger = Ledger::new().map_err(|e| format!("ledger init failed: {:?}", e))?;
        #[cfg(target_arch = "riscv64")]
        {
            use libcma_binding_rust::ledger::{LedgerAsset, LedgerSingleFileConfig};
            // The `accounts` drive is a raw 4 MiB flash drive (see cartesi.toml). The single
            // ERC-20 is fixed here on first open and immutable for the life of the drive.
            ledger
                .init_single_from_file(
                    "/dev/pmem1",
                    LedgerSingleFileConfig {
                        offset: 0,
                        memory_length: 4 * 1024 * 1024,
                        // Capacity of the withdrawable-balance drive. 4096 × 32 B = the 128 KiB
                        // (2^17) records prefix that the accounts-drive proof covers exactly.
                        max_accounts: 4096,
                    },
                    LedgerAsset::Erc20(token),
                )
                .map_err(|e| format!("accounts-drive single-asset ledger init failed: {:?}", e))?;
        }
        Ok(ledger)
    }

    /// Refresh per-input context (called once at the top of every advance).
    fn begin_advance(&mut self, advance: &Advance) {
        if let Ok(addr) = advance.app_contract.parse::<Address>() {
            self.app_address = addr;
        }
        self.block_number = advance.metadata.block_number;
    }

    // --- application registry ----------------------------------------------

    fn register(&mut self, address: Address, nickname: String) -> Result<String, String> {
        if self.users.contains_key(&address) {
            return Err(format!("account {:#x} is already registered", address));
        }
        self.users.insert(
            address,
            User {
                nickname: nickname.clone(),
                registered_at_block: self.block_number,
            },
        );
        Ok(format!("registered {:#x} as '{}'", address, nickname))
    }

    fn require_registered(&self, address: Address) -> Result<(), String> {
        if self.users.contains_key(&address) {
            Ok(())
        } else {
            Err(format!("account {:#x} is not registered", address))
        }
    }

    fn log(&mut self, kind: &'static str, account: Address, amount: U256, cp: Option<Address>) {
        self.history.push(Activity {
            kind,
            account,
            amount: amount.to_string(),
            counterparty: cp,
            block_number: self.block_number,
        });
    }

    /// Reject any deposit/withdrawal/transfer whose token isn't the one this ledger
    /// denominates. The single-asset ledger maps *every* asset request onto the one
    /// fixed asset (id 0), so without this guard a deposit of some other ERC-20 would
    /// be silently credited as the configured token.
    fn require_configured_token(&self, token: Address) -> Result<(), String> {
        if token == self.token {
            Ok(())
        } else {
            Err(format!(
                "unsupported token {:#x}; this wallet only handles {:#x}",
                token, self.token
            ))
        }
    }

    // --- ledger glue (libcma owns all balances) ----------------------------
    //
    // libcma identifies the asset and accounts by small integer ids. The single asset
    // is always id 0; accounts are looked up (and created on first use) by address.

    /// The single asset's id, creating it on first use (always 0 on the real ledger).
    fn asset(&mut self) -> Result<LedgerAssetId, String> {
        self.ledger
            .retrieve_erc20_asset_via_address(self.token)
            .map_err(|e| format!("retrieve asset: {:?}", e))
    }

    /// Account id, creating the account on first use (write paths).
    fn account(&mut self, address: Address) -> Result<LedgerAccountId, String> {
        self.ledger
            .retrieve_account_via_address(address)
            .map_err(|e| format!("retrieve account: {:?}", e))
    }

    // --- deposits: always accepted, libcma credits the ledger --------------

    fn deposit(&mut self, account: Address, amount: U256) -> Result<(), String> {
        let asset = self.asset()?;
        let account_id = self.account(account)?;
        self.ledger
            .deposit(asset, account_id, amount)
            .map_err(|e| format!("deposit: {:?}", e))?;
        self.log("deposit", account, amount, None);
        Ok(())
    }

    // --- withdrawals: registration-gated; libcma debits then builds voucher -

    /// Debit the ledger (the shared step of every withdrawal). Fails if the account
    /// is unregistered or under-funded.
    fn debit(&mut self, receiver: Address, amount: U256) -> Result<(), String> {
        self.require_registered(receiver)?;
        let asset = self.asset()?;
        let account_id = self.account(receiver)?;
        self.ledger
            .withdraw(asset, account_id, amount)
            .map_err(|e| format!("withdraw: {:?}", e))?;
        self.log("withdrawal", receiver, amount, None);
        Ok(())
    }

    fn withdraw(
        &mut self,
        receiver: Address,
        amount: U256,
        exec_data: Bytes,
    ) -> Result<CmaVoucher, String> {
        self.debit(receiver, amount)?;
        // libcma turns the request into a ready-to-emit on-chain voucher.
        withdrawal_voucher(self.token, receiver, amount, exec_data, self.app_address)
    }

    // --- transfers: internal ledger move between two registered accounts ----

    fn transfer(&mut self, from: Address, to: Address, amount: U256) -> Result<(), String> {
        self.require_registered(from)?;
        self.require_registered(to)?;
        let asset = self.asset()?;
        let from_id = self.account(from)?;
        let to_id = self.account(to)?;
        self.ledger
            .transfer(asset, from_id, to_id, amount)
            .map_err(|e| format!("transfer: {:?}", e))?;
        self.log("transfer_out", from, amount, Some(to));
        self.log("transfer_in", to, amount, Some(from));
        Ok(())
    }

    // --- read-only views for inspects (never create ledger entries) ---------

    fn balance(&mut self, account: Address) -> Result<U256, String> {
        let asset = match self.asset_find() {
            Some(id) => id,
            None => return Ok(U256::zero()),
        };
        let account_id = match self.account_find(account) {
            Some(id) => id,
            None => return Ok(U256::zero()),
        };
        self.ledger
            .get_balance(asset, account_id)
            .map_err(|e| format!("get_balance: {:?}", e))
    }

    fn total_supply(&mut self) -> Result<U256, String> {
        match self.asset_find() {
            Some(id) => self
                .ledger
                .get_total_supply(id)
                .map_err(|e| format!("get_total_supply: {:?}", e)),
            None => Ok(U256::zero()),
        }
    }

    /// Look up the asset id without creating it (read paths).
    fn asset_find(&mut self) -> Option<LedgerAssetId> {
        self.ledger
            .retrieve_asset(
                None,
                Some(self.token),
                None,
                AssetType::TokenAddress,
                RetrieveOperation::Find,
            )
            .ok()
    }

    /// Look up an account id without creating it (read paths).
    fn account_find(&mut self, address: Address) -> Option<LedgerAccountId> {
        self.ledger
            .retrieve_account(
                None,
                AccountType::WalletAddress,
                RetrieveOperation::Find,
                Some(address.as_bytes()),
            )
            .ok()
    }
}

// ===========================================================================
// 4. Withdrawal voucher building (libcma `cma_encode_voucher`)
//
// One place that turns "release these funds" into the exact destination/value/
// payload the Cartesi voucher needs — here always an ERC-20 transfer.
// ===========================================================================

fn withdrawal_voucher(
    token: Address,
    receiver: Address,
    amount: U256,
    _exec_data: Bytes,
    app_address: Address,
) -> Result<CmaVoucher, String> {
    cma_encode_voucher(
        CmaParserVoucherType::CmaParserVoucherTypeErc20,
        Some(app_address),
        CmaVoucherFieldType::Erc20VoucherFields(CmaParserErc20VoucherFields {
            token,
            receiver,
            amount,
        }),
    )
    .map_err(|e| e.message())
}

// ===========================================================================
// 5. Advance handling (deposits, withdrawals, transfers, registration)
// ===========================================================================

async fn handle_advance(
    app: &mut WalletApp,
    rollup: &mut Rollup,
) -> Result<bool, Box<dyn std::error::Error>> {
    let advance = rollup.read_advance_state()?;
    println!("advance from {}: {}", advance.msg_sender, advance.payload);
    app.begin_advance(&advance);

    // The portal that called us tells libcma which deposit layout to expect.
    // Resolving the caller to a portal is the application's job (libcma does not
    // hardcode addresses); any non-portal caller is a user input we let libcma
    // auto-decode (a withdrawal/transfer selector, or an application command).
    let sender = advance.msg_sender.parse::<Address>().unwrap_or_else(|_| Address::zero());
    let req_type = app.portals.deposit_req_type(sender);

    let input = parser_input(&advance.msg_sender, &advance.payload);
    let decoded = match cma_decode_advance(req_type, input) {
        Ok(decoded) => decoded,
        Err(e) => {
            // A malformed portal/user payload. (Unknown user selectors are NOT
            // errors — libcma returns them as `Unidentified`, handled below.)
            fail(rollup, &format!("could not decode input: {}", e.message()));
            return Ok(false);
        }
    };

    // Run the matching operation; turn its result into a report either way.
    match run_advance(app, rollup, &advance, decoded) {
        Ok(message) => ack(rollup, &message),
        Err(message) => fail(rollup, &message),
    }
    Ok(true)
}

/// Route a decoded input to the right ledger/voucher operation and return a
/// human-readable result message (or error). Only the single configured ERC-20 is
/// honored; ether and NFT inputs are rejected.
fn run_advance(
    app: &mut WalletApp,
    rollup: &mut Rollup,
    advance: &Advance,
    decoded: CmaParserInput,
) -> Result<String, String> {
    use CmaParserInputData as In;
    match decoded.input {
        // ----- Deposit: libcma credits the ledger --------------------------
        In::Erc20Deposit(d) => {
            app.require_configured_token(d.token)?;
            app.deposit(d.sender, d.amount)?;
            Ok(format!("credited {} {} to {:#x}", d.amount, token_label(d.token), d.sender))
        }

        // ----- Withdrawal: libcma debits, then we emit the voucher ---------
        In::Erc20Withdrawal(w) => {
            app.require_configured_token(w.token)?;
            let voucher = app.withdraw(w.receiver, w.amount, exec_to_bytes(&w.exec_layer_data))?;
            emit_voucher(rollup, &voucher)?;
            Ok(format!(
                "withdrew {} {} to {:#x}; voucher -> {}",
                w.amount,
                token_label(w.token),
                w.receiver,
                voucher.destination
            ))
        }

        // ----- Transfer: internal ledger move ------------------------------
        In::Erc20Transfer(t) => {
            app.require_configured_token(t.token)?;
            let from = sender(advance)?;
            let to = u256_to_address(t.receiver);
            app.transfer(from, to, t.amount)?;
            Ok(format!(
                "transferred {} {} from {:#x} to {:#x}",
                t.amount,
                token_label(t.token),
                from,
                to
            ))
        }

        // ----- Application command (e.g. registration) ---------------------
        In::Unidentified(_) => app_command(app, advance),

        // Balance/Supply are inspect-only.
        In::Balance(_) | In::Supply(_) => {
            Err("balance/supply are inspect queries, not advances".into())
        }

        // ----- Everything else: this wallet handles a single ERC-20 only ---
        _ => Err("unsupported asset; this wallet handles a single ERC-20".into()),
    }
}

/// Handle a raw payload libcma did not recognise: the demo treats it as the UTF-8
/// JSON command `{"method":"register","nickname":"alice"}`.
fn app_command(app: &mut WalletApp, advance: &Advance) -> Result<String, String> {
    let from = sender(advance)?;
    let text =
        decode_utf8(&advance.payload).ok_or("payload is not a known operation or UTF-8 command")?;
    let command = json::parse(&text).map_err(|_| "command is not valid JSON")?;

    match command["method"].as_str().unwrap_or("") {
        "register" => {
            let nickname = command["nickname"].as_str().unwrap_or("").to_string();
            app.register(from, nickname)
        }
        other => Err(format!("unknown command '{}'", other)),
    }
}

fn emit_voucher(rollup: &mut Rollup, voucher: &CmaVoucher) -> Result<(), String> {
    rollup
        .emit_voucher(&voucher.destination, Some(&voucher.value), &voucher.payload)
        .map(|_| ())
        .map_err(|e| format!("emit voucher: {e}"))
}

// ===========================================================================
// 6. Inspect handling (balances, supply, users, history)
// ===========================================================================

async fn handle_inspect(
    app: &mut WalletApp,
    rollup: &mut Rollup,
) -> Result<bool, Box<dyn std::error::Error>> {
    let inspect = rollup.read_inspect_state()?;
    println!("inspect: {}", inspect.payload);

    // Let libcma decode the ledger queries it knows (`ledger_getBalance`,
    // `ledger_getTotalSupply`); everything else is an application query.
    let input = parser_input("0x0000000000000000000000000000000000000000", &inspect.payload);
    match cma_decode_inspect(input) {
        Ok(decoded) => match decoded.input {
            CmaParserInputData::Balance(q) => balance_query(app, rollup, q),
            CmaParserInputData::Supply(q) => supply_query(app, rollup, q),
            _ => fail(rollup, "unsupported ledger query"),
        },
        Err(CmaParserError::IncompatibleInput) => app_inspect(app, rollup, &inspect.payload),
        Err(e) => fail(rollup, &e.message()),
    }
    Ok(true)
}

fn balance_query(app: &mut WalletApp, rollup: &mut Rollup, q: CmaParserBalance) {
    // The single-asset ledger denominates the one configured token; a query that
    // names a different (non-zero) token is asking about an asset we don't track.
    if !q.token.is_zero() && q.token != app.token {
        return fail(rollup, &format!("unsupported token {:#x}", q.token));
    }
    let account = u256_to_address(q.account);
    match app.balance(account) {
        Ok(balance) => report(rollup, object! {
            "query" => "balance",
            "account" => format!("{:#x}", account),
            "asset" => token_label(app.token),
            "balance" => balance.to_string(),
        }),
        Err(e) => fail(rollup, &e),
    }
}

fn supply_query(app: &mut WalletApp, rollup: &mut Rollup, q: CmaParserSupply) {
    if !q.token.is_zero() && q.token != app.token {
        return fail(rollup, &format!("unsupported token {:#x}", q.token));
    }
    match app.total_supply() {
        Ok(supply) => report(rollup, object! {
            "query" => "total_supply",
            "asset" => token_label(app.token),
            "total_supply" => supply.to_string(),
        }),
        Err(e) => fail(rollup, &e),
    }
}

/// Application inspect endpoints, dispatched on the JSON `method`:
///   * `wallet_getUser`    params: `["0x<address>"]`
///   * `wallet_listUsers`  no params
///   * `wallet_getHistory` params: `["0x<address>"]` (optional; omit for all)
fn app_inspect(app: &mut WalletApp, rollup: &mut Rollup, payload_hex: &str) {
    let text = match decode_utf8(payload_hex) {
        Some(t) => t,
        None => return fail(rollup, "inspect payload is not UTF-8 JSON"),
    };
    let query = match json::parse(&text) {
        Ok(q) => q,
        Err(_) => return fail(rollup, "inspect payload is not valid JSON"),
    };
    let first = query["params"][0].as_str().map(|s| s.parse::<Address>());

    match query["method"].as_str().unwrap_or("") {
        "wallet_getUser" => match first {
            Some(Ok(addr)) => {
                let user = app.users.get(&addr).map_or(JsonValue::Null, |u| object! {
                    "address" => format!("{:#x}", addr),
                    "nickname" => u.nickname.clone(),
                    "registered_at_block" => u.registered_at_block,
                });
                report(rollup, object! { "query" => "user", "user" => user });
            }
            _ => fail(rollup, "wallet_getUser expects params[0] = address"),
        },
        "wallet_listUsers" => {
            let mut users = JsonValue::new_array();
            for (addr, u) in &app.users {
                let _ = users.push(object! {
                    "address" => format!("{:#x}", addr),
                    "nickname" => u.nickname.clone(),
                    "registered_at_block" => u.registered_at_block,
                });
            }
            report(rollup, object! { "query" => "users", "users" => users });
        }
        "wallet_getHistory" => {
            let filter = match first {
                Some(Ok(addr)) => Some(addr),
                Some(Err(_)) => return fail(rollup, "wallet_getHistory: invalid address"),
                None => None,
            };
            let mut entries = JsonValue::new_array();
            for a in app.history.iter().filter(|a| filter.is_none_or(|f| a.account == f)) {
                let _ = entries.push(object! {
                    "kind" => a.kind,
                    "account" => format!("{:#x}", a.account),
                    "asset" => token_label(app.token),
                    "amount" => a.amount.clone(),
                    "counterparty" => a.counterparty.map_or(JsonValue::Null, |c| format!("{:#x}", c).into()),
                    "block_number" => a.block_number,
                });
            }
            report(rollup, object! { "query" => "history", "history" => entries });
        }
        other => fail(rollup, &format!("unknown inspect method '{}'", other)),
    }
}

// ===========================================================================
// 7. Small helpers
// ===========================================================================

/// Build the JSON envelope libcma's decoders expect.
fn parser_input(msg_sender: &str, payload_hex: &str) -> JsonValue {
    let mut input = JsonValue::new_object();
    input["data"]["metadata"]["msg_sender"] = msg_sender.into();
    input["data"]["payload"] = payload_hex.into();
    input
}

/// The advance's `msg_sender` as an [`Address`].
fn sender(advance: &Advance) -> Result<Address, String> {
    advance.msg_sender.parse::<Address>().map_err(|_| "invalid msg_sender".into())
}

/// Low 20 bytes of a left-padded 32-byte word as an [`Address`] (transfer
/// recipients and balance-query accounts arrive this way).
fn u256_to_address(value: U256) -> Address {
    let mut buf = [0u8; 32];
    value.to_big_endian(&mut buf);
    Address::from_slice(&buf[12..32])
}

fn exec_to_bytes(exec_hex: &str) -> Bytes {
    Bytes::from(hex::decode(exec_hex.trim_start_matches("0x")).unwrap_or_default())
}

fn decode_utf8(payload_hex: &str) -> Option<String> {
    String::from_utf8(hex::decode(payload_hex.trim_start_matches("0x")).ok()?).ok()
}

// --- reports: every input produces one observable JSON report --------------

fn report(rollup: &mut Rollup, value: JsonValue) {
    let hex = format!("0x{}", hex::encode(json::stringify(value).as_bytes()));
    if let Err(e) = rollup.emit_report(&hex) {
        eprintln!("failed to emit report: {e}");
    }
}

fn ack(rollup: &mut Rollup, message: &str) {
    println!("ok: {message}");
    report(rollup, object! { "status" => "ok", "message" => message });
}

fn fail(rollup: &mut Rollup, message: &str) {
    println!("error: {message}");
    report(rollup, object! { "status" => "error", "message" => message });
}

// ===========================================================================
// 8. Rollup run loop
// ===========================================================================

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut app = WalletApp::new().map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    let mut rollup = Rollup::new().expect("Failed to create Rollup instance");
    let mut accept_previous_request = true;

    loop {
        // libcma keeps the ledger in an mmap'd region of the accounts drive (/dev/pmem1).
        // On the non-DAX Cartesi pmem device those writes only dirty the page cache, so we
        // flush them to the drive before yielding — otherwise the machine snapshot (and the
        // accounts-drive Merkle root used for emergency withdrawal) would not see them.
        unsafe { libc::sync() };

        println!("Sending finish");
        let mut finish = cmt_rollup_finish_t {
            accept_previous_request,
            next_request_type: 0,
            next_request_payload_length: 0,
        };
        rollup.finish(&mut finish)?;

        accept_previous_request = match finish.next_request_type {
            0 => handle_advance(&mut app, &mut rollup).await?,
            1 => handle_inspect(&mut app, &mut rollup).await?,
            other => {
                eprintln!("Unknown request type: {other}");
                false
            }
        };
    }
}
