//! A CU / tx-size benchmark for Vesting: measurements of LiteSVM's
//! `compute_units_consumed` and of the bincode size of each instruction's signed
//! legacy tx. The thresholds are a regression gate; the tables come from
//! `cargo test cu_bench -- --nocapture`.
//!
//! Searching for a canonical bump costs 1500 CU per iteration
//! (`sol_try_find_program_address` charges `create_program_address_units` for EVERY
//! bump seed it tries, scanning down from 255). Some of those searches depend on the
//! environment's keys: a recipient's ATA address derives from its pubkey and the
//! mint's address, and the environment's mint is a random key, so the claim
//! measurement would jitter in multiples of 1500 CU. That cause is removed: the
//! benchmark's recipient is chosen deterministically with an ATA bump of 255
//! ([`bench_recipient`]), and the measurements repeat across runs to within tens of CU
//! (a spread of up to 31 CU was observed on individual rows; the source of those units
//! was not isolated and is two orders of magnitude below the cost of one search
//! iteration). The ceilings are therefore tight (+11-13% over the measurement) - they
//! catch structural growth in an instruction (an extra CPI, a loop, a new account in
//! the context).
//!
//! Additionally, every row checks that the signed tx fits the 1232 B packet limit and
//! that the measurement fits the default per-instruction budget of 200,000 CU: a
//! client that did not set `SetComputeUnitLimit` must be able to execute any Vesting
//! instruction without adjusting the budget.
//!
//! ===== MEASURING THE SHARE OF THE ACTIVITY BUMP DERIVATION (question V0.9) =====
//! Every PDA of the program is declared as `bump` with no value, so Anchor derives the
//! canonical bump by scanning: iterations = 256 − bump, cost = iterations × 1500 CU.
//! For the activity's two PDAs - the record `[b"vesting", id]` and the vault
//! `[b"vault", id]` - the bump depends on `activity_id` and the program id, so the
//! variable part of the CU differs per activity;
//! `cu_bench_activity_bump_derivation_share` measures claim/cancel/close across 12
//! activities with DIFFERENT bump pairs. The `[b"config"]` singleton yields the same
//! iteration count in every instruction (bump 252, four iterations, 6,000 CU) and
//! enters the residual as a constant.
//!
//! What the measurement showed (activity_id 0-11, the program id from `declare_id!`):
//! - the pair's total iteration count ranged from 2 to 15, so an activity's derivation
//!   costs 3,000-22,500 CU;
//! - within a group the CU are linear in the iteration count, with a measured slope of
//!   1,500-1,501 CU (claim: 31,870 CU at 3 iterations → 49,887 CU at 15), and the
//!   "CU without derivation" residual coincides across a group's activities to within
//!   ~20 CU;
//! - the derivation's share of an instruction's CU: claim 14-45%, cancel_vesting
//!   9-26%, close_vesting 16-49% (the residuals without derivation: claim 27,370,
//!   cancel 29,256, close 22,701 CU).
//!
//! The residuals of all three groups repeat across runs: the recipient ATA's
//! derivation enters claim's residual as a constant (bump 255 by construction, one
//! iteration), and the cancel/close contexts contain no addresses derived from the
//! environment's keys at all. The price of an "expensive" ATA is visible in the runs
//! on random recipients, before the switch to a deterministic key: the warm claim
//! measurement shifted by 4,500 CU and the cold one by 13,500 CU, meaning the ATA's
//! address is derived several times inside the instruction (the context binding and
//! the account-creation CPI).
//!
//! The conclusion on caching the bump in state: do not introduce it. The group's worst
//! measurement is 49,887 CU (15 scan iterations), that is 25% of the default
//! instruction budget (200,000) and 4% of the transaction budget (1,400,000); even
//! with an "expensive" recipient ATA (shifts of up to 13,500 CU were observed) the
//! headroom is several-fold. A cache would save a few per cent of the budget, while
//! the address's canonicity would then rest on the discipline of its creation rather
//! than on a check: comparing against a cached bump accepts whatever value was
//! written.

