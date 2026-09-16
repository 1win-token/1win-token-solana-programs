//! `initialize` (Arch §9.1) - the positive case plus the full negative matrix.
//!
//! tests-bind-to-real-code: every test invokes the REAL instruction through
//! `initialize_ix`, asserts the observable on-chain effect (the `SaleState` fields,
//! the vaults' mint/owner) and a specific error code bound to the `SaleError` enum
//! (never a magic literal).
//!
//! Codes: `ERR(SaleError::Variant)` = 6000 plus the variant index.

use anchor_lang::error::ERROR_CODE_OFFSET;
use sale::errors::SaleError;
use sale::state::{
    MAX_CLAIM_DURATION, MAX_CLAIM_START_DELAY, MAX_SALE_DURATION, MAX_SALE_START_DELAY,
    MIN_CLAIM_DURATION,
};
use sale_test_common::{
    custom_error_code, deposit_vault_pda, initialize_ix, token_vault_pda, vault_authority_pda,
    InitParams, TestEnv,
};
use solana_instruction_error::InstructionError;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction_error::TransactionError;

/// The program's custom error code (the Anchor offset 6000 plus the variant index).
fn err(variant: SaleError) -> u32 {
    ERROR_CODE_OFFSET + variant as u32
}

/// `price_usdc == TOKEN_UNITS` - the lower edge of the invalid range
/// (`>= TOKEN_UNITS`).
const TOKEN_UNITS: u64 = 100_000_000;

/// Sends `initialize` with the given `authority` (the signer), mints and arguments;
/// payer = authority.
/// We return the native litesvm TransactionResult as is - just like the
/// environment's `send_ixs` helper (boxing it would fork the interface for a test).
#[allow(clippy::result_large_err)]
fn send_initialize(
    env: &mut TestEnv,
    authority: &solana_keypair::Keypair,
    usdc_mint: Pubkey,
    token_mint: Pubkey,
    p: &InitParams,
) -> litesvm::types::TransactionResult {
    let ix = initialize_ix(authority.pubkey(), usdc_mint, token_mint, p);
    env.send_ixs(&[ix], &authority.pubkey(), &[authority])
}

// ===================================================================
// POSITIVE
// ===================================================================

#[test]
fn initialize_succeeds_and_stores_all_fields() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let p = env.valid_init_params();

    send_initialize(&mut env, &owner, usdc, win, &p).expect("a valid initialize passes");

    let s = env.sale_state();
    // The roles.
    assert_eq!(s.owner, p.owner, "owner");
    assert_eq!(s.admin, p.admin, "admin");
    assert_eq!(s.operator, p.operator, "operator");
    // The mints (read-only references).
    assert_eq!(s.usdc_mint, usdc, "usdc_mint");
    assert_eq!(s.token_mint, win, "token_mint");
    // The economics.
    assert_eq!(s.price_usdc, p.price_usdc, "price_usdc");
    assert_eq!(s.emission, p.emission, "emission");
    assert_eq!(s.min_per_tx, p.min_per_tx, "min_per_tx");
    assert_eq!(s.max_per_tx, p.max_per_tx, "max_per_tx");
    // The schedule.
    assert_eq!(s.sale_start_date, p.sale_start_date, "sale_start_date");
    assert_eq!(s.sale_end_date, p.sale_end_date, "sale_end_date");
    assert_eq!(s.claim_start_date, p.claim_start_date, "claim_start_date");
    assert_eq!(s.claim_duration, p.claim_duration, "claim_duration");
    // The flags and counters.
    assert!(!s.pool_fixed, "pool_fixed == false");
    assert!(!s.paused, "paused == false");
    assert_eq!(s.deposits_usdc_contributed, 0, "deposits counter zero");
    assert_eq!(s.pool_fixed_amount, 0, "pool_fixed_amount zero");
    assert_eq!(s.participants, 0, "participants zero");
    // The vault addresses == the PDAs.
    assert_eq!(s.deposit_vault, deposit_vault_pda(), "deposit_vault addr");
    assert_eq!(s.token_vault, token_vault_pda(), "token_vault addr");
    // The only stored bump belongs to vault_authority (it is needed for the CPI
    // signature); it was written canonically.
    let canonical_va_bump = Pubkey::find_program_address(&[b"vault_authority"], &sale::id()).1;
    assert_eq!(
        s.vault_authority_bump, canonical_va_bump,
        "vault_authority_bump is canonical"
    );
}

