//! # libcma demo wallet — a Cartesi Rollups tutorial app
//!
//! This single file is meant to be read top to bottom. It shows the three
//! things you use `libcma_binding_rust` for in a real application:
//!
//!   1. **Parsing** rollup inputs        → `cma_decode_advance` / `cma_decode_inspect`
//!   2. **Accounting** for every asset    → the `Ledger` (`cma_ledger_*`)
//!   3. **Building withdrawal vouchers**  → `cma_encode_voucher`
//!
//! The guiding idea of the demo is a clean split of responsibilities:
//!
//! * **libcma owns the money.** Deposits, withdrawals and transfers are all
//!   delegated to the ledger. We never track balances by hand.
//! * **The application owns the users.** Registration, nicknames and the
//!   activity history are plain Rust collections — the part libcma leaves to you.
//!
//! Policy: anyone can deposit (the funds already moved on-chain), but only a
//! *registered* account may withdraw or transfer.
//!
//! Assets covered: Ether, ERC-20, ERC-721, ERC-1155 single and ERC-1155 batch.

use std::collections::HashMap;

use ethers_core::types::Bytes;
use json::{object, JsonValue};

// --- libcma: parsing, ledger and voucher building ---------------------------
use libcma_binding_rust::ledger::Ledger;
use libcma_binding_rust::parser::{
    cma_decode_advance, cma_decode_inspect, cma_encode_voucher, CmaParserBalance,
    CmaParserErc1155BatchVoucherFields, CmaParserErc1155SingleVoucherFields,
    CmaParserErc20VoucherFields, CmaParserErc721VoucherFields, CmaParserEtherVoucherFields,
    CmaParserError, CmaParserInput, CmaParserInputData, CmaParserInputType, CmaParserSupply,
    CmaParserVoucherType, CmaVoucher, CmaVoucherFieldType,
};
use libcma_binding_rust::{
    AccountType, Address, AssetType, LedgerAccountId, LedgerAssetId, RetrieveOperation, U256,
};

// --- libcmt: the rollup I/O (read inputs, emit vouchers/reports) -------------
use libcmt_binding_rust::cmt_rollup_finish_t;
use libcmt_binding_rust::rollup::{Advance, Rollup};

// ===========================================================================
// 1. Asset model
//
// Every asset class maps onto one of libcma's ledger "asset types". Carrying
// this around lets the deposit/withdraw/transfer/balance code stay generic.
// ===========================================================================

#[derive(Debug, Clone)]
enum AssetKind {
    Ether,                  // ledger AssetType::Base
    Erc20(Address),         // ledger AssetType::TokenAddress
    Erc721(Address, U256),  // ledger AssetType::TokenAddressId
    Erc1155(Address, U256), // ledger AssetType::TokenAddressId
}

impl AssetKind {
    /// Short human label used in reports and history.
    fn label(&self) -> String {
        match self {
            AssetKind::Ether => "ETH".into(),
            AssetKind::Erc20(t) => format!("ERC20({:#x})", t),
            AssetKind::Erc721(t, id) => format!("ERC721({:#x}#{})", t, id),
            AssetKind::Erc1155(t, id) => format!("ERC1155({:#x}#{})", t, id),
        }
    }
}


// ===========================================================================
// 2. Portal configuration
//
// libcma decodes a deposit once you tell it which portal layout to expect, but
// it deliberately does NOT hardcode portal addresses — they differ per network
// and change over time, so choosing them is the application's responsibility.
// We read them from the environment, defaulting to the cartesi-cli local devnet
// portal addresses (`cartesi address-book`), and resolve an advance's caller to
// the matching deposit type. Override the defaults per network via env vars.
// ===========================================================================