use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_message::{Message, VersionedMessage};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use spl_associated_token_account_interface::address::get_associated_token_address_and_bump_seed;
use vesting_test_common::vesting_expected::vesting::{
    DAY_SECS, ECOSYSTEM_FULL_RELEASE_DAY, MARKETING_CLIFF_MONTHS, MARKETING_MONTHLY_BPS,
    MARKETING_TGE_BPS, V1_REFERENCE, V2_REFERENCE,
};
use vesting_test_common::{
    assert_funded_ix, cancel_vesting_ix, claim_ix, close_vesting_ix, create_vesting_ix,
    initialize_ix, recipient_ata, set_tge_ix, ActivitySpec, TestEnv, DEFAULT_TGE_OFFSET,
};

// ----- The execution environment's limits -----

/// The transaction packet limit (IPv6 MTU 1280 − 40 − 8; solana-packet).
const PACKET_DATA_SIZE: u64 = 1232;
/// The maximum CU budget of a single transaction.
const MAX_TX_CU: u64 = 1_400_000;
/// The default budget of ONE instruction when the client sets no explicit limit.
const DEFAULT_IX_CU: u64 = 200_000;
/// The cost of one bump-search iteration (`create_program_address_units`).
const BUMP_ITERATION_CU: u64 = 1500;

// ----- The CU ceilings (measured 2026-08-08 plus 11-13% for build drift) -----
//
// The measurements repeat from run to run to within tens of CU (the ATA bump search's
// jitter is removed by the deterministic recipient, [`bench_recipient`]), so the
// ceiling is kept tight: it catches structural growth in an instruction (an extra
// CPI, a loop, a new account in the context) rather than the environment's spread. A
// rebuild with changed code moves the measurement - and the ceiling is updated along
// with it.

const CEIL_INITIALIZE: u64 = 23_500; // measured 21,053
const CEIL_SET_TGE: u64 = 12_600; // measured 11,282
const CEIL_CREATE_VESTING: u64 = 38_800; // measured 34,781
const CEIL_ASSERT_FUNDED: u64 = 17_200; // measured 15,388
const CEIL_CANCEL: u64 = 37_700; // measured 33,756
const CEIL_CLOSE: u64 = 44_200; // measured 27,201 (empty), 39,577 (with a transfer)
/// A cold claim: plus the creation of the recipient's ATA inside the instruction.
const CEIL_CLAIM_COLD: u64 = 63_300; // measured 56,698
const CEIL_CLAIM_WARM: u64 = 36_300; // measured 32,515 (day 31), 32,073 (terminal)

// The ceilings for measuring the share of the activity bump derivation. They are
// separate from the ones above because the group is DELIBERATELY assembled from
// activities with differing record and vault bumps, and the worst scan case (15
// iterations, +19,500 CU over the best) is its subject rather than a regression. Every
// row's measurement goes through the same thresholds (`assert_budget`).

const CEIL_BUMP_STUDY_CANCEL: u64 = 44_400; // measured 32,256-39,756
const CEIL_BUMP_STUDY_CLOSE: u64 = 50_500; // measured 27,201-45,201
const CEIL_BUMP_STUDY_CLAIM_COLD: u64 = 62_700; // measured 56,150
const CEIL_BUMP_STUDY_CLAIM_WARM: u64 = 55_700; // measured 31,870-49,887

/// The day of the first monthly tranche of the "Ecosystem" schedule (a warm
/// claim).
const DAY_31: i64 = 31;

/// How many activities go into the derivation-share measurement: each has its own
/// `activity_id`, hence its own bump pair and its own scan iteration count.
const BUMP_STUDY_ACTIVITIES: u64 = 12;
/// This many of the set's first activities are cancelled before TGE (the
/// `cancel_vesting` measurement); the rest are claimed and closed after TGE.
const BUMP_STUDY_CANCELLED: u64 = 4;

