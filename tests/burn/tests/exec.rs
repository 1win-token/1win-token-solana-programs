//! The `burn` instruction - the positive case (the accumulator windows, supply, the
//! event) plus the negative matrix: period limits, a zero amount, substituting the
//! accumulator, the vault or its authority, the token layer around the program,
//! SPL errors, overflow, and the lazy reset of the windows across day and week
//! rollovers.
//!
//! tests-bind-to-real-code: the real instruction → an assertion of the on-chain
//! effect (the `BurnCounters.day/week` windows, `Mint.supply`, the vault balance,
//! the `BurnExecuted` payload) or of a specific error code. Every expectation is a
//! hand-written literal, never a recomputation of the program's formulas.

use anchor_lang::error::{ErrorCode, ERROR_CODE_OFFSET};
use burn::errors::BurnError;
use burn::events::BurnExecuted;
use burn_tests::{
    burn_counters_pda, burn_ix, burn_ix_with_counters, burn_ix_with_vault_authority,
    burn_state_pda, spl_burn_ix, spl_transfer_ix, vault_authority_pda, vault_pda, BurnTestEnv,
};
use sale_test_common::{custom_error_code, decode_event, TestEnv};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use spl_token_interface::error::TokenError;

/// The scenarios' time base: 2,000,077,200 (2033-05-19 01:00:00 UTC).
/// The indices derived by hand:
///   23_149 × 86_400 = 2_000_073_600 ≤ base < 23_150 × 86_400 = 2_000_160_000
///     → day 23_149;
///   3_307 × 604_800 = 2_000_073_600 ≤ base < 3_308 × 604_800 = 2_000_678_400
///     → week 3_307.
/// 23_149 = 7 × 3_307 - the FIRST day of week 3_307: a warp of +86_400 changes the
/// day but stays within the same week (which the rollover tests need).
const BASE_TS: i64 = 2_000_077_200;
const DAY0: u64 = 23_149;
const WEEK0: u64 = 3_307;

/// BASE_TS + 86_400 = 2_000_163_600: day 23_150, week STILL 3_307
/// (2_000_163_600 < 2_000_678_400).
const DAY1_TS: i64 = 2_000_163_600;
const DAY1: u64 = 23_150;

/// The start of week 3_308: 3_308 × 604_800 = 2_000_678_400 - which is at the same
/// time the start of day 23_156 (23_156 × 86_400 = 2_000_678_400): BOTH indices
/// change relative to BASE_TS.
const WEEK1_TS: i64 = 2_000_678_400;
const WEEK1_DAY: u64 = 23_156;
const WEEK1: u64 = 3_308;

/// The burn custom error code (the Anchor offset 6000 plus the variant index).
fn err(variant: BurnError) -> u32 {
    ERROR_CODE_OFFSET + variant as u32
}

// We return the native litesvm TransactionResult as is (as `send_ixs` does).
#[allow(clippy::result_large_err)]
fn send(
    env: &mut TestEnv,
    ix: solana_instruction::Instruction,
    signer: &Keypair,
) -> litesvm::types::TransactionResult {
    env.send_ixs(&[ix], &signer.pubkey(), &[signer])
}

/// Mints `amount` units straight into the vault (created by initialize) and returns
/// its address. Anyone can pay into the vault; only the program can burn out of it.
fn fund_vault(env: &mut TestEnv, amount: u64) -> Pubkey {
    let mint = env.win_mint;
    let vault = vault_pda();
    env.mint_to(&mint, &vault, amount);
    vault
}

/// A $1WIN token account OUTSIDE the program: the ATA of `wallet` holding `amount`
/// units - a decoy for the vault-substitution negatives.
fn ata_with(env: &mut TestEnv, wallet: &Pubkey, amount: u64) -> Pubkey {
    let mint = env.win_mint;
    let ata = env.create_ata(wallet, &mint);
    env.mint_to(&mint, &ata, amount);
    ata
}

