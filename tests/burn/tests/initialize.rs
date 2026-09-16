//! Burn `initialize` - the positive case plus the full negative matrix.
//!
//! tests-bind-to-real-code: every test invokes the REAL instruction through
//! `burn_initialize_ix`, asserts the observable on-chain effect (the `BurnState`
//! fields, the `BurnCounters` windows, the payload of the `Initialized` event) and a
//! specific error code bound to the `BurnError` enum (never a magic literal).

use anchor_lang::error::ERROR_CODE_OFFSET;
use burn::errors::BurnError;
use burn::events::Initialized;
use burn_tests::{
    burn_counters_pda, burn_initialize_ix, burn_initialize_ix_with_program_data, burn_state_pda,
    vault_authority_pda, vault_pda, BurnTestEnv,
};
use sale_test_common::{custom_error_code, decode_event, TestEnv};
use solana_instruction_error::InstructionError;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction_error::TransactionError;

/// The program's custom error code (the Anchor offset 6000 plus the variant index).
fn err(variant: BurnError) -> u32 {
    ERROR_CODE_OFFSET + variant as u32
}

/// The positive case's valid limits: 110 and 770 tokens × 10^8 units. The literals
/// deliberately differ from the `init_default_burn` defaults so that the stored-field
/// assertions catch the fixture substituting the arguments.
const DAILY: u64 = 11_000_000_000;
const WEEKLY: u64 = 77_000_000_000;

/// An init rejection is atomic: NONE of the three accounts was created.
fn assert_no_singletons(env: &TestEnv, ctx: &str) {
    assert!(
        !env.burn_state_exists(),
        "{ctx}: burn_state was not created"
    );
    assert!(
        !env.account_exists(&burn_counters_pda()),
        "{ctx}: burn_counters was not created"
    );
    assert!(
        !env.account_exists(&vault_pda()),
        "{ctx}: the vault was not created"
    );
}

/// Sends the burn `initialize`: authority is the signer and the payer; the mint and
/// the roles come from the arguments.
/// We return the native litesvm TransactionResult as is (as `send_ixs` does).
#[allow(clippy::result_large_err, clippy::too_many_arguments)]
fn send_initialize(
    env: &mut TestEnv,
    authority: &Keypair,
    token_mint: Pubkey,
    owner: Pubkey,
    admin: Pubkey,
    operator: Pubkey,
    daily: u64,
    weekly: u64,
) -> litesvm::types::TransactionResult {
    let ix = burn_initialize_ix(
        authority.pubkey(),
        token_mint,
        owner,
        admin,
        operator,
        daily,
        weekly,
    );
    env.send_ixs(&[ix], &authority.pubkey(), &[authority])
}

// ===================================================================
// POSITIVE
// ===================================================================

#[test]
fn initialize_stores_all_fields_and_emits_event() {
    let mut env = TestEnv::new();
    let authority = env.actors.owner.insecure_clone();
    let win = env.win_mint;
    let owner_role = env.actors.owner.pubkey();
    let admin_role = env.actors.admin.pubkey();
    let operator_role = env.actors.operator.pubkey();

    let r = send_initialize(
        &mut env,
        &authority,
        win,
        owner_role,
        admin_role,
        operator_role,
        DAILY,
        WEEKLY,
    );
    r.as_ref().expect("a valid initialize passes");

    // Every field of the state.
    let s = env.burn_state();
    assert_eq!(s.owner, owner_role, "owner");
    assert_eq!(
        s.pending_owner, None,
        "pending_owner - no handover in flight"
    );
    assert_eq!(s.admin, admin_role, "admin");
    assert_eq!(s.operator, operator_role, "operator");
    assert_eq!(s.token_mint, win, "token_mint");
    assert_eq!(s.daily_limit, DAILY, "daily_limit");
    assert_eq!(s.weekly_limit, WEEKLY, "weekly_limit");
    // There is no field to assert the address's canonicity with, and no need: the bump
    // is not stored, and the read goes through `burn_state_pda()` =
    // find_program_address - a successful deserialization is itself proof that the
    // singleton was created at the canonical address.

    // The accumulator was created by THAT SAME initialize: both windows are "period 0
    // holding zero" (which is safe: the first burn of a real period resets the window
    // lazily).
    let c = env.burn_counters();
    assert_eq!(c.day.index, 0, "day.index after init");
    assert_eq!(c.day.burned, 0, "day.burned after init");
    assert_eq!(c.week.index, 0, "week.index after init");
    assert_eq!(c.week.burned, 0, "week.burned after init");

    // The vault was created by that same initialize: a $1WIN account owned by the
    // vault_authority PDA, empty.
    assert_eq!(
        env.token_account_mint_owner(&vault_pda()),
        (win, vault_authority_pda()),
        "the vault's (mint, owner)"
    );
    assert_eq!(env.token_balance(&vault_pda()), 0, "the vault is empty");

    // The Initialized event - its payload is an observable effect too.
    let ev: Initialized = decode_event(&r).expect("the Initialized event is emitted");
    assert_eq!(ev.owner, owner_role, "event.owner");
    assert_eq!(ev.admin, admin_role, "event.admin");
    assert_eq!(ev.operator, operator_role, "event.operator");
    assert_eq!(ev.token_mint, win, "event.token_mint");
    assert_eq!(ev.daily_limit, DAILY, "event.daily_limit");
    assert_eq!(ev.weekly_limit, WEEKLY, "event.weekly_limit");
}

