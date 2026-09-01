//! The core of the test environment: LiteSVM + the ProgramData fixture + actors +
//! the clock. Loading the artifact is gated by the freshness check
//! ([`read_fresh_program_artifact`]).

// DUP: to be merged into svm-core (follow-up). The module is a copy of
// tests/sale/src/env.rs; the differences are the program under test
// (vesting.so), the set of actors and a single mint instead of two. Both suites
// carry the artifact freshness gate.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use litesvm::{types::TransactionResult, LiteSVM};
use solana_clock::Clock;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_loader_v3_interface::state::UpgradeableLoaderState;
use solana_message::{Message, VersionedMessage};
use solana_pubkey::Pubkey;
use solana_sdk_ids::bpf_loader_upgradeable;
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;

/// SOL held by each actor when the environment is created: 100 SOL (enough for the
/// rent of every PDA/ATA and the fees of any test; "not enough lamports" must never
/// become a hidden cause of failures).
pub const ACTOR_LAMPORTS: u64 = 100_000_000_000;

/// The deterministic link between time and slot in `warp_to`: 2 slots per second of
/// timestamp (≈ mainnet's 400-500 ms per slot). The ratio itself is irrelevant to
/// the program's logic (it only reads `unix_timestamp`); what matters is determinism
/// and the strict monotonicity of the slot alongside the timestamp.
///
/// Sale's ratio is kept deliberately: the furthest Vesting scenario is day 991 of
/// the Treasury bucket, 85,536,000 s from TGE, that is 171,072,000 slots in a single
/// `set_sysvar(Clock)`. Measured: ~48 µs per jump, with no cost growth as the delta
/// grows (LiteSVM stores the slot as a number rather than replaying it) - there is no
/// need to tune the ratio for distant dates.
pub const SLOTS_PER_SECOND: u64 = 2;

/// How many activity recipients are created by default: one per activity of the
/// typical scenario plus a "foreign recipient" for the negatives.
const DEFAULT_RECIPIENTS: usize = 3;

/// The path to the program artifact relative to this crate (the workspace target).
/// Public: the e2e crate deploys EXACTLY this file into surfpool, and the
/// [`assert_artifact_fresh`] freshness gate must check that same file rather than a
/// same-named one of its own.
pub const VESTING_SO_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../target/deploy/vesting.so"
);

/// The program crate: its `Cargo.toml` and all `src/**/*.rs` are the sources the
/// artifact's freshness is checked against.
const VESTING_CRATE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../programs/vesting");

/// The freshness gate's hint - one shared by all of its panics.
const REBUILD_HINT: &str = "run `make build`: cargo rebuilds the workspace crates, but target/deploy/vesting.so is not rebuilt, and the suite would execute the previous version of the program";

/// The actors of the Vesting role model (Arch §8) plus the activity recipients. All
/// of them are funded with SOL ([`ACTOR_LAMPORTS`]).
pub struct Actors {
    /// Owner: the roles, and creating / cancelling / closing activities.
    pub owner: Keypair,
    /// Admin: the TGE date and an activity's recipient (`set_tge`, `set_recipient`).
    pub admin: Keypair,
    /// Operator: claims on behalf of a recipient (the transfer still goes to the
    /// recipient).
    pub operator: Keypair,
    /// The activity recipients ([`DEFAULT_RECIPIENTS`] of them): `recipients[0..2]` are
    /// the "own" ones, and the last is used as the "foreign recipient" in the negatives.
    pub recipients: Vec<Keypair>,
    /// The token recipient when an activity is cancelled (`beneficiary_token` is its
    /// ATA).
    pub beneficiary: Keypair,
    /// The rent recipient of an activity's closed accounts (`rent_collector`).
    pub rent_collector: Keypair,
    /// A wallet holding no role at all - for the "called by anyone" negatives.
    pub stranger: Keypair,
}