/// The standard fixture: warp to BASE_TS, init burn with the default limits (daily
/// 50e9, weekly 200e9), and the vault holding 100e9 units.
fn setup_default(env: &mut TestEnv) -> (Keypair, Pubkey) {
    env.warp_to(BASE_TS);
    env.init_default_burn();
    let operator = env.actors.operator.insecure_clone();
    let vault = fund_vault(env, 100_000_000_000);
    (operator, vault)
}

/// Asserts both accumulator windows: `day` / `week` = (index, burned) - the literals
/// are supplied by the caller.
fn assert_windows(env: &TestEnv, day: (u64, u64), week: (u64, u64), ctx: &str) {
    let c = env.burn_counters();
    assert_eq!(c.day.index, day.0, "{ctx}: day.index");
    assert_eq!(c.day.burned, day.1, "{ctx}: day.burned");
    assert_eq!(c.week.index, week.0, "{ctx}: week.index");
    assert_eq!(c.week.burned, week.1, "{ctx}: week.burned");
}

// ===================================================================
// POSITIVE: the accumulator windows, supply, the vault balance, the event; a second
// burn tops up both windows.
// ===================================================================

#[test]
fn burn_updates_windows_supply_and_emits_event() {
    let mut env = TestEnv::new();
    let (operator, kettle) = setup_default(&mut env);
    let win = env.win_mint;

    // The first burn: 120 tokens (12e9 units).
    let r = send(
        &mut env,
        burn_ix(operator.pubkey(), kettle, win, 12_000_000_000),
        &operator,
    );
    r.as_ref().expect("a burn within the limits passes");

    // Both windows were overwritten with the indices FROM Clock and the burned amount:
    // before the burn they held "period 0" from initialize - the first burn of a real
    // period reset them lazily.
    assert_windows(
        &env,
        (DAY0, 12_000_000_000),
        (WEEK0, 12_000_000_000),
        "after the first burn",
    );
    // Supply dropped by exactly the burned amount: 100e9 − 12e9 = 88e9.
    assert_eq!(
        env.mint_supply(&win),
        88_000_000_000,
        "supply after the burn"
    );
    // The vault balance dropped by the same amount.
    assert_eq!(
        env.token_balance(&kettle),
        88_000_000_000,
        "the vault after the burn"
    );
    // The event: the reference values are literals; the indices come from Clock.
    let ev: BurnExecuted = decode_event(&r).expect("the BurnExecuted event is emitted");
    assert_eq!(ev.day_index, 23_149, "event.day_index");
    assert_eq!(ev.week_index, 3_307, "event.week_index");
    assert_eq!(ev.amount, 12_000_000_000, "event.amount");
    assert_eq!(ev.day_total, 12_000_000_000, "event.day_total");
    assert_eq!(ev.week_total, 12_000_000_000, "event.week_total");

    // A second burn within the same period: 30 tokens → the windows are topped up to
    // 12e9 + 3e9 = 15e9 (the indices are idempotently the same).
    env.expire_blockhash();
    let r2 = send(
        &mut env,
        burn_ix(operator.pubkey(), kettle, win, 3_000_000_000),
        &operator,
    );
    r2.as_ref()
        .expect("a second burn within the same period passes");
    assert_windows(
        &env,
        (DAY0, 15_000_000_000),
        (WEEK0, 15_000_000_000),
        "after the second burn",
    );
    assert_eq!(
        env.mint_supply(&win),
        85_000_000_000,
        "supply after the second one"
    );
    let ev2: BurnExecuted = decode_event(&r2).expect("the second event");
    assert_eq!(
        ev2.day_total, 15_000_000_000,
        "event2.day_total is cumulative"
    );
    assert_eq!(
        ev2.week_total, 15_000_000_000,
        "event2.week_total is cumulative"
    );
}

// ===================================================================
// LIMITS: exactly at the limit passes; +1 above is rejected; a weekly limit below
// the daily one.
// ===================================================================