// ===================================================================
// NEGATIVE: a repeated init (the structural init guard)
// ===================================================================

#[test]
fn double_initialize_reverts_and_keeps_first_state() {
    let mut env = TestEnv::new();
    let authority = env.actors.owner.insecure_clone();
    let win = env.win_mint;
    let owner_role = env.actors.owner.pubkey();
    let admin_role = env.actors.admin.pubkey();
    let operator_role = env.actors.operator.pubkey();

    send_initialize(
        &mut env,
        &authority,
        win,
        owner_role,
        admin_role,
        operator_role,
        DAILY,
        WEEKLY,
    )
    .expect("the first initialize passes");
    let first = env.burn_state();

    // A second init with DIFFERENT values (new admin/operator, different limits).
    env.expire_blockhash(); // a fresh blockhash, otherwise AlreadyProcessed before execution
    let r = send_initialize(
        &mut env,
        &authority,
        win,
        owner_role,
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        DAILY + 1,
        WEEKLY + 1,
    );

    // FACT (the same as for sale, see tests/initialize.rs): a repeated
    // `#[account(init)]` fails inside the System Program CPI with "account already in
    // use" = Custom(0) FROM the System Program (not a 6xxx burn code) - Anchor's
    // structural init guard fires BEFORE the handler's body, on the very FIRST init
    // account of the context (burn_state; burn_counters is never reached).
    assert!(r.is_err(), "the second initialize must revert: {r:?}");
    let failed = r.as_ref().expect_err("the double-init revert");
    assert!(
        matches!(
            failed.err,
            TransactionError::InstructionError(_, InstructionError::Custom(0))
        ),
        "double-init → System `account already in use` = Custom(0): {:?}",
        failed.err
    );

    // The state keeps the values of the first init; the accumulator is alive and zeroed.
    let after = env.burn_state();
    assert_eq!(after.admin, first.admin, "admin was not overwritten");
    assert_eq!(
        after.operator, first.operator,
        "operator was not overwritten"
    );
    assert_eq!(
        after.daily_limit, first.daily_limit,
        "daily was not overwritten"
    );
    assert_eq!(
        after.weekly_limit, first.weekly_limit,
        "weekly was not overwritten"
    );
    let c = env.burn_counters();
    assert_eq!(
        c.day.burned, 0,
        "the accumulator was untouched by the repeated init"
    );
    assert_eq!(
        c.week.burned, 0,
        "the accumulator was untouched by the repeated init"
    );
}

// ===================================================================
// NEGATIVE: front-running / authorization (the upgrade authority gate)
// ===================================================================

#[test]
fn initialize_rejects_non_upgrade_authority_signer() {
    // The upgrade authority is a random key; the owner actor signs and does NOT hold the
    // upgrade authority → the program_data constraint → Unauthorized.
    let mut env = TestEnv::with_upgrade_authority(Pubkey::new_unique());
    let authority = env.actors.owner.insecure_clone();
    let win = env.win_mint;
    let owner_role = env.actors.owner.pubkey();
    let admin_role = env.actors.admin.pubkey();
    let operator_role = env.actors.operator.pubkey();

    let r = send_initialize(
        &mut env,
        &authority,
        win,
        owner_role,
        admin_role,
        operator_role,
        DAILY,
        WEEKLY,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(BurnError::Unauthorized)),
        "owner-not-upgrade-authority → Unauthorized: {r:?}"
    );
    assert_no_singletons(&env, "not the upgrade authority");
}

#[test]
fn initialize_rejects_random_signer_not_upgrade_authority() {
    // The signer is a fresh funded key; the environment's default upgrade authority is
    // the owner actor.
    let mut env = TestEnv::new();
    let stranger = env.new_user();
    let win = env.win_mint;
    let owner_role = env.actors.owner.pubkey();
    let admin_role = env.actors.admin.pubkey();
    let operator_role = env.actors.operator.pubkey();

    let r = send_initialize(
        &mut env,
        &stranger,
        win,
        owner_role,
        admin_role,
        operator_role,
        DAILY,
        WEEKLY,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(BurnError::Unauthorized)),
        "a random signer → Unauthorized: {r:?}"
    );
    assert_no_singletons(&env, "a random signer");
}