/// The canonical ATA bump of the benchmark's recipient: the ATA address is derived
/// INSIDE the instruction (the `associated_token` context binding and the
/// account-creation CPI), and the price of that search enters the claim's CU. The
/// recipient is chosen so that the search costs exactly one iteration, otherwise the
/// claim measurement would jitter in multiples of [`BUMP_ITERATION_CU`] from run to
/// run: the environment's mint is a random key and its address is part of the ATA's
/// seeds.
const BENCH_ATA_BUMP: u8 = 255;

/// How many deterministic keys [`bench_recipient`] scans. A key qualifies when its
/// ATA's bump is 255; a rejected one means the address at bump 255 landed on the curve
/// (roughly half of all keys), so the allowance is more than generous. Exhausting it
/// is a panic: "take any of them" would silently bring the measurement's jitter back.
const BENCH_RECIPIENT_ATTEMPTS: u8 = 64;

/// SOL held by the benchmark's recipient: it signs the claim and pays the fee.
const BENCH_RECIPIENT_LAMPORTS: u64 = 100_000_000_000;

/// A single transaction's measurement: the execution CU and the signed tx's bincode
/// size.
struct Measured {
    cu: u64,
    tx_size: u64,
}

/// Sends ONE instruction and returns the measurement. The fee payer is `signers[0]`.
///
/// The transaction is assembled here rather than through `TestEnv::send_ixs`: the size
/// must come from the very tx instance that executed, or the size measurement would be
/// about a different transaction. The blockhash is rotated before every send -
/// otherwise a repeated measurement of an identical instruction would be filtered out
/// as `AlreadyProcessed` before execution.
fn measure(env: &mut TestEnv, ix: Instruction, signers: &[&Keypair], what: &str) -> Measured {
    env.expire_blockhash();
    let payer = signers[0].pubkey();
    let blockhash = env.svm.latest_blockhash();
    let msg = Message::new_with_blockhash(&[ix], Some(&payer), &blockhash);
    let tx = VersionedTransaction::try_new(VersionedMessage::Legacy(msg), &signers.to_vec())
        .expect("the bench tx's signatures");
    let tx_size = bincode::serialized_size(&tx).expect("the tx's bincode size");
    let meta = env
        .svm
        .send_transaction(tx)
        .unwrap_or_else(|e| panic!("{what}: the bench tx must pass: {e:?}"));
    Measured {
        cu: meta.compute_units_consumed,
        tx_size,
    }
}

/// The three thresholds of any measurement: this instruction's ceiling, the default
/// per-instruction budget and the transaction packet limit. It is called from EVERY
/// measurement in the file - from the table's rows [`row`] and from the
/// derivation-share groups alike.
fn assert_budget(name: &str, m: &Measured, ceiling: u64) {
    assert!(m.cu > 0, "{name}: a zero CU measurement");
    assert!(
        m.cu < ceiling,
        "{name}: CU {} ≥ the ceiling {ceiling}",
        m.cu
    );
    assert!(
        m.cu < DEFAULT_IX_CU,
        "{name}: CU {} does not fit the default instruction budget {DEFAULT_IX_CU} - a client would have to set SetComputeUnitLimit",
        m.cu
    );
    assert!(
        m.tx_size <= PACKET_DATA_SIZE,
        "{name}: tx {} B > the packet limit {PACKET_DATA_SIZE} B",
        m.tx_size
    );
}

/// Prints a table row and holds the measurement's thresholds.
fn row(name: &str, m: &Measured, ceiling: u64) {
    println!(
        "| {name:<44} | {:>6} CU | < {ceiling:>6} | {:>4} B |",
        m.cu, m.tx_size
    );
    assert_budget(name, m, ceiling);
}

