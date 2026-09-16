//! The core of the test environment: LiteSVM + the ProgramData fixture + actors +
//! the clock.

use litesvm::{types::TransactionResult, LiteSVM};
use solana_clock::Clock;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_loader_v3_interface::state::UpgradeableLoaderState;
use solana_message::{Message, VersionedMessage};
use solana_pubkey::Pubkey;
use solana_sdk_ids::bpf_loader_upgradeable;
use solana_signature::Signature;
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// SOL held by each actor when the environment is created: 100 SOL (enough for the
/// rent of every PDA/ATA and the fees of any test; "not enough lamports" must never
/// become a hidden cause of failures).
pub const ACTOR_LAMPORTS: u64 = 100_000_000_000;

/// The deterministic link between time and slot in `warp_to`: 2 slots per second of
/// timestamp (≈ mainnet's 400-500 ms per slot). The ratio itself is irrelevant to
/// the program's logic (it only reads `unix_timestamp`); what matters is determinism
/// and the strict monotonicity of the slot alongside the timestamp.
pub const SLOTS_PER_SECOND: u64 = 2;

/// How many user wallets are created by default.
const DEFAULT_USERS: usize = 3;

/// The path to the program artifact relative to this crate (the workspace target).
/// Public: the e2e crate deploys into surfpool the very file the gate guards.
pub const SALE_SO_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/deploy/sale.so");

/// The Sale program crate - the source for the artifact freshness gate. Public: the
/// e2e crate checks the IDL's freshness against it.
pub const SALE_CRATE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../programs/sale");

/// The path to the Burn program artifact (the workspace target).
const BURN_SO_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/deploy/burn.so");

/// The Burn program crate - the source for the artifact freshness gate.
const BURN_CRATE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../programs/burn");

/// The hint in the freshness gate's panic: `cargo test` rebuilds the program crates
/// as ordinary libraries, but the artifacts in `target/deploy` come only from
/// `make build`.
const REBUILD_HINT: &str = "run `make build`: cargo rebuilds the workspace crates, but the target/deploy artifacts are not rebuilt, and the suite would execute the previous version of the program";

/// The actors of the Sale role model (Arch §8) plus the user wallets. All of them
/// are funded with SOL ([`ACTOR_LAMPORTS`]).
pub struct Actors {
    pub owner: Keypair,
    pub admin: Keypair,
    pub operator: Keypair,
    pub users: Vec<Keypair>,
}

/// The test environment: LiteSVM with `sale.so` and `burn.so` executable, each in
/// the state "after a deploy through the BPF Loader Upgradeable" (the ProgramData
/// fixture of §9.1, a shared upgrade authority), the actors, and two mints (USDC
/// dec 6, $1WIN dec 8).
pub struct TestEnv {
    pub svm: LiteSVM,
    pub actors: Actors,
    /// The fee payer of the infrastructure transactions and the mint authority of both
    /// mints. In production the mint is external; owning the authority in tests is a
    /// deliberate simplification.
    pub payer: Keypair,
    /// The USDC mint, 6 decimals.
    pub usdc_mint: Pubkey,
    /// The $1WIN mint, 8 decimals.
    pub win_mint: Pubkey,
}

impl Default for TestEnv {
    fn default() -> Self {
        Self::new()
    }
}

impl TestEnv {
    /// The default environment: the upgrade authority of both programs is the `owner`
    /// actor (as in production after "deploy → SetAuthority to the Owner MS",
    /// Arch §9.1).
    pub fn new() -> Self {
        Self::build(None, true, false)
    }

    /// The same as [`TestEnv::new`], but the USDC mint has a freeze authority (the
    /// `payer`) - the shape of an administered stablecoin, whose issuer can freeze any
    /// token account of the mint ([`TestEnv::freeze_account`]).
    pub fn with_usdc_freeze_authority() -> Self {
        Self::build(None, true, true)
    }

    /// The same, but the upgrade authority (of both programs) is an arbitrary key (for
    /// the negatives: front-running `initialize` with a signer that does not hold the
    /// upgrade authority).
    pub fn with_upgrade_authority(authority: Pubkey) -> Self {
        Self::build(Some(authority), true, false)
    }

    /// The same as [`TestEnv::new`], but transaction signatures are not verified. This
    /// is needed to put an address with no private key (a PDA) in the signer slot: a
    /// genuine signature cannot be produced for it, and the program must reject such a
    /// wallet on its own. From the program's point of view the picture is the same as a
    /// multisig's `invoke_signed`: `is_signer == true` and the address is off-curve. The
    /// program still executes as the very same `sale.so` used everywhere else.
    ///
    /// Transactions here are sent through [`TestEnv::send_ixs_unsigned`]; the ordinary
    /// [`TestEnv::send_ixs`] with real signatures works too.
    pub fn without_sigverify() -> Self {
        Self::build(None, false, false)
    }