/// The test environment: LiteSVM with `vesting.so` executable, in the state "after a
/// deploy through the BPF Loader Upgradeable" (the ProgramData fixture of Arch §9.1),
/// the actors and the mints.
pub struct TestEnv {
    pub svm: LiteSVM,
    pub actors: Actors,
    /// The fee payer of the infrastructure transactions and the mint authority of both
    /// mints. In production the mint is external; owning the authority in tests is a
    /// deliberate simplification.
    pub payer: Keypair,
    /// The $1WIN mint, 8 decimals - the one pinned in the config at `initialize`.
    pub win_mint: Pubkey,
    /// A "foreign" mint with THE SAME 8 decimals: the mint-binding negatives
    /// (`InvalidMint`) must not fire on a decimals mismatch - for
    /// `InvalidMintDecimals` there is [`TestEnv::create_mint_for_test`].
    pub foreign_mint: Pubkey,
}

impl Default for TestEnv {
    fn default() -> Self {
        Self::new()
    }
}

impl TestEnv {
    /// The default environment: the program's upgrade authority is the `owner` actor (as
    /// in production after "deploy → SetAuthority to the Owner MS", Arch §9.1).
    pub fn new() -> Self {
        Self::build(None)
    }

    /// The same, but the upgrade authority is an arbitrary key (for the `initialize`
    /// negatives: hijacking init with a signer that does not hold the upgrade authority).
    pub fn with_upgrade_authority(authority: Pubkey) -> Self {
        Self::build(Some(authority))
    }

    fn build(upgrade_authority: Option<Pubkey>) -> Self {
        let mut svm = LiteSVM::new();

        let owner = Keypair::new();
        let admin = Keypair::new();
        let operator = Keypair::new();
        let beneficiary = Keypair::new();
        let rent_collector = Keypair::new();
        let stranger = Keypair::new();
        let payer = Keypair::new();
        let recipients: Vec<Keypair> = (0..DEFAULT_RECIPIENTS).map(|_| Keypair::new()).collect();

        for kp in recipients.iter().chain([
            &owner,
            &admin,
            &operator,
            &beneficiary,
            &rent_collector,
            &stranger,
            &payer,
        ]) {
            svm.airdrop(&kp.pubkey(), ACTOR_LAMPORTS)
                .expect("actor airdrop");
        }

        install_vesting_program(
            &mut svm,
            upgrade_authority.unwrap_or_else(|| owner.pubkey()),
        );

        let win_mint = crate::token::create_mint(&mut svm, &payer, crate::WIN_DECIMALS);
        let foreign_mint = crate::token::create_mint(&mut svm, &payer, crate::WIN_DECIMALS);

        Self {
            svm,
            actors: Actors {
                owner,
                admin,
                operator,
                recipients,
                beneficiary,
                rent_collector,
                stranger,
            },
            payer,
            win_mint,
            foreign_mint,
        }
    }

    /// The canonical address of the Vesting program's ProgramData account:
    /// `find_program_address([program_id], bpf_loader_upgradeable)`.
    pub fn programdata_address() -> Pubkey {
        Pubkey::find_program_address(&[vesting::id().as_ref()], &bpf_loader_upgradeable::id()).0
    }

    /// Forges the ProgramData of a FOREIGN program: a BPF Loader Upgradeable account
    /// with valid metadata and the given upgrade authority, at an address unrelated to
    /// the Vesting program. Returns that address.
    ///
    /// It is unobtainable by honest means in this environment: there is no
    /// second upgradeable program here, and `add_program` places ProgramData only at its
    /// own program's canonical PDA. Needed by the negative: the signer does hold the
    /// upgrade right over the ProgramData that was passed, but that ProgramData does not
    /// belong to this program.
    pub fn forge_foreign_program_data(&mut self, upgrade_authority: Pubkey) -> Pubkey {
        let address = Pubkey::new_unique();
        let metadata_len = UpgradeableLoaderState::size_of_programdata_metadata();
        let mut data = vec![0u8; metadata_len];
        bincode::serialize_into(
            &mut data[..],
            &UpgradeableLoaderState::ProgramData {
                slot: self.clock().slot,
                upgrade_authority_address: Some(upgrade_authority),
            },
        )
        .expect("foreign ProgramData metadata serialization");

        let account = solana_account::Account {
            lamports: self.svm.minimum_balance_for_rent_exemption(metadata_len),
            data,
            owner: bpf_loader_upgradeable::id(),
            executable: false,
            rent_epoch: 0,
        };
        self.svm
            .set_account(address, account)
            .expect("set_account(foreign ProgramData)");
        address
    }