/// The recipient of the benchmark's claims: a deterministic key whose ATA bump under
/// the environment's mint equals [`BENCH_ATA_BUMP`]. The keys are scanned in a fixed
/// order, so the choice is unambiguous for any mint and the cost of deriving the ATA
/// address inside a claim is always the same - the measurement repeats from run to run
/// and the ceilings can be kept tight.
fn bench_recipient(env: &mut TestEnv) -> Keypair {
    let mint = env.win_mint;
    for salt in 0..BENCH_RECIPIENT_ATTEMPTS {
        let candidate = Keypair::new_from_array([salt; 32]);
        let (_, bump) = get_associated_token_address_and_bump_seed(
            &candidate.pubkey(),
            &mint,
            &spl_associated_token_account_interface::program::id(),
            &spl_token_interface::id(),
        );
        if bump == BENCH_ATA_BUMP {
            env.svm
                .airdrop(&candidate.pubkey(), BENCH_RECIPIENT_LAMPORTS)
                .expect("an airdrop to the benchmark's recipient");
            return candidate;
        }
    }
    panic!(
        "none of the {BENCH_RECIPIENT_ATTEMPTS} deterministic keys is a recipient with ATA bump {BENCH_ATA_BUMP} - without one the claim measurement jitters in multiples of {BUMP_ITERATION_CU} CU"
    );
}

/// The "100% at TGE" bucket (Marketing, §10.5): a single claim at the TGE instant
/// draws down the whole reference amount, so the activity becomes eligible for
/// `close_vesting` immediately.
fn marketing_spec(reference_amount: u64) -> ActivitySpec {
    ActivitySpec {
        reference_amount,
        tge_bps: MARKETING_TGE_BPS,
        monthly_bps: MARKETING_MONTHLY_BPS,
        cliff_months: MARKETING_CLIFF_MONTHS,
        label: String::from("marketing"),
    }
}

/// The canonical bumps of an activity's two PDAs and the total number of scan
/// iterations for them.
///
/// The seeds mirror the schemes of `vesting_pda` and `vault_pda` (the crate's helpers
/// return only the address). This is part of the MEASUREMENT rather than a test
/// expectation: a seed mismatch here is not "a green test on its own formula" but an
/// immediate Anchor rejection (`ConstraintSeeds`) in the instructions below.
fn activity_bumps(activity_id: u64) -> ActivityBumps {
    let id_le = activity_id.to_le_bytes();
    let (_, vesting_bump) =
        Pubkey::find_program_address(&[b"vesting", id_le.as_ref()], &vesting::id());
    let (_, vault_bump) = Pubkey::find_program_address(&[b"vault", id_le.as_ref()], &vesting::id());
    ActivityBumps {
        vesting_bump,
        vault_bump,
        iterations: (256 - u64::from(vesting_bump)) + (256 - u64::from(vault_bump)),
    }
}

/// The bumps of an activity's record and its vault plus the price of deriving them, in
/// iterations.
struct ActivityBumps {
    vesting_bump: u8,
    vault_bump: u8,
    iterations: u64,
}

/// The withdrawal recipient's ATA (`config.withdrawal_authority` = the
/// `rent_collector` actor) under the environment's mint - the `beneficiary_token` slot
/// of a cancellation and a close.
fn beneficiary_token(env: &mut TestEnv) -> Pubkey {
    let withdrawal_authority = env.actors.rent_collector.pubkey();
    let mint = env.win_mint;
    env.create_ata(&withdrawal_authority, &mint)
}

// ===================================================================
// The config and an activity's lifecycle before TGE:
// initialize → set_tge → create_vesting → assert_funded → cancel_vesting
// ===================================================================