#[test]
fn burn_exactly_at_daily_limit_passes_then_one_more_rejected() {
    let mut env = TestEnv::new();
    let (operator, kettle) = setup_default(&mut env);
    let win = env.win_mint;

    // Exactly the daily limit (DEFAULT_DAILY_LIMIT = 50e9) - it passes.
    send(
        &mut env,
        burn_ix(operator.pubkey(), kettle, win, 50_000_000_000),
        &operator,
    )
    .expect("a burn exactly at the daily limit passes");
    assert_windows(
        &env,
        (DAY0, 50_000_000_000),
        (WEEK0, 50_000_000_000),
        "the windows equal the daily limit",
    );

    // +1 unit above the daily limit - DailyLimitExceeded (it is not clamped).
    env.expire_blockhash();
    let r = send(
        &mut env,
        burn_ix(operator.pubkey(), kettle, win, 1),
        &operator,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(BurnError::DailyLimitExceeded)),
        "the limit + 1 → DailyLimitExceeded: {r:?}"
    );
    // Nothing changed: the windows and supply are as after the first burn.
    assert_windows(
        &env,
        (DAY0, 50_000_000_000),
        (WEEK0, 50_000_000_000),
        "the windows were untouched by the revert",
    );
    assert_eq!(env.mint_supply(&win), 50_000_000_000, "supply is untouched");
}

#[test]
fn burn_weekly_limit_binds_when_daily_ok() {
    // A weekly limit BELOW the daily one (legal: init only requires > 0): daily 100
    // tokens (10e9), weekly 50 tokens (5e9).
    let mut env = TestEnv::new();
    env.warp_to(BASE_TS);
    env.init_burn_with_limits(10_000_000_000, 5_000_000_000);
    let operator = env.actors.operator.insecure_clone();
    let kettle = fund_vault(&mut env, 100_000_000_000);
    let win = env.win_mint;

    // 60 tokens: the daily side is fine (6e9 ≤ 10e9), the weekly one would be exceeded
    // (6e9 > 5e9) → WeeklyLimitExceeded.
    let r = send(
        &mut env,
        burn_ix(operator.pubkey(), kettle, win, 6_000_000_000),
        &operator,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(BurnError::WeeklyLimitExceeded)),
        "the weekly limit with the daily one fine → WeeklyLimitExceeded: {r:?}"
    );
    // The rejection comes BEFORE any mutation: both windows still hold the init values
    // {0, 0} - including the daily one, whose limit check would have passed (there is no
    // partial mutation).
    assert_windows(
        &env,
        (0, 0),
        (0, 0),
        "the windows were untouched by the rejection",
    );
    assert_eq!(
        env.mint_supply(&win),
        100_000_000_000,
        "supply is untouched"
    );

    // The positive counterpart in the same config: exactly the weekly limit passes.
    env.expire_blockhash();
    send(
        &mut env,
        burn_ix(operator.pubkey(), kettle, win, 5_000_000_000),
        &operator,
    )
    .expect("a burn exactly at the weekly limit passes");
    assert_windows(
        &env,
        (DAY0, 5_000_000_000),
        (WEEK0, 5_000_000_000),
        "the windows equal the weekly limit",
    );
}

// ===================================================================
// NEGATIVE: a zero amount, overflow, substituting the accumulator.
// ===================================================================

#[test]
fn burn_rejects_zero_amount() {
    let mut env = TestEnv::new();
    let (operator, kettle) = setup_default(&mut env);
    let win = env.win_mint;

    let r = send(
        &mut env,
        burn_ix(operator.pubkey(), kettle, win, 0),
        &operator,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(BurnError::ZeroBurnAmount)),
        "amount == 0 → ZeroBurnAmount: {r:?}"
    );
    // The accumulator is untouched (the init values) and nothing was burned.
    assert_windows(&env, (0, 0), (0, 0), "the windows after the rejection");
    assert_eq!(
        env.mint_supply(&win),
        100_000_000_000,
        "supply is untouched"
    );
}