    fn build(upgrade_authority: Option<Pubkey>, sigverify: bool, usdc_freeze: bool) -> Self {
        let mut svm = LiteSVM::new().with_sigverify(sigverify);

        let owner = Keypair::new();
        let admin = Keypair::new();
        let operator = Keypair::new();
        let payer = Keypair::new();
        let users: Vec<Keypair> = (0..DEFAULT_USERS).map(|_| Keypair::new()).collect();

        for kp in users.iter().chain([&owner, &admin, &operator, &payer]) {
            svm.airdrop(&kp.pubkey(), ACTOR_LAMPORTS)
                .expect("actor airdrop");
        }

        let authority = upgrade_authority.unwrap_or_else(|| owner.pubkey());
        install_program(
            &mut svm,
            sale::id(),
            SALE_SO_PATH,
            SALE_CRATE_DIR,
            authority,
        );
        install_program(
            &mut svm,
            burn::id(),
            BURN_SO_PATH,
            BURN_CRATE_DIR,
            authority,
        );

        let usdc_freeze_authority = usdc_freeze.then(|| payer.pubkey());
        let usdc_mint = crate::token::create_mint(
            &mut svm,
            &payer,
            crate::USDC_DECIMALS,
            usdc_freeze_authority.as_ref(),
        );
        let win_mint = crate::token::create_mint(&mut svm, &payer, crate::WIN_DECIMALS, None);

        Self {
            svm,
            actors: Actors {
                owner,
                admin,
                operator,
                users,
            },
            payer,
            usdc_mint,
            win_mint,
        }
    }

    /// The canonical address of the Sale program's ProgramData account:
    /// `find_program_address([program_id], bpf_loader_upgradeable)`.
    pub fn programdata_address() -> Pubkey {
        Pubkey::find_program_address(&[sale::id().as_ref()], &bpf_loader_upgradeable::id()).0
    }

    /// The canonical address of the Burn program's ProgramData account.
    pub fn burn_programdata_address() -> Pubkey {
        Pubkey::find_program_address(&[burn::id().as_ref()], &bpf_loader_upgradeable::id()).0
    }

    /// A new funded user wallet (beyond the default ones).
    pub fn new_user(&mut self) -> Keypair {
        let user = Keypair::new();
        self.svm
            .airdrop(&user.pubkey(), ACTOR_LAMPORTS)
            .expect("new user airdrop");
        user
    }

    /// Deploys a program at the wallet's own address - what a wallet does with
    /// `allocate` + `assign` + a deploy: the account becomes executable, owned by the
    /// upgradeable loader, with its ProgramData at the canonical PDA (the ELF is the
    /// Sale artifact; any program would do). The runtime refuses any lamport change
    /// on an executable account.
    pub fn make_executable(&mut self, address: &Pubkey) {
        let elf = read_fresh_program_artifact(SALE_SO_PATH, SALE_CRATE_DIR);
        self.svm
            .add_program(*address, &elf)
            .expect("add_program(executable wallet)");
        assert!(
            self.svm
                .get_account(address)
                .is_some_and(|account| account.executable),
            "the wallet is executable"
        );
    }

    // ---------- Clock ----------

    /// The current Clock sysvar.
    pub fn clock(&self) -> Clock {
        self.svm.get_sysvar::<Clock>()
    }

    /// The current `unix_timestamp` (what the program's `current_phase()` reads).
    pub fn now(&self) -> i64 {
        self.clock().unix_timestamp
    }