#[test]
fn initialize_rejects_foreign_program_data() {
    // A foreign ProgramData: we slip in the ProgramData of the SALE program. The signer
    // (the owner actor) does hold its upgrade authority - that is, the program_data
    // constraint alone would pass; what rejects it is the
    // `burn_program.programdata_address() == program_data.key()` check.
    let mut env = TestEnv::new();
    let authority = env.actors.owner.insecure_clone();
    let win = env.win_mint;
    let owner_role = env.actors.owner.pubkey();
    let admin_role = env.actors.admin.pubkey();
    let operator_role = env.actors.operator.pubkey();

    let ix = burn_initialize_ix_with_program_data(
        authority.pubkey(),
        TestEnv::programdata_address(), // the ProgramData of the Sale program, not Burn
        win,
        owner_role,
        admin_role,
        operator_role,
        DAILY,
        WEEKLY,
    );
    let r = env.send_ixs(&[ix], &authority.pubkey(), &[&authority]);
    assert_eq!(
        custom_error_code(&r),
        Some(err(BurnError::Unauthorized)),
        "a foreign ProgramData → Unauthorized: {r:?}"
    );
    assert_no_singletons(&env, "a foreign ProgramData");
}

// ===================================================================
// NEGATIVE: zero roles / limits
// ===================================================================

#[test]
fn initialize_rejects_zero_role_pubkeys() {
    for role in ["owner", "admin", "operator"] {
        let mut env = TestEnv::new();
        let authority = env.actors.owner.insecure_clone();
        let win = env.win_mint;
        let mut owner_role = env.actors.owner.pubkey();
        let mut admin_role = env.actors.admin.pubkey();
        let mut operator_role = env.actors.operator.pubkey();
        match role {
            "owner" => owner_role = Pubkey::default(),
            "admin" => admin_role = Pubkey::default(),
            "operator" => operator_role = Pubkey::default(),
            _ => unreachable!(),
        }

        let r = send_initialize(
            &mut env,
            &authority,
            win,
            owner_role,
            admin_role,
            operator_role,
            DAILY,
            WEEKLY,
        );
        assert_eq!(
            custom_error_code(&r),
            Some(err(BurnError::InvalidAuthority)),
            "a zero {role} → InvalidAuthority: {r:?}"
        );
        assert_no_singletons(&env, &format!("the {role} role"));
    }
}

#[test]
fn initialize_rejects_zero_limits() {
    for limit in ["daily", "weekly"] {
        let mut env = TestEnv::new();
        let authority = env.actors.owner.insecure_clone();
        let win = env.win_mint;
        let owner_role = env.actors.owner.pubkey();
        let admin_role = env.actors.admin.pubkey();
        let operator_role = env.actors.operator.pubkey();
        let (daily, weekly) = match limit {
            "daily" => (0, WEEKLY),
            "weekly" => (DAILY, 0),
            _ => unreachable!(),
        };

        let r = send_initialize(
            &mut env,
            &authority,
            win,
            owner_role,
            admin_role,
            operator_role,
            daily,
            weekly,
        );
        assert_eq!(
            custom_error_code(&r),
            Some(err(BurnError::InvalidParams)),
            "a zero {limit} limit → InvalidParams: {r:?}"
        );
        assert_no_singletons(&env, &format!("the {limit} limit"));
    }
}

// ===================================================================
// NEGATIVE: the mint's decimals
// ===================================================================

#[test]
fn initialize_rejects_wrong_mint_decimals() {
    // Both directions away from 8: the environment's real USDC mint (dec 6) and a
    // purpose-built dec 9 one.
    let mut env = TestEnv::new();
    let authority = env.actors.owner.insecure_clone();
    let owner_role = env.actors.owner.pubkey();
    let admin_role = env.actors.admin.pubkey();
    let operator_role = env.actors.operator.pubkey();
    let usdc = env.usdc_mint; // dec 6
    let dec9 = env.create_mint_for_test(9);

    for (label, bad_mint) in [("dec6", usdc), ("dec9", dec9)] {
        let r = send_initialize(
            &mut env,
            &authority,
            bad_mint,
            owner_role,
            admin_role,
            operator_role,
            DAILY,
            WEEKLY,
        );
        assert_eq!(
            custom_error_code(&r),
            Some(err(BurnError::InvalidMintDecimals)),
            "a {label} mint (≠ 8) → InvalidMintDecimals: {r:?}"
        );
        assert_no_singletons(&env, &format!("the {label} mint"));
        env.expire_blockhash();
    }

    // The control for the negative pair: the same config with a dec-8 mint passes.
    let ix = burn_tests::burn_initialize_ix(
        authority.pubkey(),
        env.win_mint,
        owner_role,
        admin_role,
        operator_role,
        DAILY,
        WEEKLY,
    );
    env.send_ixs(&[ix], &authority.pubkey(), &[&authority])
        .expect("a dec-8 mint: initialize passes");
    assert!(
        env.account_exists(&burn_state_pda()),
        "burn_state was created"
    );
    assert!(
        env.account_exists(&burn_counters_pda()),
        "burn_counters was created"
    );
    assert!(env.account_exists(&vault_pda()), "the vault was created");
}