#[test]
fn burn_windows_overflow_is_error_not_panic() {
    // An overflow of checked_add inside accumulate is a handled MathOverflow error
    // (errors-explicit), not a panic. It is reachable without tokens: limits of
    // u64::MAX, burned = 6e8, amount = u64::MAX → the addition overflows BEFORE the
    // limit checks and BEFORE the CPI.
    let mut env = TestEnv::new();
    env.warp_to(BASE_TS);
    env.init_burn_with_limits(u64::MAX, u64::MAX);
    let operator = env.actors.operator.insecure_clone();
    let kettle = fund_vault(&mut env, 1_000_000_000);
    let win = env.win_mint;

    // The windows become non-zero (6 tokens).
    send(
        &mut env,
        burn_ix(operator.pubkey(), kettle, win, 600_000_000),
        &operator,
    )
    .expect("a burn with u64::MAX limits passes");

    // 6e8 + u64::MAX overflows u64 → MathOverflow.
    env.expire_blockhash();
    let r = send(
        &mut env,
        burn_ix(operator.pubkey(), kettle, win, u64::MAX),
        &operator,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(BurnError::MathOverflow)),
        "a window overflow → MathOverflow: {r:?}"
    );
    // The windows and the vault are untouched.
    assert_windows(
        &env,
        (DAY0, 600_000_000),
        (WEEK0, 600_000_000),
        "the windows are untouched",
    );
    assert_eq!(
        env.token_balance(&kettle),
        400_000_000,
        "the vault is untouched"
    );
}

#[test]
fn burn_rejects_foreign_counters_account() {
    // Substituting the accumulator: the `seeds = [b"burn_counters"]` constraint accepts
    // ONLY the canonical PDA - three kinds of decoy account, each with its own
    // structural Anchor rejection (none of them a 6xxx program code).
    let mut env = TestEnv::new();
    let (operator, kettle) = setup_default(&mut env);
    let win = env.win_mint;

    // (1) A perfect forgery: a byte-for-byte copy of the real BurnCounters (owned by the
    // burn program, with a correct discriminator) but at a FOREIGN address. Such an
    // account cannot be created on-chain (owner = burn is set only by the program
    // itself) - the test proves the binding is structural (seeds) rather than "the data
    // looks alike": Anchor recomputes the canonical address through find_program_address
    // and it differs from the one passed in. Copying the data is powerless here by
    // construction - the bump is not taken from the account.
    let real = env
        .svm
        .get_account(&burn_counters_pda())
        .expect("the real accumulator exists");
    let fake = Pubkey::new_unique();
    env.svm
        .set_account(fake, real)
        .expect("the forgery is in place");
    let r = env.send_ixs(
        &[burn_ix_with_counters(
            operator.pubkey(),
            fake,
            kettle,
            win,
            1_000_000_000,
        )],
        &operator.pubkey(),
        &[&operator],
    );
    assert_eq!(
        custom_error_code(&r),
        Some(ErrorCode::ConstraintSeeds as u32),
        "a copy at a foreign address → ConstraintSeeds: {r:?}"
    );

    // (2) A live program account of a DIFFERENT type: burn_state in place of the
    // accumulator → the Account<BurnCounters> discriminator rejects it.
    let r = env.send_ixs(
        &[burn_ix_with_counters(
            operator.pubkey(),
            burn_state_pda(),
            kettle,
            win,
            1_000_000_000,
        )],
        &operator.pubkey(),
        &[&operator],
    );
    assert_eq!(
        custom_error_code(&r),
        Some(ErrorCode::AccountDiscriminatorMismatch as u32),
        "burn_state in place of the counters → AccountDiscriminatorMismatch: {r:?}"
    );

    // (3) A non-existent address → AccountNotInitialized.
    let r = env.send_ixs(
        &[burn_ix_with_counters(
            operator.pubkey(),
            Pubkey::new_unique(),
            kettle,
            win,
            1_000_000_000,
        )],
        &operator.pubkey(),
        &[&operator],
    );
    assert_eq!(
        custom_error_code(&r),
        Some(ErrorCode::AccountNotInitialized as u32),
        "an empty address in place of the counters → AccountNotInitialized: {r:?}"
    );

    // The real accumulator is untouched and nothing was burned.
    assert_windows(
        &env,
        (0, 0),
        (0, 0),
        "the real windows after the substitutions",
    );
    assert_eq!(env.mint_supply(&win), 100_000_000_000, "nothing was burned");
}

// ===================================================================
// NEGATIVE: the vault - substituting it or its authority, the token layer around
// the program - plus an insufficient balance and a mint substitution.
// ===================================================================