    /// Warps time STRICTLY forward: `unix_timestamp = unix_ts`, and the slot grows
    /// consistently - by [`SLOTS_PER_SECOND`] for every second of timestamp. A single
    /// `set_sysvar(Clock)` updates both the sysvar account and LiteSVM's program-cache
    /// slot (`maybe_handle_sysvar_account` → CLOCK_ID).
    ///
    /// Panics when `unix_ts <= now`: rolling time back is forbidden by construction
    /// (state-machine-and-time: phase transitions are one-way; a test that "needs to go
    /// back" is masking a scenario bug).
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
        let blockhash = self.svm.latest_blockhash();
        let msg = Message::new_with_blockhash(ixs, Some(payer), &blockhash);
        let tx = VersionedTransaction::try_new(VersionedMessage::Legacy(msg), &signers.to_vec())
            .expect("transaction signatures");
        self.svm.send_transaction(tx)
    }

    /// Assembles and sends a transaction in which every signature slot holds a zero
    /// stub. That way an address without a private key can be declared a signer. It
    /// works only in a [`TestEnv::without_sigverify`] environment.
    #[allow(clippy::result_large_err)]
    pub fn send_ixs_unsigned(&mut self, ixs: &[Instruction], payer: &Pubkey) -> TransactionResult {
        assert!(
            !self.svm.get_sigverify(),
            "send_ixs_unsigned requires a TestEnv::without_sigverify environment"
        );
        let blockhash = self.svm.latest_blockhash();
        let msg = Message::new_with_blockhash(ixs, Some(payer), &blockhash);
        let signatures = vec![Signature::default(); msg.header.num_required_signatures as usize];
        let tx = VersionedTransaction {
            signatures,
            message: VersionedMessage::Legacy(msg),
        };
        self.svm.send_transaction(tx)
    }
}

/// Reads the program's `.so`, having first checked it against its crate's sources
/// for freshness. The single point where the artifact is read; it is called from the
/// [`TestEnv`] constructor, so an environment that has not passed the gate is never
/// assembled.
fn read_fresh_program_artifact(so_path: &str, crate_dir: &str) -> Vec<u8> {
    assert_fresh_vs_sources(
        Path::new(so_path),
        crate_dir,
        "the program artifact",
        REBUILD_HINT,
    );

    std::fs::read(so_path)
        .unwrap_or_else(|e| panic!("cannot read the program artifact {so_path}: {e}"))
}

/// The `sale.so` freshness gate without building an environment: the e2e crate
/// deploys the same file into surfpool, and a stale artifact would invalidate the
/// whole run.
pub fn assert_artifact_fresh() {
    assert_fresh_vs_sources(
        Path::new(SALE_SO_PATH),
        SALE_CRATE_DIR,
        "the program artifact",
        REBUILD_HINT,
    );
}

/// Checks the mtime of `artifact` against the newest source of the `crate_dir`
/// crate. `what` names the artifact in the panic text and `hint` says how to refresh
/// it.
///
/// Why the gate exists. The suite executes an artifact from disk; `cargo test`
/// rebuilds the program crate as an ordinary library, but `target/deploy/*.so` comes
/// only from `make build`. Without the check a source edit (a removed guard
/// included) never reaches the tests, and a green suite proves nothing.
///
/// Fail-closed: if the file is missing, or older than any `src/**/*.rs` or
/// `Cargo.toml` of the program crate, it panics rather than "running on whatever is
/// lying around".
///
/// Public for the sake of a second derived artifact - `target/idl/sale.json`: the
/// e2e crate reads the event discriminators from it, and a stale IDL would
/// invalidate that cross-check exactly as a stale `.so` invalidates the whole run.
///
/// The method's limits: it compares modification times, not contents, and only
/// across the program crate - a dependency version bump is not caught by the gate
/// (the rebuild itself catches that). The flip side: a manifest edit that does not
/// change the build graph triggers no rebuild - then `touch` any `.rs` of the crate
/// and retry.
pub fn assert_fresh_vs_sources(artifact: &Path, crate_dir: &str, what: &str, hint: &str) {
    let (newest_src, newest_src_mtime) = newest_source(crate_dir);
    let artifact_mtime = std::fs::metadata(artifact)
        .and_then(|meta| meta.modified())
        .unwrap_or_else(|e| panic!("{what} not found at {}: {e}. {hint}", artifact.display()));

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
/// `src/` (recursively). The path goes into the panic text so it is clear which edit
/// outran the build.
fn newest_source(crate_dir: &str) -> (PathBuf, SystemTime) {
    let crate_dir = PathBuf::from(crate_dir);
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
                "freshness gate: cannot read the source mtime {}: {e}",
                path.display()
            )
        })
}

/// Loads the `.so` of program `program_id` (through the freshness gate) and
/// reproduces the state "after a deploy through the BPF Loader Upgradeable" with the
/// given upgrade authority.
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
/// (which the Custom(101) smoke test proves: that code is born inside the program's
/// own dispatch).
fn install_program(
    svm: &mut LiteSVM,
    program_id: Pubkey,
    so_path: &str,
    crate_dir: &str,
    upgrade_authority: Pubkey,
) {
    let elf = read_fresh_program_artifact(so_path, crate_dir);
    svm.add_program(program_id, &elf).expect("add_program(.so)");

    let programdata_address =
        Pubkey::find_program_address(&[program_id.as_ref()], &bpf_loader_upgradeable::id()).0;
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