    /// A new funded wallet (beyond the default actors).
    pub fn new_wallet(&mut self) -> Keypair {
        let wallet = Keypair::new();
        self.svm
            .airdrop(&wallet.pubkey(), ACTOR_LAMPORTS)
            .expect("new wallet airdrop");
        wallet
    }

    /// Tops up `lamports` at an arbitrary address, including the address of a PDA that
    /// does not exist yet: a system transfer is not signed by the recipient, so the
    /// account comes into being empty (zero data, owned by the System Program).
    ///
    /// Such a state cannot be produced honestly in a test: every PDA of the
    /// program is created rent-exempt from the start by the program's own instructions,
    /// and "the lamports are already there but the account is not" is outside
    /// interference. Needed by the regression (rent griefing): pre-funding
    /// the predictable address of a future vault must not break `create_vesting` (Anchor
    /// takes the transfer-shortfall + allocate/assign path).
    pub fn prefund_lamports(&mut self, address: &Pubkey, lamports: u64) {
        self.svm
            .airdrop(address, lamports)
            .expect("airdrop to the address of a future account");
    }

    // ---------- Clock ----------

    /// The current Clock sysvar.
    pub fn clock(&self) -> Clock {
        self.svm.get_sysvar::<Clock>()
    }

    /// The current `unix_timestamp` (what the program's `vested()` reads).
    pub fn now(&self) -> i64 {
        self.clock().unix_timestamp
    }

    /// Warps time STRICTLY forward: `unix_timestamp = unix_ts`, and the slot grows
    /// consistently - by [`SLOTS_PER_SECOND`] for every second of timestamp. A single
    /// `set_sysvar(Clock)` updates both the sysvar account and LiteSVM's program-cache
    /// slot (`maybe_handle_sysvar_account` → CLOCK_ID).
    ///
    /// Panics when `unix_ts <= now`: rolling time back is forbidden by construction
    /// (state-machine-and-time: the TGE gate is one-way; a test that "needs to go back"
    /// is masking a scenario bug).
    pub fn warp_to(&mut self, unix_ts: i64) {
        let mut clock = self.clock();
        assert!(
            unix_ts > clock.unix_timestamp,
            "warp_to only moves forward: now = {}, target = {unix_ts}",
            clock.unix_timestamp
        );
        let delta_secs = unix_ts
            .checked_sub(clock.unix_timestamp)
            .and_then(|d| u64::try_from(d).ok())
            .expect("delta seconds fit in u64");
        clock.slot = delta_secs
            .checked_mul(SLOTS_PER_SECOND)
            .and_then(|s| s.checked_add(clock.slot))
            .expect("the slot does not overflow");
        clock.unix_timestamp = unix_ts;
        self.svm.set_sysvar(&clock);
    }

    /// Rotates latest_blockhash. A replay test MUST call this between two sends of a
    /// semantically identical transaction: without a fresh blockhash the repeated tx is
    /// byte-for-byte identical and LiteSVM's history filters it out as
    /// `AlreadyProcessed` BEFORE execution - the program's replay guard would then never
    /// run at all (see the `identical_tx_replay_*` smoke test).
    pub fn expire_blockhash(&mut self) {
        self.svm.expire_blockhash();
    }

    // ---------- Transactions ----------