#[test]
fn cu_bench_config_and_activity_lifecycle() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let admin = env.actors.admin.insecure_clone();
    let stranger = env.actors.stranger.insecure_clone();
    let recipient = env.actors.recipients[0].pubkey();
    let mint = env.win_mint;
    let params = env.valid_init_params(DEFAULT_TGE_OFFSET);

    println!("== CU/size: the config and an activity before TGE ==");

    let m = measure(
        &mut env,
        initialize_ix(owner.pubkey(), &params),
        &[&owner],
        "initialize",
    );
    row("initialize (creating the config)", &m, CEIL_INITIALIZE);

    // The TGE date is moved a day forward - it has not arrived yet.
    let m = measure(
        &mut env,
        set_tge_ix(admin.pubkey(), params.tge_ts + DAY_SECS),
        &[&admin],
        "set_tge",
    );
    row("set_tge", &m, CEIL_SET_TGE);

    // Two inits in one context (the activity's record plus its vault through a
    // token-account creation CPI).
    let id = env.next_activity_id();
    let m = measure(
        &mut env,
        create_vesting_ix(
            owner.pubkey(),
            recipient,
            id,
            mint,
            &ActivitySpec::valid(V2_REFERENCE),
        ),
        &[&owner],
        "create_vesting",
    );
    row("create_vesting (2 init + CPI)", &m, CEIL_CREATE_VESTING);

    // Permissionless: the instruction has no signature and anyone pays for the
    // transaction.
    env.fund_vault(id, V2_REFERENCE);
    let m = measure(
        &mut env,
        assert_funded_ix(id, mint),
        &[&stranger],
        "assert_funded",
    );
    row("assert_funded (with no signer)", &m, CEIL_ASSERT_FUNDED);

    // A cancellation before TGE: the vault is non-empty → a transfer of the remainder
    // plus a close (two CPIs).
    let beneficiary = beneficiary_token(&mut env);
    let rent_collector = env.actors.rent_collector.pubkey();
    let m = measure(
        &mut env,
        cancel_vesting_ix(owner.pubkey(), id, beneficiary, rent_collector, mint),
        &[&owner],
        "cancel_vesting",
    );
    row("cancel_vesting (remainder + close)", &m, CEIL_CANCEL);

    // A measurement is only about the instruction when its effect actually happened: the
    // benchmark sends REAL instructions, and each must be executed on the merits rather
    // than merely "not fail" (tests-bind-to-real-code).
    assert!(
        !env.vesting_exists(id) && !env.vault_exists(id),
        "cancel_vesting must close both the activity's record and its vault"
    );
    assert_eq!(
        env.token_balance(&beneficiary),
        V2_REFERENCE,
        "the vault's remainder must go to the withdrawal recipient's account"
    );
}

// ===================================================================
// The claim: cold (the ATA is created inside) and warm (the ATA already exists);
// close_vesting in both branches - an empty vault and a remainder beyond the
// reference amount.
// ===================================================================