#[test]
fn initialize_creates_vaults_bound_to_mints_and_vault_authority() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let p = env.valid_init_params();

    send_initialize(&mut env, &owner, usdc, win, &p).expect("initialize passes");

    let vault_auth = vault_authority_pda();
    // deposit_vault: mint == USDC, owner == vault_authority.
    let (dv_mint, dv_owner) = env.token_account_mint_owner(&deposit_vault_pda());
    assert_eq!(dv_mint, usdc, "deposit_vault.mint == usdc");
    assert_eq!(
        dv_owner, vault_auth,
        "deposit_vault.owner == vault_authority"
    );
    // token_vault: mint == $1WIN, owner == vault_authority.
    let (tv_mint, tv_owner) = env.token_account_mint_owner(&token_vault_pda());
    assert_eq!(tv_mint, win, "token_vault.mint == token");
    assert_eq!(tv_owner, vault_auth, "token_vault.owner == vault_authority");
}

// ===================================================================
// NEGATIVE: front-running / authorization
// ===================================================================

#[test]
fn initialize_rejects_non_upgrade_authority_signer() {
    // The upgrade authority is a random key; the owner actor signs and does NOT hold the
    // upgrade authority → the program_data constraint → Unauthorized.
    let random_authority = Pubkey::new_unique();
    let mut env = TestEnv::with_upgrade_authority(random_authority);
    let owner = env.actors.owner.insecure_clone();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let p = env.valid_init_params();

    let r = send_initialize(&mut env, &owner, usdc, win, &p);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::Unauthorized)),
        "owner-not-upgrade-authority → Unauthorized, the result: {r:?}"
    );
    assert!(
        !env.sale_state_exists(),
        "the singleton was not created on rejection"
    );
}

#[test]
fn initialize_rejects_random_signer_not_upgrade_authority() {
    // The signer is a fresh random (funded) key that likewise does not hold the upgrade
    // authority (which is the owner actor by default).
    let mut env = TestEnv::new();
    let stranger = env.new_user();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let p = env.valid_init_params();

    let r = send_initialize(&mut env, &stranger, usdc, win, &p);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::Unauthorized)),
        "a random signer → Unauthorized, the result: {r:?}"
    );
    assert!(!env.sale_state_exists());
}

// ===================================================================
// NEGATIVE: the mints' decimals
// ===================================================================

#[test]
fn initialize_rejects_wrong_usdc_decimals() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let win = env.win_mint;
    let p = env.valid_init_params();
    // A USDC mint with NON-6 decimals (7).
    let bad_usdc = env.create_mint_for_test(7);

    let r = send_initialize(&mut env, &owner, bad_usdc, win, &p);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::InvalidMintDecimals)),
        "usdc decimals != 6 → InvalidMintDecimals, the result: {r:?}"
    );
    assert!(!env.sale_state_exists());
}

#[test]
fn initialize_rejects_wrong_token_decimals() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let usdc = env.usdc_mint;
    let p = env.valid_init_params();
    // A token mint with NON-8 decimals (9).
    let bad_token = env.create_mint_for_test(9);

    let r = send_initialize(&mut env, &owner, usdc, bad_token, &p);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::InvalidMintDecimals)),
        "token decimals != 8 → InvalidMintDecimals, the result: {r:?}"
    );
    assert!(!env.sale_state_exists());
}

// ===================================================================
// NEGATIVE: the Sale schedule
// ===================================================================

#[test]
fn initialize_rejects_sale_start_date_not_in_future() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let mut p = env.valid_init_params();
    p.sale_start_date = env.now(); // sale_start_date == now (violates now < sale_start_date)

    let r = send_initialize(&mut env, &owner, usdc, win, &p);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::InvalidDeadline)),
        "sale_start_date <= now → InvalidDeadline, the result: {r:?}"
    );
}

#[test]
fn initialize_rejects_end_before_start() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let mut p = env.valid_init_params();
    p.sale_end_date = p.sale_start_date; // end == start (violates start < end)

    let r = send_initialize(&mut env, &owner, usdc, win, &p);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::InvalidDeadline)),
        "sale_end_date <= sale_start_date → InvalidDeadline, the result: {r:?}"
    );
}

// The Sale schedule ceilings: the start's delay from now and the length of the
// intake window. The boundaries are tight - exactly at the ceiling it passes, one
// second beyond it reverts.

#[test]
fn initialize_rejects_sale_start_date_too_far() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let mut p = env.valid_init_params();
    p.sale_start_date = env.now() + MAX_SALE_START_DELAY + 1; // one second past the ceiling
    p.sale_end_date = p.sale_start_date + 1_000; // the window length is fine
    p.claim_start_date = p.sale_end_date + 1_000;

    let r = send_initialize(&mut env, &owner, usdc, win, &p);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::StartDateTooFar)),
        "sale_start_date > now+90d → StartDateTooFar, the result: {r:?}"
    );
    assert!(!env.sale_state_exists(), "the state was not created");
}