#[test]
fn burn_rejects_any_account_but_the_vault() {
    // `seeds = [b"vault"]` accepts ONLY the canonical vault. Three decoys, each a
    // plausible substitute under some weaker binding, all rejected before any CPI:
    let mut env = TestEnv::new();
    let (operator, vault) = setup_default(&mut env);
    let win = env.win_mint;
    let stranger = env.new_user();
    // (1) The operator's OWN $1WIN account - the operator holds the token-level right
    //     over it, and still the program burns from nothing but the vault.
    let own_ata = ata_with(&mut env, &operator.pubkey(), 10_000_000_000);
    // (2) A stranger's account.
    let strangers_ata = ata_with(&mut env, &stranger.pubkey(), 10_000_000_000);
    // (3) The ATA of vault_authority: the same mint and the same owner as the vault,
    //     at a different address - "the same mint and the same authority" is not
    //     uniqueness.
    let authority_ata = ata_with(&mut env, &vault_authority_pda(), 10_000_000_000);

    for (label, decoy) in [
        ("the operator's own account", own_ata),
        ("a stranger's account", strangers_ata),
        ("the ATA of vault_authority", authority_ata),
    ] {
        env.expire_blockhash();
        let r = send(
            &mut env,
            burn_ix(operator.pubkey(), decoy, win, 1_000_000_000),
            &operator,
        );
        assert_eq!(
            custom_error_code(&r),
            Some(ErrorCode::ConstraintSeeds as u32),
            "{label} in place of the vault → ConstraintSeeds: {r:?}"
        );
    }
    // Nothing moved anywhere: the decoys, the vault, the supply, the windows.
    for (label, decoy) in [
        ("the operator's own account", own_ata),
        ("the stranger's account", strangers_ata),
        ("the ATA of vault_authority", authority_ata),
    ] {
        assert_eq!(
            env.token_balance(&decoy),
            10_000_000_000,
            "{label} is intact"
        );
    }
    assert_eq!(
        env.token_balance(&vault),
        100_000_000_000,
        "the vault is intact"
    );
    assert_eq!(env.mint_supply(&win), 130_000_000_000, "nothing was burned");
    assert_windows(&env, (0, 0), (0, 0), "the windows after the substitutions");

    // The positive counterpart: the vault, the same amount.
    env.expire_blockhash();
    send(
        &mut env,
        burn_ix(operator.pubkey(), vault, win, 1_000_000_000),
        &operator,
    )
    .expect("the vault passes");
    assert_eq!(
        env.token_balance(&vault),
        99_000_000_000,
        "the vault after the burn"
    );
}

#[test]
fn burn_rejects_foreign_vault_authority() {
    // "Bring your own authority": the vault together with a foreign key as
    // `vault_authority` - the seeds constraint rejects the authority; the program
    // signs for its own PDA alone.
    let mut env = TestEnv::new();
    let (operator, vault) = setup_default(&mut env);
    let win = env.win_mint;
    let stranger = env.new_user();

    let r = send(
        &mut env,
        burn_ix_with_vault_authority(
            operator.pubkey(),
            vault,
            stranger.pubkey(),
            win,
            1_000_000_000,
        ),
        &operator,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(ErrorCode::ConstraintSeeds as u32),
        "a foreign vault_authority → ConstraintSeeds: {r:?}"
    );
    assert_eq!(
        env.token_balance(&vault),
        100_000_000_000,
        "the vault is intact"
    );
    assert_windows(&env, (0, 0), (0, 0), "the windows after the rejection");
}