#[test]
fn cu_bench_claim_cold_warm_and_close() {
    let mut env = TestEnv::new();
    let params = env.init_default(DEFAULT_TGE_OFFSET);
    let owner = env.actors.owner.insecure_clone();
    let recipient = bench_recipient(&mut env);
    let mint = env.win_mint;

    // A - the "Ecosystem" schedule (10% at TGE, 5% per month): a cold claim at TGE,
    // a warm one on day 31, and the terminal one on the full-release day.
    let id_a = env.create_activity_for(&recipient.pubkey(), &ActivitySpec::valid(V2_REFERENCE));
    env.fund_vault(id_a, V2_REFERENCE);
    // B - "100% at TGE", funded BEYOND the reference amount: after the terminal claim a
    // surplus remains in the vault and the close takes the branch with a transfer.
    let surplus = 1_000;
    let id_b = env.create_activity_for(&recipient.pubkey(), &marketing_spec(V1_REFERENCE));
    env.fund_vault(id_b, V1_REFERENCE + surplus);

    println!("== CU/size: the claim and the close ==");

    assert!(
        !env.account_exists(&recipient_ata(&recipient.pubkey(), &mint)),
        "the cold claim's precondition: the recipient's ATA does not exist yet"
    );
    env.warp_to_day(params.tge_ts, 1);
    let m = measure(
        &mut env,
        claim_ix(recipient.pubkey(), id_a, recipient.pubkey(), mint),
        &[&recipient],
        "claim cold",
    );
    row("claim, cold (the ATA is created)", &m, CEIL_CLAIM_COLD);
    assert!(
        env.recipient_ata_exists(&recipient.pubkey()),
        "a cold claim must create the recipient's ATA - otherwise the measurement is about the warm branch"
    );

    env.warp_to_day(params.tge_ts, DAY_31);
    let m = measure(
        &mut env,
        claim_ix(recipient.pubkey(), id_a, recipient.pubkey(), mint),
        &[&recipient],
        "claim warm",
    );
    row("claim, warm (the ATA exists)", &m, CEIL_CLAIM_WARM);

    env.warp_to_day(params.tge_ts, ECOSYSTEM_FULL_RELEASE_DAY);
    let m = measure(
        &mut env,
        claim_ix(recipient.pubkey(), id_a, recipient.pubkey(), mint),
        &[&recipient],
        "claim terminal",
    );
    row(
        "claim, terminal (the reference amount's remainder)",
        &m,
        CEIL_CLAIM_WARM,
    );

    let beneficiary = beneficiary_token(&mut env);
    let rent_collector = env.actors.rent_collector.pubkey();

    // A was drawn down to exactly the reference amount → the vault is empty and the
    // transfer is skipped.
    assert_eq!(
        env.vault_balance(id_a),
        0,
        "precondition: activity A is fully drawn down and its vault is empty"
    );
    let m = measure(
        &mut env,
        close_vesting_ix(owner.pubkey(), id_a, beneficiary, rent_collector, mint),
        &[&owner],
        "close_vesting empty",
    );
    row("close_vesting (the vault is empty)", &m, CEIL_CLOSE);

    // B: the terminal claim, then a close with the surplus transferred.
    env.expire_blockhash();
    env.claim_as(&recipient, id_b, recipient.pubkey())
        .expect("B's terminal claim (100% at TGE)");
    assert_eq!(
        env.vault_balance(id_b),
        surplus,
        "precondition: a surplus beyond the reference amount remained in B's vault"
    );
    let m = measure(
        &mut env,
        close_vesting_ix(owner.pubkey(), id_b, beneficiary, rent_collector, mint),
        &[&owner],
        "close_vesting surplus",
    );
    row("close_vesting (remainder + close)", &m, CEIL_CLOSE);

    for id in [id_a, id_b] {
        assert!(
            !env.vesting_exists(id) && !env.vault_exists(id),
            "close_vesting must close both activity {id}'s record and its vault"
        );
    }
    assert_eq!(
        env.token_balance(&beneficiary),
        surplus,
        "exactly B's vault surplus must go to the withdrawal recipient's account (A's vault was empty)"
    );
}

// ===================================================================
// The share of the activity bump derivation in the CU of claim / cancel_vesting /
// close_vesting
// ===================================================================

/// A measurement row: the activity, the bumps of its record and vault, and the
/// instruction's CU.
struct BumpRow {
    activity_id: u64,
    bumps: ActivityBumps,
    cu: u64,
}

/// The spread of the "CU without derivation" residual within a group that the
/// measurement tolerates: up to 17 CU was observed (ids 7 and 8 against the other rows
/// of the claim group). The source of those CU was not isolated by the measurement,
/// but it is two orders of magnitude below the cost of one iteration (1,500 CU) and
/// does not move the derivation-share estimate. A threshold of half an iteration
/// catches the appearance of a SECOND variable quantity comparable in scale to the
/// scan and does not fire on those units.
const RESIDUAL_TOLERANCE_CU: u64 = BUMP_ITERATION_CU / 2;

/// The bounds of the measured "CU per scan iteration" slope: the expectation is
/// [`BUMP_ITERATION_CU`], with a ±100 CU tolerance for the same spread.
const SLOPE_BOUNDS: (u64, u64) = (BUMP_ITERATION_CU - 100, BUMP_ITERATION_CU + 100);