#[test]
fn initialize_accepts_sale_start_date_at_delay_ceiling() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let mut p = env.valid_init_params();
    p.sale_start_date = env.now() + MAX_SALE_START_DELAY; // exactly the ceiling
    p.sale_end_date = p.sale_start_date + 1_000;
    p.claim_start_date = p.sale_end_date + 1_000;

    send_initialize(&mut env, &owner, usdc, win, &p)
        .expect("a start exactly at the ceiling passes");
    assert_eq!(
        env.sale_state().sale_start_date,
        p.sale_start_date,
        "sale_start_date was written"
    );
}

#[test]
fn initialize_rejects_sale_duration_too_long() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let mut p = env.valid_init_params();
    p.sale_end_date = p.sale_start_date + MAX_SALE_DURATION + 1; // one second past the ceiling
    p.claim_start_date = p.sale_end_date + 1_000;

    let r = send_initialize(&mut env, &owner, usdc, win, &p);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::SaleDurationTooLong)),
        "end-start > 30d → SaleDurationTooLong, the result: {r:?}"
    );
    assert!(!env.sale_state_exists(), "the state was not created");
}

#[test]
fn initialize_accepts_sale_duration_at_ceiling() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let mut p = env.valid_init_params();
    p.sale_end_date = p.sale_start_date + MAX_SALE_DURATION; // exactly the ceiling
    p.claim_start_date = p.sale_end_date + 1_000;

    send_initialize(&mut env, &owner, usdc, win, &p)
        .expect("a length exactly at the ceiling passes");
    let s = env.sale_state();
    assert_eq!(
        s.sale_end_date - s.sale_start_date,
        MAX_SALE_DURATION,
        "the window was written at its full permitted length"
    );
}

// ===================================================================
// NEGATIVE: the Claim duration
// ===================================================================

#[test]
fn initialize_rejects_claim_duration_below_min() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let mut p = env.valid_init_params();
    p.claim_duration = MIN_CLAIM_DURATION - 1; // one second below the floor

    let r = send_initialize(&mut env, &owner, usdc, win, &p);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::ClaimDurationTooShort)),
        "claim_duration < MIN → ClaimDurationTooShort, the result: {r:?}"
    );
    assert!(!env.sale_state_exists(), "the state was not created");
}

#[test]
fn initialize_accepts_claim_duration_at_min() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let mut p = env.valid_init_params();
    p.claim_duration = MIN_CLAIM_DURATION; // exactly the floor

    send_initialize(&mut env, &owner, usdc, win, &p)
        .expect("a duration exactly at the floor passes");
    assert_eq!(
        env.sale_state().claim_duration,
        MIN_CLAIM_DURATION,
        "the window was written at its shortest permitted length"
    );
}

#[test]
fn initialize_rejects_claim_duration_above_max() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let mut p = env.valid_init_params();
    p.claim_duration = MAX_CLAIM_DURATION + 1;

    let r = send_initialize(&mut env, &owner, usdc, win, &p);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::ClaimDurationTooLong)),
        "claim_duration > MAX → ClaimDurationTooLong, the result: {r:?}"
    );
}

// ===================================================================
// NEGATIVE: the Claim schedule
// ===================================================================

#[test]
fn initialize_rejects_claim_start_date_in_past() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let mut p = env.valid_init_params();
    p.claim_start_date = p.sale_end_date; // claim_start_date == sale_end_date (violates end < claim_start_date)

    let r = send_initialize(&mut env, &owner, usdc, win, &p);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::ClaimStartInPast)),
        "claim_start_date <= sale_end_date → ClaimStartInPast, the result: {r:?}"
    );
}

#[test]
fn initialize_rejects_claim_start_date_too_far() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let mut p = env.valid_init_params();
    p.claim_start_date = p.sale_end_date + MAX_CLAIM_START_DELAY + 1; // one second past the ceiling

    let r = send_initialize(&mut env, &owner, usdc, win, &p);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::ClaimStartTooFar)),
        "claim_start_date > end+30d → ClaimStartTooFar, the result: {r:?}"
    );
}

// ===================================================================
// NEGATIVE: the economics
// ===================================================================

#[test]
fn initialize_rejects_zero_price() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let mut p = env.valid_init_params();
    p.price_usdc = 0;

    let r = send_initialize(&mut env, &owner, usdc, win, &p);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::InvalidParams)),
        "price == 0 → InvalidParams, the result: {r:?}"
    );
}

#[test]
fn initialize_rejects_price_at_or_above_token_units() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let mut p = env.valid_init_params();
    p.price_usdc = TOKEN_UNITS; // exactly the lower edge of the invalid range (>= TOKEN_UNITS)

    let r = send_initialize(&mut env, &owner, usdc, win, &p);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::InvalidParams)),
        "price >= TOKEN_UNITS → InvalidParams, the result: {r:?}"
    );
}