struct PortalAddresses {
    ether: Address,
    erc20: Address,
    erc721: Address,
    erc1155_single: Address,
    erc1155_batch: Address,
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
            ether: addr("ETHER_PORTAL_ADDRESS", "0xA632c5c05812c6a6149B7af5C56117d1D2603828"),
            erc20: addr("ERC20_PORTAL_ADDRESS", "0xACA6586A0Cf05bD831f2501E7B4aea550dA6562D"),
            erc721: addr("ERC721_PORTAL_ADDRESS", "0x9E8851dadb2b77103928518846c4678d48b5e371"),
            erc1155_single: addr(
                "ERC1155_SINGLE_PORTAL_ADDRESS",
                "0x18558398Dd1a8cE20956287a4Da7B76aE7A96662",
            ),
            erc1155_batch: addr(
                "ERC1155_BATCH_PORTAL_ADDRESS",
                "0xe246Abb974B307490d9C6932F48EbE79de72338A",
            ),
        }
    }

    /// Map an advance's caller to the deposit layout libcma should expect. A
    /// caller that is not a known portal is a user input we let libcma
    /// auto-decode (a withdrawal/transfer selector, or an application command).
    fn deposit_req_type(&self, sender: Address) -> CmaParserInputType {
        if sender == self.ether {
            CmaParserInputType::CmaParserInputTypeEtherDeposit
        } else if sender == self.erc20 {
            CmaParserInputType::CmaParserInputTypeErc20Deposit
        } else if sender == self.erc721 {
            CmaParserInputType::CmaParserInputTypeErc721Deposit
        } else if sender == self.erc1155_single {
            CmaParserInputType::CmaParserInputTypeErc1155SingleDeposit
        } else if sender == self.erc1155_batch {
            CmaParserInputType::CmaParserInputTypeErc1155BatchDeposit
        } else {
            CmaParserInputType::CmaParserInputTypeAuto
        }
    }
}

// ===========================================================================
// 3. Application state
//
// The ledger (libcma) plus the application's own registry and history.
// ===========================================================================

/// Application registration record.
struct User {
    nickname: String,
    registered_at_block: u64,
}

/// One line in the activity log — pure application bookkeeping. The
/// authoritative balances always come from the ledger.
struct Activity {
    kind: &'static str, // "deposit" | "withdrawal" | "transfer_out" | "transfer_in"
    account: Address,
    asset: String,
    amount: String,
    counterparty: Option<Address>,
    block_number: u64,
}

struct WalletApp {
    ledger: Ledger,
    portals: PortalAddresses,
    users: HashMap<Address, User>,
    history: Vec<Activity>,
    // ERC-721 and ERC-1155 both arrive as (token, token_id) and share the
    // ledger's TokenAddressId asset, so an inspect query can't tell them apart.
    // We remember the concrete kind each token-with-id asset was seen as on a
    // write path and resolve queries against it.
    asset_kinds: HashMap<(Address, U256), AssetKind>,
    // Context for the input currently being processed, refreshed each advance.
    app_address: Address, // our own rollup address; needed for 721/1155 vouchers
    block_number: u64,
}

impl WalletApp {
    fn new() -> Result<Self, String> {
        Ok(Self {
            ledger: Self::open_ledger()?,
            portals: PortalAddresses::from_env(),
            users: HashMap::new(),
            history: Vec::new(),
            asset_kinds: HashMap::new(),
            app_address: Address::zero(),
            block_number: 0,
        })
    }