    /// Assembles and sends a transaction: a legacy message with the current
    /// latest_blockhash, the fee payer `payer` and the signers `signers` (the fee payer
    /// must be among `signers`).
    /// result_large_err: we return the native litesvm::types::TransactionResult as is -
    /// boxing it would fork the litesvm interface for the sake of a test helper.
    #[allow(clippy::result_large_err)]
    pub fn send_ixs(
        &mut self,
        ixs: &[Instruction],
        payer: &Pubkey,
        signers: &[&Keypair],
    ) -> TransactionResult {
        let tx = self.build_tx(ixs, payer, signers);
        self.svm.send_transaction(tx)
    }

    /// Assembles a signed transaction the same way [`Self::send_ixs`] does (a legacy
    /// message with the current latest_blockhash) but does not send it.
    fn build_tx(
        &self,
        ixs: &[Instruction],
        payer: &Pubkey,
        signers: &[&Keypair],
    ) -> VersionedTransaction {
        let blockhash = self.svm.latest_blockhash();
        let msg = Message::new_with_blockhash(ixs, Some(payer), &blockhash);
        VersionedTransaction::try_new(VersionedMessage::Legacy(msg), &signers.to_vec())
            .expect("transaction signatures")
    }

    /// The serialized transaction's size in BYTES - against Solana's packet limit
    /// (1232 bytes). It is measured on the very transaction [`Self::send_ixs`] would
    /// send: the value does not depend on the blockhash (always 32 bytes) and matches
    /// the size on a real validator.
    ///
    /// Needed for the funding transaction (Arch §2): its length grows by three links per
    /// activity, and "it did not fit into the packet" must be caught by the suite, not at
    /// TGE.
    pub fn tx_size(&self, ixs: &[Instruction], payer: &Pubkey, signers: &[&Keypair]) -> usize {
        let tx = self.build_tx(ixs, payer, signers);
        bincode::serialize(&tx)
            .expect("transaction serialization")
            .len()
    }
}

/// Loads `vesting.so` and reproduces the state "after a deploy through the BPF Loader
/// Upgradeable" with the given upgrade authority.
///
/// What `litesvm 0.10::add_program` provides on its own (see its
/// `add_program_internal`): the program account (owner = bpf_loader_upgradeable,
/// executable = true, data = `UpgradeableLoaderState::Program { programdata_address }`),
/// the ProgramData at the canonical PDA (metadata + ELF) and the load into the
/// program cache. What it does NOT provide: it writes `upgrade_authority_address` as
/// `None`.
///
/// So the fixture writes back, through `set_account`, only the ProgramData metadata
/// (the first 45 bytes, `size_of_programdata_metadata()`):
/// `ProgramData { slot, upgrade_authority_address: Some(authority) }`, preserving the
/// ELF tail. The program cache is left untouched - the program stays executable
/// (which the Custom(101) smoke test proves: that code is born inside vesting.so's
/// own dispatch).
fn install_vesting_program(svm: &mut LiteSVM, upgrade_authority: Pubkey) {
    let elf = read_fresh_program_artifact();
    svm.add_program(vesting::id(), &elf)
        .expect("add_program(vesting.so)");

    let programdata_address = TestEnv::programdata_address();
    let mut programdata = svm
        .get_account(&programdata_address)
        .expect("ProgramData was created by add_program");

    let metadata_len = UpgradeableLoaderState::size_of_programdata_metadata();
    let slot = svm.get_sysvar::<Clock>().slot;
    bincode::serialize_into(
        &mut programdata.data[..metadata_len],
        &UpgradeableLoaderState::ProgramData {
            slot,
            upgrade_authority_address: Some(upgrade_authority),
        },
    )
    .expect("ProgramData metadata serialization");

    svm.set_account(programdata_address, programdata)
        .expect("set_account(ProgramData)");
}

/// Reads `vesting.so`, having first checked it against the program's sources for
/// freshness. It is the single point where the artifact is read, and it is called
/// from `install_vesting_program`, that is, from the [`TestEnv`] constructor: an
/// environment that has not passed the gate is never assembled.
fn read_fresh_program_artifact() -> Vec<u8> {
    assert_artifact_fresh();
    std::fs::read(VESTING_SO_PATH)
        .unwrap_or_else(|e| panic!("cannot read the program artifact {VESTING_SO_PATH}: {e}"))
}