#[test]
fn direct_spl_burn_and_transfer_out_of_vault_are_rejected() {
    // The token layer around the program: the vault is owned by the vault_authority
    // PDA, so SPL Token itself refuses a Burn or a Transfer signed by any key - the
    // operator included. Tokens leave the vault through the program's `burn` alone,
    // i.e. under the limits.
    let mut env = TestEnv::new();
    let (operator, vault) = setup_default(&mut env);
    let win = env.win_mint;
    let owner = env.actors.owner.insecure_clone();
    let admin = env.actors.admin.insecure_clone();
    let outside = env.create_ata(&operator.pubkey(), &win);

    for actor in [&operator, &owner, &admin] {
        env.expire_blockhash();
        let r = send(
            &mut env,
            spl_burn_ix(vault, win, actor.pubkey(), 1_000_000_000),
            actor,
        );
        assert_eq!(
            custom_error_code(&r),
            Some(TokenError::OwnerMismatch as u32),
            "a direct SPL Burn signed by {} → OwnerMismatch: {r:?}",
            actor.pubkey()
        );
        env.expire_blockhash();
        let r = send(
            &mut env,
            spl_transfer_ix(vault, outside, actor.pubkey(), 1_000_000_000),
            actor,
        );
        assert_eq!(
            custom_error_code(&r),
            Some(TokenError::OwnerMismatch as u32),
            "a direct SPL Transfer signed by {} → OwnerMismatch: {r:?}",
            actor.pubkey()
        );
    }
    assert_eq!(
        env.token_balance(&vault),
        100_000_000_000,
        "the vault is intact"
    );
    assert_eq!(env.token_balance(&outside), 0, "nothing left the vault");
    assert_eq!(env.mint_supply(&win), 100_000_000_000, "nothing was burned");
    assert_windows(&env, (0, 0), (0, 0), "the windows are untouched");

    // The positive counterpart: the same amount through the program.
    env.expire_blockhash();
    send(
        &mut env,
        burn_ix(operator.pubkey(), vault, win, 1_000_000_000),
        &operator,
    )
    .expect("through the program the burn passes");
    assert_eq!(
        env.token_balance(&vault),
        99_000_000_000,
        "the vault after the burn"
    );
    assert_eq!(
        env.mint_supply(&win),
        99_000_000_000,
        "supply after the burn"
    );
}

#[test]
fn fund_and_burn_in_one_transaction_are_atomic() {
    // The client's flow: a Transfer into the vault and the burn of the same amount
    // in ONE transaction. The vault holds nothing between burns, and a burn the
    // program rejects rolls the funding transfer back with it - the tokens never
    // reach the vault.
    let mut env = TestEnv::new();
    env.warp_to(BASE_TS);
    env.init_default_burn(); // daily 50e9
    let operator = env.actors.operator.insecure_clone();
    let win = env.win_mint;
    let vault = vault_pda();
    // The treasury: the operator's own account holding 100 tokens (10e9).
    let treasury = ata_with(&mut env, &operator.pubkey(), 10_000_000_000);

    // Within the limit: 20 tokens move into the vault and burn in the same
    // transaction.
    env.send_ixs(
        &[
            spl_transfer_ix(treasury, vault, operator.pubkey(), 2_000_000_000),
            burn_ix(operator.pubkey(), vault, win, 2_000_000_000),
        ],
        &operator.pubkey(),
        &[&operator],
    )
    .expect("transfer + burn within the limit pass");
    assert_eq!(env.token_balance(&vault), 0, "the vault is empty again");
    assert_eq!(
        env.token_balance(&treasury),
        8_000_000_000,
        "the treasury after the burn"
    );
    assert_eq!(
        env.mint_supply(&win),
        8_000_000_000,
        "supply after the burn"
    );
    assert_windows(
        &env,
        (DAY0, 2_000_000_000),
        (WEEK0, 2_000_000_000),
        "the windows after the burn",
    );

    // Above the daily limit (2e9 + 6e10 > 5e10): the burn is rejected and the
    // transfer rolls back with it.
    env.mint_to(&win, &treasury, 60_000_000_000);
    env.expire_blockhash();
    let r = env.send_ixs(
        &[
            spl_transfer_ix(treasury, vault, operator.pubkey(), 60_000_000_000),
            burn_ix(operator.pubkey(), vault, win, 60_000_000_000),
        ],
        &operator.pubkey(),
        &[&operator],
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(BurnError::DailyLimitExceeded)),
        "transfer + a burn above the limit → DailyLimitExceeded: {r:?}"
    );
    assert_eq!(env.token_balance(&vault), 0, "nothing reached the vault");
    assert_eq!(
        env.token_balance(&treasury),
        68_000_000_000,
        "the treasury is untouched"
    );
    assert_windows(
        &env,
        (DAY0, 2_000_000_000),
        (WEEK0, 2_000_000_000),
        "the windows are untouched",
    );
}