/// Prints a group of measurements and verifies that all the CU differences within the
/// group are explained by the bump scan: the residual `CU − iterations × 1500`
/// coincides across all rows to within [`RESIDUAL_TOLERANCE_CU`], and the slope
/// measured across the extreme points falls inside [`SLOPE_BOUNDS`]. Returns the
/// minimum residual - the instruction's CU without the activity PDA derivation.
fn report_group(name: &str, rows: &[BumpRow]) -> u64 {
    println!("-- {name} --");
    let mut residuals = Vec::new();
    for r in rows {
        let derivation = r.bumps.iterations * BUMP_ITERATION_CU;
        let residual = r.cu.checked_sub(derivation).unwrap_or_else(|| {
            panic!(
                "{name} id={}: the CU are below the scan's cost",
                r.activity_id
            )
        });
        println!(
            "| id {:>2} | record bump {:>3} / vault {:>3} | iterations {:>2} | {:>6} CU | derivation ~{:>5} CU ({:>2}%) |",
            r.activity_id,
            r.bumps.vesting_bump,
            r.bumps.vault_bump,
            r.bumps.iterations,
            r.cu,
            derivation,
            derivation * 100 / r.cu
        );
        residuals.push(residual);
    }

    let (min_res, max_res) = min_max(&residuals);
    assert!(
        max_res - min_res <= RESIDUAL_TOLERANCE_CU,
        "{name}: the CU residual net of the bump scan diverged by {} CU ({min_res}…{max_res}) - so the CU depend on more than the bump-search iteration count, and the derivation-share estimate is wrong",
        max_res - min_res
    );

    // The slope across the extreme points: a measurement, not a substitution of
    // BUMP_ITERATION_CU.
    let lo = rows
        .iter()
        .min_by_key(|r| r.bumps.iterations)
        .expect("the group is non-empty");
    let hi = rows
        .iter()
        .max_by_key(|r| r.bumps.iterations)
        .expect("the group is non-empty");
    if hi.bumps.iterations > lo.bumps.iterations {
        let slope = (hi.cu - lo.cu) / (hi.bumps.iterations - lo.bumps.iterations);
        println!(
            "the measured slope: {slope} CU per iteration (the expectation is {BUMP_ITERATION_CU}), the residual without derivation is {min_res} CU"
        );
        assert!(
            slope >= SLOPE_BOUNDS.0 && slope <= SLOPE_BOUNDS.1,
            "{name}: the slope of {slope} CU per iteration is outside the expected {SLOPE_BOUNDS:?} - either the cost of a bump-search iteration changed or the CU difference is explained by something other than the scan"
        );
    } else {
        // The bumps of both activity PDAs are determined by the program id: if they coincide
        // there is no spread and there is nothing to measure the slope on. Staying silent is
        // not an option - the run would otherwise look like a verified model.
        println!(
            "every activity of the group has {} scan iterations - the slope was not measured",
            lo.bumps.iterations
        );
    }
    min_res
}

fn min_max(values: &[u64]) -> (u64, u64) {
    let min = *values
        .iter()
        .min()
        .expect("a non-empty set of measurements");
    let max = *values
        .iter()
        .max()
        .expect("a non-empty set of measurements");
    (min, max)
}