    /// Open the libcma ledger.
    ///
    /// On the Cartesi machine (riscv64) the ledger is backed by the raw `accounts` flash
    /// drive at `/dev/pmem1`, so every balance persists in the machine state and is provable
    /// on-chain for emergency withdrawal. On the host (the libcma `native` mock) it is
    /// purely in-memory, which is all the host type-check / tests need.
    fn open_ledger() -> Result<Ledger, String> {
        #[allow(unused_mut)]
        let mut ledger = Ledger::new().map_err(|e| format!("ledger init failed: {:?}", e))?;
        #[cfg(target_arch = "riscv64")]
        {
            use libcma_binding_rust::ledger::{LedgerFileConfig, LedgerMemoryMode};
            // The `accounts` drive is a raw 4 MiB flash drive (see cartesi.toml).
            ledger
                .init_from_file(
                    "/dev/pmem1",
                    LedgerFileConfig {
                        mode: LedgerMemoryMode::CreateOnly,
                        offset: 0,
                        memory_length: 4 * 1024 * 1024,
                        max_accounts: 4096,
                        max_assets: 256,
                        max_balances: 4096,
                    },
                )
                .map_err(|e| format!("accounts-drive ledger init failed: {:?}", e))?;
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

    fn log(&mut self, kind: &'static str, account: Address, asset: &str, amount: U256, cp: Option<Address>) {
        self.history.push(Activity {
            kind,
            account,
            asset: asset.to_string(),
            amount: amount.to_string(),
            counterparty: cp,
            block_number: self.block_number,
        });
    }

    // --- ledger glue (libcma owns all balances) ----------------------------
    //
    // libcma identifies assets and accounts by small integer ids. You ask the
    // ledger for an id (creating the entry on first use), then deposit/withdraw
    // against it. These two helpers wrap that lookup for every asset class.

    /// Asset id, creating the asset on first use (write paths).
    fn asset(&mut self, kind: &AssetKind) -> Result<LedgerAssetId, String> {
        // Remember the concrete kind of token-with-id assets so later inspect
        // queries (which only carry token+id) can tell ERC-721 from ERC-1155.
        if let AssetKind::Erc721(t, id) | AssetKind::Erc1155(t, id) = kind {
            self.asset_kinds.insert((*t, *id), kind.clone());
        }
        match kind {
            AssetKind::Ether => self.ledger.retrieve_ether_assets(),
            AssetKind::Erc20(t) => self.ledger.retrieve_erc20_asset_via_address(*t),
            AssetKind::Erc721(t, id) | AssetKind::Erc1155(t, id) => {
                self.ledger.retrieve_erc721_assets_via_address(*t, *id)
            }
        }
        .map_err(|e| format!("retrieve asset: {:?}", e))
    }

    /// Account id, creating the account on first use (write paths).
    fn account(&mut self, address: Address) -> Result<LedgerAccountId, String> {
        self.ledger
            .retrieve_account_via_address(address)
            .map_err(|e| format!("retrieve account: {:?}", e))
    }

    // --- deposits: always accepted, libcma credits the ledger --------------

    fn deposit(&mut self, kind: AssetKind, account: Address, amount: U256) -> Result<(), String> {
        let asset = self.asset(&kind)?;
        let account_id = self.account(account)?;
        self.ledger
            .deposit(asset, account_id, amount)
            .map_err(|e| format!("deposit: {:?}", e))?;
        self.log("deposit", account, &kind.label(), amount, None);
        Ok(())
    }

    // --- withdrawals: registration-gated; libcma debits then builds voucher -

    /// Debit the ledger for a single asset (the shared step of every
    /// withdrawal). Fails if the account is unregistered or under-funded.
    fn debit(&mut self, kind: &AssetKind, receiver: Address, amount: U256) -> Result<(), String> {
        self.require_registered(receiver)?;
        let asset = self.asset(kind)?;
        let account_id = self.account(receiver)?;
        self.ledger
            .withdraw(asset, account_id, amount)
            .map_err(|e| format!("withdraw: {:?}", e))?;
        self.log("withdrawal", receiver, &kind.label(), amount, None);
        Ok(())
    }

    fn withdraw(
        &mut self,
        kind: AssetKind,
        receiver: Address,
        amount: U256,
        exec_data: Bytes,
    ) -> Result<CmaVoucher, String> {
        self.debit(&kind, receiver, amount)?;
        // libcma turns the request into a ready-to-emit on-chain voucher.
        withdrawal_voucher(&kind, receiver, amount, exec_data, self.app_address)
    }

    // --- transfers: internal ledger move between two registered accounts ----

    fn transfer(&mut self, kind: AssetKind, from: Address, to: Address, amount: U256) -> Result<(), String> {
        self.require_registered(from)?;
        self.require_registered(to)?;
        let asset = self.asset(&kind)?;
        let from_id = self.account(from)?;
        let to_id = self.account(to)?;
        self.ledger
            .transfer(asset, from_id, to_id, amount)
            .map_err(|e| format!("transfer: {:?}", e))?;
        let label = kind.label();
        self.log("transfer_out", from, &label, amount, Some(to));
        self.log("transfer_in", to, &label, amount, Some(from));
        Ok(())
    }

    // --- read-only views for inspects (never create ledger entries) ---------

    /// Resolve an inspect query's `(token, token_id)` to an [`AssetKind`]:
    /// zero token → Ether, token + zero id → ERC-20. A token with an id is
    /// either ERC-721 or ERC-1155, which the query alone can't distinguish, so
    /// we use the kind recorded when the asset was first seen on a write path,
    /// defaulting to ERC-1155 for assets we have never touched.
    fn asset_from_query(&self, token: Address, token_id: U256) -> AssetKind {
        if token.is_zero() {
            AssetKind::Ether
        } else if token_id.is_zero() {
            AssetKind::Erc20(token)
        } else {
            self.asset_kinds
                .get(&(token, token_id))
                .cloned()
                .unwrap_or_else(|| AssetKind::Erc1155(token, token_id))
        }
    }

    fn balance(&mut self, kind: &AssetKind, account: Address) -> Result<U256, String> {
        let asset = match self.asset_find(kind) {
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

    fn total_supply(&mut self, kind: &AssetKind) -> Result<U256, String> {
        match self.asset_find(kind) {
            Some(id) => self
                .ledger
                .get_total_supply(id)
                .map_err(|e| format!("get_total_supply: {:?}", e)),
            None => Ok(U256::zero()),
        }
    }

    /// Look up an asset id without creating it (read paths).
    fn asset_find(&mut self, kind: &AssetKind) -> Option<LedgerAssetId> {
        let (token, id, ty) = match kind {
            AssetKind::Ether => (None, None, AssetType::Base),
            AssetKind::Erc20(t) => (Some(*t), None, AssetType::TokenAddress),
            AssetKind::Erc721(t, id) | AssetKind::Erc1155(t, id) => {
                (Some(*t), Some(*id), AssetType::TokenAddressId)
            }
        };
        self.ledger
            .retrieve_asset(None, token, id, ty, RetrieveOperation::Find)
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
// 3. Withdrawal voucher building (libcma `cma_encode_voucher`)
//
// One place that turns "release these funds" into the exact destination/value/
// payload the Cartesi voucher needs. Batch is separate as it carries vectors.
// ===========================================================================

fn withdrawal_voucher(
    kind: &AssetKind,
    receiver: Address,
    amount: U256,
    exec_data: Bytes,
    app_address: Address,
) -> Result<CmaVoucher, String> {
    let (voucher_type, fields) = match kind {
        AssetKind::Ether => (
            CmaParserVoucherType::CmaParserVoucherTypeEther,
            CmaVoucherFieldType::EtherVoucherFields(CmaParserEtherVoucherFields { amount, receiver }),
        ),
        AssetKind::Erc20(token) => (
            CmaParserVoucherType::CmaParserVoucherTypeErc20,
            CmaVoucherFieldType::Erc20VoucherFields(CmaParserErc20VoucherFields {
                token: *token,
                receiver,
                amount,
            }),
        ),
        AssetKind::Erc721(token, id) => (
            CmaParserVoucherType::CmaParserVoucherTypeErc721,
            CmaVoucherFieldType::Erc721VoucherFields(CmaParserErc721VoucherFields {
                token: *token,
                token_id: *id,
                receiver,
                application_address: app_address,
            }),
        ),
        AssetKind::Erc1155(token, id) => (
            CmaParserVoucherType::CmaParserVoucherTypeErc1155Single,
            CmaVoucherFieldType::Erc1155SingleVoucherFields(CmaParserErc1155SingleVoucherFields {
                token: *token,
                receiver,
                token_id: *id,
                amount,
                exec_layer_data: exec_data,
            }),
        ),
    };
    cma_encode_voucher(voucher_type, Some(app_address), fields).map_err(|e| e.message())
}

fn batch_withdrawal_voucher(
    token: Address,
    receiver: Address,
    token_ids: Vec<U256>,
    amounts: Vec<U256>,
    exec_data: Bytes,
    app_address: Address,
) -> Result<CmaVoucher, String> {
    cma_encode_voucher(
        CmaParserVoucherType::CmaParserVoucherTypeErc1155Batch,
        Some(app_address),
        CmaVoucherFieldType::Erc1155BatchVoucherFields(CmaParserErc1155BatchVoucherFields {
            token,
            receiver,
            token_ids,
            amounts,
            exec_layer_data: exec_data,
        }),
    )
    .map_err(|e| e.message())
}

// ===========================================================================
// 4. Advance handling (deposits, withdrawals, transfers, registration)
// ===========================================================================

async fn handle_advance(app: &mut WalletApp, rollup: &mut Rollup) -> Result<bool, Box<dyn std::error::Error>> {
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
/// human-readable result message (or error).
fn run_advance(
    app: &mut WalletApp,
    rollup: &mut Rollup,
    advance: &Advance,
    decoded: CmaParserInput,
) -> Result<String, String> {
    use CmaParserInputData as In;
    match decoded.input {
        // ----- Deposits: libcma credits the ledger -------------------------
        In::EtherDeposit(d) => deposit(app, AssetKind::Ether, d.sender, d.amount),
        In::Erc20Deposit(d) => deposit(app, AssetKind::Erc20(d.token), d.sender, d.amount),
        In::Erc721Deposit(d) => deposit(app, AssetKind::Erc721(d.token, d.token_id), d.sender, U256::one()),
        In::Erc1155SingleDeposit(d) => {
            deposit(app, AssetKind::Erc1155(d.token, d.token_id), d.sender, d.amount)
        }
        In::Erc1155BatchDeposit(d) => {
            for (id, amount) in d.token_ids.iter().zip(&d.amounts) {
                app.deposit(AssetKind::Erc1155(d.token, *id), d.sender, *amount)?;
            }
            Ok(format!("credited {} token type(s) to {:#x}", d.token_ids.len(), d.sender))
        }

        // ----- Withdrawals: libcma debits, then we emit the voucher --------
        In::EtherWithdrawal(w) => withdraw(app, rollup, AssetKind::Ether, w.receiver, w.amount, &w.exec_layer_data),
        In::Erc20Withdrawal(w) => {
            withdraw(app, rollup, AssetKind::Erc20(w.token), w.receiver, w.amount, &w.exec_layer_data)
        }
        In::Erc721Withdrawal(w) => withdraw(
            app,
            rollup,
            AssetKind::Erc721(w.token, w.token_id),
            w.receiver,
            U256::one(),
            &w.exec_layer_data,
        ),
        In::Erc1155SingleWithdrawal(w) => withdraw(
            app,
            rollup,
            AssetKind::Erc1155(w.token, w.token_id),
            w.receiver,
            w.amount,
            &w.exec_layer_data,
        ),
        In::Erc1155BatchWithdrawal(w) => {
            // Debit each (id, amount), then release the whole batch in one voucher.
            for (id, amount) in w.token_ids.iter().zip(&w.amounts) {
                app.debit(&AssetKind::Erc1155(w.token, *id), w.receiver, *amount)?;
            }
            let voucher = batch_withdrawal_voucher(
                w.token,
                w.receiver,
                w.token_ids,
                w.amounts,
                exec_to_bytes(&w.exec_layer_data),
                app.app_address,
            )?;
            emit_voucher(rollup, &voucher)?;
            Ok(format!("withdrew batch to {:#x}; voucher -> {}", w.receiver, voucher.destination))
        }

        // ----- Transfers: internal ledger moves ----------------------------
        In::EtherTransfer(t) => transfer(app, advance, AssetKind::Ether, t.receiver, t.amount),
        In::Erc20Transfer(t) => transfer(app, advance, AssetKind::Erc20(t.token), t.receiver, t.amount),
        In::Erc721Transfer(t) => {
            transfer(app, advance, AssetKind::Erc721(t.token, t.token_id), t.receiver, U256::one())
        }
        In::Erc1155SingleTransfer(t) => {
            transfer(app, advance, AssetKind::Erc1155(t.token, t.token_id), t.receiver, t.amount)
        }
        In::Erc1155BatchTransfer(t) => {
            let from = sender(advance)?;
            let to = u256_to_address(t.receiver);
            for (id, amount) in t.token_ids.iter().zip(&t.amounts) {
                app.transfer(AssetKind::Erc1155(t.token, *id), from, to, *amount)?;
            }
            Ok(format!("transferred batch from {:#x} to {:#x}", from, to))
        }

        // ----- Application command (e.g. registration) ---------------------
        In::Unidentified(_) => app_command(app, advance),

        // Balance/Supply are inspect-only.
        In::Balance(_) | In::Supply(_) => Err("balance/supply are inspect queries, not advances".into()),
    }
}

fn deposit(app: &mut WalletApp, kind: AssetKind, account: Address, amount: U256) -> Result<String, String> {
    let label = kind.label();
    app.deposit(kind, account, amount)?;
    Ok(format!("credited {} {} to {:#x}", amount, label, account))
}

fn withdraw(
    app: &mut WalletApp,
    rollup: &mut Rollup,
    kind: AssetKind,
    receiver: Address,
    amount: U256,
    exec_hex: &str,
) -> Result<String, String> {
    let label = kind.label();
    let voucher = app.withdraw(kind, receiver, amount, exec_to_bytes(exec_hex))?;
    emit_voucher(rollup, &voucher)?;
    Ok(format!("withdrew {} {} to {:#x}; voucher -> {}", amount, label, receiver, voucher.destination))
}

fn transfer(
    app: &mut WalletApp,
    advance: &Advance,
    kind: AssetKind,
    receiver: U256,
    amount: U256,
) -> Result<String, String> {
    let from = sender(advance)?;
    let to = u256_to_address(receiver);
    let label = kind.label();
    app.transfer(kind, from, to, amount)?;
    Ok(format!("transferred {} {} from {:#x} to {:#x}", amount, label, from, to))
}

/// Handle a raw payload libcma did not recognise: the demo treats it as the
/// UTF-8 JSON command `{"method":"register","nickname":"alice"}`.
fn app_command(app: &mut WalletApp, advance: &Advance) -> Result<String, String> {
    let from = sender(advance)?;
    let text = decode_utf8(&advance.payload).ok_or("payload is not a known operation or UTF-8 command")?;
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
// 5. Inspect handling (balances, supply, users, history)
// ===========================================================================

async fn handle_inspect(app: &mut WalletApp, rollup: &mut Rollup) -> Result<bool, Box<dyn std::error::Error>> {
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
    let account = u256_to_address(q.account);
    let kind = app.asset_from_query(q.token, q.token_id);
    match app.balance(&kind, account) {
        Ok(balance) => report(rollup, object! {
            "query" => "balance",
            "account" => format!("{:#x}", account),
            "asset" => kind.label(),
            "balance" => balance.to_string(),
        }),
        Err(e) => fail(rollup, &e),
    }
}

fn supply_query(app: &mut WalletApp, rollup: &mut Rollup, q: CmaParserSupply) {
    let kind = app.asset_from_query(q.token, q.token_id);
    match app.total_supply(&kind) {
        Ok(supply) => report(rollup, object! {
            "query" => "total_supply",
            "asset" => kind.label(),
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
                    "asset" => a.asset.clone(),
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
// 6. Small helpers
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
// 7. Rollup run loop
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