#[test]
fn burn_insufficient_vault_is_spl_error_and_windows_roll_back() {
    // Atomicity: the windows are mutated BEFORE the CPI - a failing CPI must roll them
    // back too. An insufficient vault balance yields the SPL InsufficientFunds code.
    let mut env = TestEnv::new();
    env.warp_to(BASE_TS);
    env.init_default_burn();
    let operator = env.actors.operator.insecure_clone();
    let kettle = fund_vault(&mut env, 1_000_000_000); // 10 tokens
    let win = env.win_mint;

    // The positive case: 6 tokens are burned and 4e8 remain in the vault.
    send(
        &mut env,
        burn_ix(operator.pubkey(), kettle, win, 600_000_000),
        &operator,
    )
    .expect("a burn within the balance passes");
    assert_windows(
        &env,
        (DAY0, 600_000_000),
        (WEEK0, 600_000_000),
        "the windows after the positive case",
    );

    // The negative: 5 tokens against a remainder of 4 → SPL InsufficientFunds; the
    // windows stay at their pre-transaction values.
    env.expire_blockhash();
    let r = send(
        &mut env,
        burn_ix(operator.pubkey(), kettle, win, 500_000_000),
        &operator,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(TokenError::InsufficientFunds as u32),
        "an insufficient vault balance → SPL InsufficientFunds: {r:?}"
    );
    assert_windows(
        &env,
        (DAY0, 600_000_000),
        (WEEK0, 600_000_000),
        "the windows rolled back to their pre-transaction values",
    );
    assert_eq!(
        env.token_balance(&kettle),
        400_000_000,
        "the vault is untouched"
    );
}

#[test]
fn burn_rejects_foreign_mint_account() {
    // Substituting the mint account: `address = burn_state.token_mint` → InvalidMint.
    let mut env = TestEnv::new();
    let (operator, kettle) = setup_default(&mut env);
    let foreign_mint = env.create_mint_for_test(8);

    let r = send(
        &mut env,
        burn_ix(operator.pubkey(), kettle, foreign_mint, 1_000_000_000),
        &operator,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(BurnError::InvalidMint)),
        "a foreign mint → InvalidMint: {r:?}"
    );
}

// ===================================================================
// PERIOD ROLLOVERS: the lazy reset of the accumulator windows.
// ===================================================================

#[test]
fn burn_day_rollover_resets_daily_accumulates_weekly() {
    let mut env = TestEnv::new();
    env.warp_to(BASE_TS);
    env.init_default_burn(); // daily 50e9, weekly 200e9
    let operator = env.actors.operator.insecure_clone();
    let kettle = fund_vault(&mut env, 200_000_000_000);
    let win = env.win_mint;

    // Day 23_149: we burn the FULL daily limit.
    send(
        &mut env,
        burn_ix(operator.pubkey(), kettle, win, 50_000_000_000),
        &operator,
    )
    .expect("day 0: a burn at the limit passes");
    assert_windows(
        &env,
        (DAY0, 50_000_000_000),
        (WEEK0, 50_000_000_000),
        "day 0",
    );

    // The next day within the same week. The FULL daily limit again: within the old day
    // the sum would not have passed (50e9 + 50e9 > 50e9) - it passes ONLY because the
    // daily window was reset lazily by the index change. In v3 burn takes no index
    // arguments - both txs are byte-identical, and a warp does not rotate the blockhash
    // by itself → expire_blockhash is mandatory, otherwise AlreadyProcessed.
    env.warp_to(DAY1_TS);
    env.expire_blockhash();
    let r = send(
        &mut env,
        burn_ix(operator.pubkey(), kettle, win, 50_000_000_000),
        &operator,
    );
    r.as_ref()
        .expect("day 1: the full daily limit is available again");

    // The daily window was RESET and accumulated afresh; the weekly one ACCUMULATED
    // across the days: 50e9 + 50e9 = 100e9.
    assert_windows(
        &env,
        (DAY1, 50_000_000_000),
        (WEEK0, 100_000_000_000),
        "day 1",
    );
    // The second day's event: the indices come from Clock, day_total starts over and
    // week_total runs through.
    let ev: BurnExecuted = decode_event(&r).expect("day 1's event");
    assert_eq!(ev.day_index, 23_150, "event.day_index of the new day");
    assert_eq!(ev.week_index, 3_307, "event.week_index of the same week");
    assert_eq!(ev.day_total, 50_000_000_000, "event.day_total starts over");
    assert_eq!(
        ev.week_total, 100_000_000_000,
        "event.week_total runs through"
    );
    // The supply total: 200e9 − 100e9.
    assert_eq!(
        env.mint_supply(&win),
        100_000_000_000,
        "supply after 2 days"
    );
}