#[test]
fn initialize_rejects_zero_emission() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let mut p = env.valid_init_params();
    p.emission = 0;

    let r = send_initialize(&mut env, &owner, usdc, win, &p);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::InvalidParams)),
        "emission == 0 → InvalidParams, the result: {r:?}"
    );
}

#[test]
fn initialize_rejects_zero_min_per_tx() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let mut p = env.valid_init_params();
    p.min_per_tx = 0;

    let r = send_initialize(&mut env, &owner, usdc, win, &p);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::InvalidParams)),
        "min_per_tx == 0 → InvalidParams, the result: {r:?}"
    );
}

#[test]
fn initialize_rejects_min_above_max() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let mut p = env.valid_init_params();
    p.min_per_tx = p.max_per_tx + 1; // min > max

    let r = send_initialize(&mut env, &owner, usdc, win, &p);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::InvalidParams)),
        "min_per_tx > max_per_tx → InvalidParams, the result: {r:?}"
    );
}

// ===================================================================
// NEGATIVE: the mints and the roles
// ===================================================================

#[test]
fn initialize_rejects_same_mint_for_usdc_and_token() {
    // The same mint in both roles → InvalidMint. The program checks
    // require_keys_neq(usdc, token) BEFORE the decimals checks, so the InvalidMint code
    // precisely diagnoses the violated "the mints are distinct" invariant (§9.1) and is
    // reachable regardless of the submitted mint's decimals.
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let win = env.win_mint; // dec 8
    let p = env.valid_init_params();

    let r = send_initialize(&mut env, &owner, win, win, &p);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::InvalidMint)),
        "the same mint in both roles → InvalidMint, the result: {r:?}"
    );
    assert!(!env.sale_state_exists());
}

#[test]
fn initialize_rejects_zero_role_pubkeys() {
    // A zero owner / admin / operator - each in its own run.
    for role in ["owner", "admin", "operator"] {
        let mut env = TestEnv::new();
        let owner = env.actors.owner.insecure_clone();
        let (usdc, win) = (env.usdc_mint, env.win_mint);
        let mut p = env.valid_init_params();
        match role {
            "owner" => p.owner = Pubkey::default(),
            "admin" => p.admin = Pubkey::default(),
            "operator" => p.operator = Pubkey::default(),
            _ => unreachable!(),
        }
        let r = send_initialize(&mut env, &owner, usdc, win, &p);
        assert_eq!(
            custom_error_code(&r),
            Some(err(SaleError::InvalidAuthority)),
            "a zero {role} → InvalidAuthority, the result: {r:?}"
        );
        assert!(
            !env.sale_state_exists(),
            "the singleton was not created (the {role} role)"
        );
    }
}

// ===================================================================
// NEGATIVE: a double initialize (the structural init guard)
// ===================================================================

#[test]
fn double_initialize_reverts_and_keeps_first_state() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let p1 = env.valid_init_params();

    // The first init - success.
    send_initialize(&mut env, &owner, usdc, win, &p1).expect("the first initialize passes");
    let first = env.sale_state();

    // A second init with DIFFERENT values (new roles, a new price). It must revert.
    env.expire_blockhash(); // a fresh blockhash, otherwise AlreadyProcessed before execution
    let mut p2 = p1;
    p2.price_usdc = p1.price_usdc + 1;
    p2.admin = Pubkey::new_unique();
    let r = send_initialize(&mut env, &owner, usdc, win, &p2);

    // FACT (observed on LiteSVM / Anchor 1.0.2): a repeated init of an account through
    // `#[account(init)]` is translated by Anchor into a CPI to the System Program's
    // `Allocate`/`CreateAccount`, which on an already existing account answers "account
    // already in use" = `Custom(0)` FROM the System Program (not from sale!). The full
    // log:
    //   "Allocate: account ... already in use"
    //   "Program 11111111111111111111111111111111 failed: custom program error: 0x0"
    //   and the transaction's final error: InstructionError(0, Custom(0)).
    // So this is NOT one of the Sale program's custom 6xxx codes but system code 0 -
    // Anchor's structural init guard fired BEFORE the handler's body.
    assert!(
        r.is_err(),
        "the second initialize must revert, the result: {r:?}"
    );
    let failed = r.as_ref().expect_err("the double-init revert");
    assert!(
        matches!(
            failed.err,
            TransactionError::InstructionError(_, InstructionError::Custom(0))
        ),
        "double-init → System `account already in use` = InstructionError(_, Custom(0)): {:?}",
        failed.err
    );

    // The state was NOT overwritten by the second init: it holds the first one's values.
    let after = env.sale_state();
    assert_eq!(
        after.price_usdc, first.price_usdc,
        "price was not overwritten"
    );
    assert_eq!(after.admin, first.admin, "admin was not overwritten");
    assert_eq!(after.owner, first.owner, "owner was not overwritten");
}