#[test]
fn cu_bench_activity_bump_derivation_share() {
    let mut env = TestEnv::new();
    let params = env.init_default(DEFAULT_TGE_OFFSET);
    let owner = env.actors.owner.insecure_clone();
    let recipient = bench_recipient(&mut env);
    let mint = env.win_mint;

    // All the activities are "100% at TGE" for one and the same recipient: a single claim
    // draws down the whole reference amount and the ATA address is always the same, so
    // its derivation enters the CU as a constant and does not interfere with isolating
    // the activity's bump scan.
    let mut ids = Vec::new();
    for _ in 0..BUMP_STUDY_ACTIVITIES {
        let id = env.create_activity_for(&recipient.pubkey(), &marketing_spec(V1_REFERENCE));
        env.fund_vault(id, V1_REFERENCE);
        ids.push(id);
    }
    let beneficiary = beneficiary_token(&mut env);
    let rent_collector = env.actors.rent_collector.pubkey();

    println!("== The share of the activity bump derivation ==");

    // cancel_vesting - only before TGE.
    let cancelled: Vec<BumpRow> = ids[..BUMP_STUDY_CANCELLED as usize]
        .iter()
        .map(|&id| {
            let bumps = activity_bumps(id);
            let m = measure(
                &mut env,
                cancel_vesting_ix(owner.pubkey(), id, beneficiary, rent_collector, mint),
                &[&owner],
                "cancel_vesting (bump study)",
            );
            assert_budget("cancel_vesting (bump study)", &m, CEIL_BUMP_STUDY_CANCEL);
            assert!(
                !env.vesting_exists(id) && !env.vault_exists(id),
                "cancel_vesting {id}: the measurement must concern a cancellation that took place"
            );
            BumpRow {
                activity_id: id,
                bumps,
                cu: m.cu,
            }
        })
        .collect();
    let cancel_base = report_group("cancel_vesting", &cancelled);

    env.warp_to_day(params.tge_ts, 1);

    // The first claim creates the recipient's ATA (a cold one) - it does not go into the
    // model, or the one-off cost of creating the account would mix with the bump scan.
    let live = &ids[BUMP_STUDY_CANCELLED as usize..];
    let (&cold_id, warm_ids) = live
        .split_first()
        .expect("there are activities for the claims");
    let cold = measure(
        &mut env,
        claim_ix(recipient.pubkey(), cold_id, recipient.pubkey(), mint),
        &[&recipient],
        "claim cold (bump study)",
    );
    assert_budget("claim cold (bump study)", &cold, CEIL_BUMP_STUDY_CLAIM_COLD);
    let cold_bumps = activity_bumps(cold_id);
    println!(
        "-- claim, cold (does not go into the model) --\n| id {cold_id:>2} | record bump {:>3} / vault {:>3} | iterations {:>2} | {:>6} CU |",
        cold_bumps.vesting_bump, cold_bumps.vault_bump, cold_bumps.iterations, cold.cu
    );

    let claimed: Vec<BumpRow> = warm_ids
        .iter()
        .map(|&id| {
            let bumps = activity_bumps(id);
            let m = measure(
                &mut env,
                claim_ix(recipient.pubkey(), id, recipient.pubkey(), mint),
                &[&recipient],
                "claim (bump study)",
            );
            assert_budget("claim (bump study)", &m, CEIL_BUMP_STUDY_CLAIM_WARM);
            // "100% at TGE": a single claim must draw down the whole reference amount - otherwise
            // the measurement is about a payout of a different size of work.
            assert_eq!(
                env.vesting(id).claimed,
                V1_REFERENCE,
                "claim {id}: not the whole reference amount of the \"100% at TGE\" bucket was released"
            );
            BumpRow {
                activity_id: id,
                bumps,
                cu: m.cu,
            }
        })
        .collect();
    let claim_base = report_group("claim (warm)", &claimed);

    // Every claimed activity was drawn down to the reference amount in full → they are
    // closable.
    let closed: Vec<BumpRow> = warm_ids
        .iter()
        .map(|&id| {
            let bumps = activity_bumps(id);
            let m = measure(
                &mut env,
                close_vesting_ix(owner.pubkey(), id, beneficiary, rent_collector, mint),
                &[&owner],
                "close_vesting (bump study)",
            );
            assert_budget("close_vesting (bump study)", &m, CEIL_BUMP_STUDY_CLOSE);
            assert!(
                !env.vesting_exists(id) && !env.vault_exists(id),
                "close_vesting {id}: the measurement must concern a close that took place"
            );
            BumpRow {
                activity_id: id,
                bumps,
                cu: m.cu,
            }
        })
        .collect();
    let close_base = report_group("close_vesting", &closed);

    println!(
        "the CU residual without the activity's derivation: claim {claim_base}, cancel {cancel_base}, close {close_base}; the tx budget is {MAX_TX_CU} and the instruction default is {DEFAULT_IX_CU}"
    );
}