#[test]
fn burn_week_rollover_resets_both_windows() {
    // Daily 50e9, weekly 60e9: the old week is topped up to its limit over two days, and
    // the burn in the new week passes ONLY because the weekly window was reset
    // (60e9 + 50e9 = 110e9 > 60e9 without the reset).
    let mut env = TestEnv::new();
    env.warp_to(BASE_TS);
    env.init_burn_with_limits(50_000_000_000, 60_000_000_000);
    let operator = env.actors.operator.insecure_clone();
    let kettle = fund_vault(&mut env, 200_000_000_000);
    let win = env.win_mint;

    // Day 23_149 (week 3_307): the full daily limit.
    send(
        &mut env,
        burn_ix(operator.pubkey(), kettle, win, 50_000_000_000),
        &operator,
    )
    .expect("week 0, day 0: the burn passes");

    // Day 23_150 (the same week): +10e9 → the weekly window sits EXACTLY at the 60e9
    // limit.
    env.warp_to(DAY1_TS);
    send(
        &mut env,
        burn_ix(operator.pubkey(), kettle, win, 10_000_000_000),
        &operator,
    )
    .expect("week 0, day 1: topping up to the weekly limit passes");
    assert_windows(
        &env,
        (DAY1, 10_000_000_000),
        (WEEK0, 60_000_000_000),
        "week 0 is exhausted",
    );

    // The old week is exhausted: one more unit yields WeeklyLimitExceeded (the daily
    // window would have let it through: 10e9 + 1 ≤ 50e9).
    env.expire_blockhash();
    let r = send(
        &mut env,
        burn_ix(operator.pubkey(), kettle, win, 1),
        &operator,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(BurnError::WeeklyLimitExceeded)),
        "the week at its limit + 1 → WeeklyLimitExceeded: {r:?}"
    );

    // The start of week 3_308 (= the start of day 23_156): BOTH windows were reset - 50e9
    // passes even though the old weekly window would not have allowed it. The tx is
    // byte-identical to the day-0 burn (v3 takes no index arguments) → a fresh blockhash
    // is mandatory.
    env.warp_to(WEEK1_TS);
    env.expire_blockhash();
    let r = send(
        &mut env,
        burn_ix(operator.pubkey(), kettle, win, 50_000_000_000),
        &operator,
    );
    r.as_ref()
        .expect("week 1: the weekly limit is available again");
    assert_windows(
        &env,
        (WEEK1_DAY, 50_000_000_000),
        (WEEK1, 50_000_000_000),
        "both windows were reset by the new week",
    );
    let ev: BurnExecuted = decode_event(&r).expect("the new week's event");
    assert_eq!(ev.day_index, 23_156, "event.day_index of the new day");
    assert_eq!(ev.week_index, 3_308, "event.week_index of the new week");
    assert_eq!(ev.day_total, 50_000_000_000, "event.day_total starts over");
    assert_eq!(
        ev.week_total, 50_000_000_000,
        "event.week_total starts over"
    );
    // The supply total: 200e9 − (50e9 + 10e9 + 50e9) = 90e9.
    assert_eq!(
        env.mint_supply(&win),
        90_000_000_000,
        "supply after three burns"
    );
}