/// The `target/deploy/vesting.so` freshness gate: it panics if the artifact is
/// missing or older than the program crate's sources.
///
/// Why the gate exists. The suite executes an artifact from disk; `cargo test`
/// rebuilds the `vesting` crate as an ordinary library, but
/// `target/deploy/vesting.so` comes only from `make build`. Without the check a
/// source edit (a removed guard included) never reaches the tests, and a green suite
/// proves nothing. The same holds for the e2e crate, which deploys this file into
/// surfpool - which is why the gate is public and called from both suites.
pub fn assert_artifact_fresh() {
    assert_fresh_vs_sources(
        Path::new(VESTING_SO_PATH),
        "the program artifact",
        REBUILD_HINT,
    );
}

/// Checks the mtime of `artifact` against the newest source of the program crate.
/// `what` names the artifact in the panic text and `hint` says how to refresh it.
///
/// Fail-closed: if the file is missing, or older than any
/// `programs/vesting/src/**/*.rs` or `programs/vesting/Cargo.toml`, it panics rather
/// than "running on whatever is lying around".
///
/// Public for the sake of a second derived artifact - `target/idl/vesting.json`: the
/// e2e crate reads the event discriminators from it, and a stale IDL would
/// invalidate that cross-check exactly as a stale `.so` invalidates the whole run.
///
/// The method's limits: it compares modification times, not contents, and only
/// across the program crate - a dependency version bump in `Cargo.lock` or in the
/// shared manifest is not caught by the gate (the rebuild itself catches that). The
/// flip side: a manifest edit that does not change the build graph (a comment, a
/// `touch`) triggers no cargo rebuild, and `make build` will not refresh the artifact
/// - then `touch` any `.rs` of the crate and rebuild; the hint is in the panic text.
pub fn assert_fresh_vs_sources(artifact: &Path, what: &str, hint: &str) {
    let (newest_src, newest_src_mtime) = newest_source();
    let artifact_mtime = std::fs::metadata(artifact)
        .and_then(|meta| meta.modified())
        .unwrap_or_else(|e| {
            panic!("{what} not found at {}: {e}. {hint}", artifact.display());
        });

    if artifact_mtime < newest_src_mtime {
        let lag = newest_src_mtime
            .duration_since(artifact_mtime)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        panic!(
            "{what} is {lag} s older than the sources - {hint}\n  file: {}\n  newer than it: {}\n  if the build did not refresh the artifact (a manifest edit does not change the build graph) - `touch` any .rs of the crate and retry",
            artifact.display(),
            newest_src.display()
        );
    }
}

/// The newest source of the program crate: `Cargo.toml` and every `*.rs` under
/// `src/` (recursively). The path is returned together with the mtime - it goes into
/// the panic text so it is clear which edit outran the build.
fn newest_source() -> (PathBuf, SystemTime) {
    let crate_dir = PathBuf::from(VESTING_CRATE_DIR);
    let manifest = crate_dir.join("Cargo.toml");
    let mut newest = (manifest.clone(), file_mtime(&manifest));
    visit_rust_sources(&crate_dir.join("src"), &mut newest);
    newest
}

fn visit_rust_sources(dir: &Path, newest: &mut (PathBuf, SystemTime)) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| {
        panic!(
            "freshness gate: cannot read the source directory {}: {e}",
            dir.display()
        )
    });
    for entry in entries {
        let path = entry
            .unwrap_or_else(|e| {
                panic!(
                    "freshness gate: cannot read the directory entry {}: {e}",
                    dir.display()
                )
            })
            .path();
        if path.is_dir() {
            visit_rust_sources(&path, newest);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let mtime = file_mtime(&path);
            if mtime > newest.1 {
                *newest = (path, mtime);
            }
        }
    }
}

fn file_mtime(path: &Path) -> SystemTime {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .unwrap_or_else(|e| {
            panic!(
                "freshness gate: cannot read the mtime of {}: {e}",
                path.display()
            )
        })
}
