//! Fixing the pool and splitting the USDC: `activate_claim` - Arch §9.9; TRD §5.6.
//! Admin atomically fixes `pool_fixed_amount`, verifies the token (I12) and USDC (I2)
//! coverage, creates `proceeds_vault` (always) and `refund_vault` (only under
//! oversubscription), and physically distributes the USDC.
//!
//! tests-bind-to-real-code: a real instruction; the economic reference values are
//! literals derived by hand (the derivation lives in the constants' comments); the
//! assertions run against observable vault balances and state fields. The economics
//! here are local (a small emission) so oversubscription is reachable with a small
//! pool; with the default $3.9M soft cap it would have taken hundreds of purchases.
//!
//! The invariants under test:
//! - I14 (§3): oversubscribed - `proceeds + refund == pool_fixed_amount`, with the
//!   reserve in refund_vault; undersubscribed - `proceeds == pool_fixed_amount` and
//!   no refund_vault.
//! - I12 (§3): with no tokens in the vault the activation fails (`VaultNotFunded`).
//! - I2 (§3): `deposit_vault.amount ≥ deposits_usdc_contributed`
//!   (`DepositVaultUnderfunded`).
//! - I8 (§3): the activation happens exactly once.
//! - The claim window: fixation is possible only while `now < claim_start_date`,
//!   otherwise the window collapses (`ClaimStartInPast`); a missed window is
//!   recovered by rescheduling, since the setter's ceiling follows the edit rather
//!   than the sale's end.

use anchor_lang::error::ERROR_CODE_OFFSET;
use sale::errors::SaleError;
use sale::state::MAX_CLAIM_START_DELAY;
use sale_test_common::{
    activate_claim_ix, claim_ix, custom_error_code, deposit_vault_pda,
    expected::DEFAULT_PRICE_USDC, init_aggregate_ix, initialize_ix, proceeds_vault_pda,
    purchase_ix, refund_vault_pda, set_claim_schedule_ix, sweep_token_vault_ix, token_vault_pda,
    wallet_token_ata, InitParams, TestEnv,
};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

// ----- The reference economics of the activation (derived by hand) -----

/// The price $0.03 = 30_000 USDC units per whole $1WIN (valid: 0 < price < 10^8).
const PRICE: u64 = DEFAULT_PRICE_USDC;
/// An emission of 1000 $1WIN = 10^11 token units (kept small so oversubscription is
/// reachable).
const EMISSION: u64 = 100_000_000_000;
/// The soft cap = floor(EMISSION × PRICE / 10^8) = 10^11 × 30_000 / 10^8
/// = 3×10^15 / 10^8 = 30_000_000 USDC units ($30), with no remainder.
const SOFT_CAP: u64 = 30_000_000;

/// Oversubscribed: 2 wallets × $20. The pool is $40 (40_000_000) > the soft cap of $30.
const OVER_DEP: u64 = 20_000_000;
const OVER_POOL: u64 = 40_000_000;
/// proceeds = soft_cap − reserve; reserve = participants(2) × 1 unit = 2.
const OVER_PROCEEDS: u64 = SOFT_CAP - 2;
/// refund = pool − proceeds = 40_000_000 − 29_999_998 = 10_000_002.
const OVER_REFUND: u64 = OVER_POOL - OVER_PROCEEDS;

/// Undersubscribed: 2 wallets × $10. The pool is $20 (20_000_000) ≤ the soft cap of $30.
const UNDER_DEP: u64 = 10_000_000;
const UNDER_POOL: u64 = 20_000_000;
/// distributable(undersubscribed) = floor(pool × 10^8 / price)
/// = floor(20_000_000 × 10^8 / 30_000) = floor(2×10^15 / 3×10^4)
/// = floor(66_666_666_666.66…) = 66_666_666_666 token units.
const UNDER_DIST: u64 = 66_666_666_666;
/// The share of one of two equal undersubscribed contributors:
/// floor(10_000_000 × 10^8 / 30_000) = floor(10^15 / 3×10^4) = 33_333_333_333 token units.
const UNDER_TOKENS_PER_WALLET: u64 = 33_333_333_333;

fn err(variant: SaleError) -> u32 {
    ERROR_CODE_OFFSET + variant as u32
}

fn custom(result: &litesvm::types::TransactionResult) -> Option<u32> {
    custom_error_code(result)
}

#[allow(clippy::result_large_err)]
fn send(
    env: &mut TestEnv,
    ix: solana_instruction::Instruction,
    signer: &Keypair,
) -> litesvm::types::TransactionResult {
    env.send_ixs(&[ix], &signer.pubkey(), &[signer])
}

fn funded_ata(env: &mut TestEnv, wallet: &Pubkey, amount: u64) -> Pubkey {
    let usdc = env.usdc_mint;
    let ata = env.create_ata(wallet, &usdc);
    env.mint_to(&usdc, &ata, amount);
    ata
}

/// Parameters with the local economics (PRICE/EMISSION); the dates are the defaults.
fn params_with_emission(env: &TestEnv, emission: u64) -> InitParams {
    let mut p = env.valid_init_params();
    p.price_usdc = PRICE;
    p.emission = emission;
    p // min = $10, max = $10000 - the $10/$20 deposits are within these bounds
}

fn init_with(env: &mut TestEnv, p: &InitParams) {
    let owner = env.actors.owner.insecure_clone();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    send(env, initialize_ix(owner.pubkey(), usdc, win, p), &owner).expect("initialize");
}

/// Initializes the sale, enters Sale and makes one deposit from each wallet
/// (participants = deposits.len()). It stays in the Sale phase.
fn build_pool_in_sale(env: &mut TestEnv, p: &InitParams, deposits: &[(&Keypair, u64)]) {
    init_with(env, p);
    env.warp_to(p.sale_start_date);
    for (w, amount) in deposits {
        let ata = funded_ata(env, &w.pubkey(), *amount);
        env.expire_blockhash();
        send(env, init_aggregate_ix(w.pubkey()), w).expect("init_aggregate");
        env.expire_blockhash();
        send(env, purchase_ix(w.pubkey(), ata, 0, *amount), w).expect("purchase");
    }
}

/// Mints `amount` $1WIN into `token_vault` (in production: the Owner MS before
/// activation).
fn fund_token_vault(env: &mut TestEnv, amount: u64) {
    let win = env.win_mint;
    env.mint_to(&win, &token_vault_pda(), amount);
}

// ===================================================================
// activate_claim - the POSITIVE oversubscribed case (I14): the proceeds / refund
// split
// ===================================================================

#[test]
fn activate_oversubscribed_splits_proceeds_and_refund() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let admin = env.actors.admin.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    build_pool_in_sale(&mut env, &p, &[(&a, OVER_DEP), (&b, OVER_DEP)]);
    assert_eq!(
        env.sale_state().participants,
        2,
        "2 participants → reserve = 2"
    );
    assert_eq!(
        env.sale_state().deposits_usdc_contributed,
        OVER_POOL,
        "the pool = $40"
    );

    env.warp_to(p.sale_end_date); // SaleEnded
    fund_token_vault(&mut env, EMISSION); // oversubscribed → distributable = the whole emission

    // The entire activation is ONE legacy transaction (a single ix through send): if the
    // test passes, then initializing two vaults plus two transfers fit within the tx
    // limits.
    env.expire_blockhash();
    send(
        &mut env,
        activate_claim_ix(admin.pubkey(), usdc, Some(refund_vault_pda())),
        &admin,
    )
    .expect("activate_claim (oversubscribed) passes");

    // The state: the pool is fixed.
    let st = env.sale_state();
    assert!(st.pool_fixed, "pool_fixed was set");
    assert_eq!(
        st.pool_fixed_amount, OVER_POOL,
        "pool_fixed_amount = the pool"
    );

    // I14: the split across the vaults. proceeds = soft_cap − reserve; refund = the rest.
    assert_eq!(
        env.token_balance(&proceeds_vault_pda()),
        OVER_PROCEEDS,
        "proceeds_vault = soft_cap − reserve"
    );
    assert!(
        env.account_exists(&refund_vault_pda()),
        "refund_vault was created"
    );
    assert_eq!(
        env.token_balance(&refund_vault_pda()),
        OVER_REFUND,
        "refund_vault = pool − proceeds (the refunds plus the reserve)"
    );
    // The I14 identity.
    assert_eq!(
        env.token_balance(&proceeds_vault_pda()) + env.token_balance(&refund_vault_pda()),
        st.pool_fixed_amount,
        "proceeds + refund == pool_fixed_amount"
    );
    // deposit_vault was drained by the split.
    assert_eq!(
        env.token_balance(&deposit_vault_pda()),
        0,
        "deposit_vault was drained"
    );
}

// ===================================================================
// activate_claim - the POSITIVE undersubscribed case (I14): the whole pool goes into
// proceeds, with no refund_vault
// ===================================================================

#[test]
fn activate_undersubscribed_all_to_proceeds_no_refund_vault() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let admin = env.actors.admin.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    build_pool_in_sale(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);
    assert_eq!(
        env.sale_state().deposits_usdc_contributed,
        UNDER_POOL,
        "the pool = $20"
    );

    env.warp_to(p.sale_end_date);
    fund_token_vault(&mut env, UNDER_DIST); // exactly distributable (the I12 boundary: ≥)

    env.expire_blockhash();
    send(
        &mut env,
        activate_claim_ix(admin.pubkey(), usdc, None),
        &admin,
    )
    .expect("activate_claim (undersubscribed) passes");

    let st = env.sale_state();
    assert!(st.pool_fixed, "pool_fixed was set");
    assert_eq!(
        st.pool_fixed_amount, UNDER_POOL,
        "pool_fixed_amount = the pool"
    );
    // I14 (undersubscribed): the whole pool is in proceeds and refund_vault was not
    // created.
    assert_eq!(
        env.token_balance(&proceeds_vault_pda()),
        UNDER_POOL,
        "proceeds_vault holds the whole pool"
    );
    assert!(
        !env.account_exists(&refund_vault_pda()),
        "refund_vault was NOT created (undersubscribed)"
    );
    assert_eq!(
        env.token_balance(&deposit_vault_pda()),
        0,
        "deposit_vault was drained"
    );
}

// ===================================================================
// activate_claim - I12: with no tokens in the vault → VaultNotFunded
// ===================================================================

#[test]
fn activate_rejected_when_token_vault_underfunded() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let admin = env.actors.admin.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    build_pool_in_sale(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);
    env.warp_to(p.sale_end_date);
    // distributable − 1: the tight I12 boundary (a `≥` is required).
    fund_token_vault(&mut env, UNDER_DIST - 1);

    env.expire_blockhash();
    let r = send(
        &mut env,
        activate_claim_ix(admin.pubkey(), usdc, None),
        &admin,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::VaultNotFunded)),
        "token_vault < distributable → VaultNotFunded: {r:?}"
    );
    assert!(!env.sale_state().pool_fixed, "the pool is not fixed");
    assert!(
        !env.account_exists(&proceeds_vault_pda()),
        "proceeds_vault was not created (the revert rolled the init back)"
    );
}

// ===================================================================
// activate_claim - I2: an understated deposit_vault → DepositVaultUnderfunded
// ===================================================================

#[test]
fn activate_rejected_when_deposit_vault_underfunded() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let admin = env.actors.admin.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    build_pool_in_sale(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);
    env.warp_to(p.sale_end_date);
    fund_token_vault(&mut env, UNDER_DIST); // the tokens are fine - the check will reach step 3

    // We forge deposit_vault 1 unit below the accounted pool (unreachable honestly:
    // purchase and refund are symmetric).
    env.set_token_amount(&deposit_vault_pda(), UNDER_POOL - 1);

    env.expire_blockhash();
    let r = send(
        &mut env,
        activate_claim_ix(admin.pubkey(), usdc, None),
        &admin,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::DepositVaultUnderfunded)),
        "deposit_vault < the pool → DepositVaultUnderfunded (a different code from VaultNotFunded): {r:?}"
    );
    assert!(!env.sale_state().pool_fixed, "the pool is not fixed");
}

// ===================================================================
// activate_claim - it happens once (I8): a repeated activation fails
// ===================================================================

#[test]
fn activate_twice_rejected() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let admin = env.actors.admin.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    build_pool_in_sale(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);
    env.warp_to(p.sale_end_date);
    fund_token_vault(&mut env, UNDER_DIST);

    env.expire_blockhash();
    send(
        &mut env,
        activate_claim_ix(admin.pubkey(), usdc, None),
        &admin,
    )
    .expect("the first activation passes");
    assert!(env.sale_state().pool_fixed, "the pool is fixed");
    let fixed = env.sale_state().pool_fixed_amount;

    // The repeat WITH A FRESH blockhash: the init of proceeds_vault fails on the already
    // existing account (System "account already in use" = Custom(0)) before the body.
    env.expire_blockhash();
    let r = send(
        &mut env,
        activate_claim_ix(admin.pubkey(), usdc, None),
        &admin,
    );
    assert_eq!(
        custom(&r),
        Some(0),
        "a repeated activation → the init of proceeds_vault on an existing account = Custom(0): {r:?}"
    );
    assert!(env.sale_state().pool_fixed, "the pool stays fixed");
    assert_eq!(
        env.sale_state().pool_fixed_amount,
        fixed,
        "pool_fixed_amount is unchanged (I8)"
    );
}

// ===================================================================
// activate_claim - the phase (§4.2): SaleEnded only
// ===================================================================

#[test]
fn activate_in_sale_rejected_wrong_phase() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let admin = env.actors.admin.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    build_pool_in_sale(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);
    // We do NOT warp to sale_end_date - we stay in Sale.
    fund_token_vault(&mut env, UNDER_DIST);

    env.expire_blockhash();
    let r = send(
        &mut env,
        activate_claim_ix(admin.pubkey(), usdc, None),
        &admin,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::WrongPhase)),
        "an activation in Sale → WrongPhase: {r:?}"
    );
    assert!(!env.sale_state().pool_fixed, "the pool is not fixed");
}

#[test]
fn activate_in_presale_rejected_wrong_phase() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let admin = env.actors.admin.insecure_clone();
    let p = params_with_emission(&env, EMISSION);
    init_with(&mut env, &p);
    // PreSale: now < sale_start_date, and there is no pool.
    fund_token_vault(&mut env, UNDER_DIST);

    let r = send(
        &mut env,
        activate_claim_ix(admin.pubkey(), usdc, None),
        &admin,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::WrongPhase)),
        "an activation in PreSale → WrongPhase: {r:?}"
    );
    assert!(!env.sale_state().pool_fixed, "the pool is not fixed");
}

// ===================================================================
// activate_claim - the role (§8): Admin only
// ===================================================================

#[test]
fn activate_by_non_admin_rejected() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let operator = env.actors.operator.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    build_pool_in_sale(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);
    env.warp_to(p.sale_end_date);
    fund_token_vault(&mut env, UNDER_DIST);

    // operator in the admin slot → has_one admin → Unauthorized.
    env.expire_blockhash();
    let r = send(
        &mut env,
        activate_claim_ix(operator.pubkey(), usdc, None),
        &operator,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::Unauthorized)),
        "an activation by a non-admin → Unauthorized: {r:?}"
    );
    assert!(!env.sale_state().pool_fixed, "the pool is not fixed");
}

// ===================================================================
// activate_claim - refund_vault ⟺ oversubscription must agree (fail-closed, I14)
// ===================================================================

#[test]
fn activate_oversubscribed_without_refund_vault_rejected() {
    // Oversubscribed but refund_vault was not passed → RefundVaultMismatch.
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let admin = env.actors.admin.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    build_pool_in_sale(&mut env, &p, &[(&a, OVER_DEP), (&b, OVER_DEP)]);
    env.warp_to(p.sale_end_date);
    fund_token_vault(&mut env, EMISSION);

    env.expire_blockhash();
    let r = send(
        &mut env,
        activate_claim_ix(admin.pubkey(), usdc, None),
        &admin,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::RefundVaultMismatch)),
        "oversubscribed with no refund_vault → RefundVaultMismatch: {r:?}"
    );
    assert!(!env.sale_state().pool_fixed, "the pool is not fixed");
    assert!(
        !env.account_exists(&proceeds_vault_pda()),
        "the revert rolled back the init of proceeds_vault"
    );
}

#[test]
fn activate_undersubscribed_with_refund_vault_rejected() {
    // Undersubscribed but refund_vault was passed → RefundVaultMismatch (we do not
    // create an empty vault).
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let admin = env.actors.admin.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    build_pool_in_sale(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);
    env.warp_to(p.sale_end_date);
    fund_token_vault(&mut env, UNDER_DIST);

    env.expire_blockhash();
    let r = send(
        &mut env,
        activate_claim_ix(admin.pubkey(), usdc, Some(refund_vault_pda())),
        &admin,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::RefundVaultMismatch)),
        "undersubscribed with a refund_vault → RefundVaultMismatch: {r:?}"
    );
    assert!(!env.sale_state().pool_fixed, "the pool is not fixed");
    assert!(
        !env.account_exists(&refund_vault_pda()),
        "the revert rolled back the superfluous init of refund_vault"
    );
}

// ===================================================================
// activate_claim - the claim window still lies ahead: now < claim_start_date
// The schedule is set before the sale while fixation happens after the whole sale is
// over: if by then now has moved past claim_start_date, setting pool_fixed would
// make the phase read as Claim already (part of the window is lost) or ClaimClosed
// (there is no window at all, yet sweep_* is already open). The check lives exactly
// at the moment of fixation. A missed window is recovered by rescheduling: the
// setter's ceiling follows the edit rather than the sale's end, so the way out never
// expires.
// ===================================================================

/// The boundary: exactly `now == claim_start_date` is already too late (a strict `<`
/// is required).
#[test]
fn activate_at_claim_start_date_rejected() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let admin = env.actors.admin.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    build_pool_in_sale(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);
    env.warp_to(p.claim_start_date); // the pool is not fixed → the phase is still SaleEnded
    fund_token_vault(&mut env, UNDER_DIST); // the funding is fine

    env.expire_blockhash();
    let r = send(
        &mut env,
        activate_claim_ix(admin.pubkey(), usdc, None),
        &admin,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::ClaimStartInPast)),
        "fixation exactly at claim_start_date → ClaimStartInPast: {r:?}"
    );
    assert!(!env.sale_state().pool_fixed, "the pool is not fixed");
    assert!(
        !env.account_exists(&proceeds_vault_pda()),
        "the revert rolled back the init of proceeds_vault"
    );
}

/// The deadline has already passed: without the check, fixation would throw the sale
/// straight into ClaimClosed - claiming would be unavailable while sweep_* would
/// already be open to the owner.
#[test]
fn activate_after_claim_deadline_rejected() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let admin = env.actors.admin.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    build_pool_in_sale(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);
    env.warp_to(p.claim_start_date + p.claim_duration + 1);
    fund_token_vault(&mut env, UNDER_DIST);

    env.expire_blockhash();
    let r = send(
        &mut env,
        activate_claim_ix(admin.pubkey(), usdc, None),
        &admin,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::ClaimStartInPast)),
        "fixation past the deadline → ClaimStartInPast: {r:?}"
    );
    assert!(!env.sale_state().pool_fixed, "the pool is not fixed");
}

/// The positive at the tight `claim_start_date − 1` boundary: fixation passes and the
/// claim window is preserved in full - before claim_start_date the phase stays
/// SaleEnded (a claim fails with WrongPhase), and from claim_start_date a claim
/// releases the tokens.
#[test]
fn activate_just_before_claim_start_date_keeps_full_window() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let win = env.win_mint;
    let admin = env.actors.admin.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    build_pool_in_sale(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);
    env.warp_to(p.claim_start_date - 1);
    fund_token_vault(&mut env, UNDER_DIST);

    env.expire_blockhash();
    send(
        &mut env,
        activate_claim_ix(admin.pubkey(), usdc, None),
        &admin,
    )
    .expect("an activation one second before claim_start_date passes");
    assert!(env.sale_state().pool_fixed, "the pool is fixed");

    // After fixation but before claim_start_date it is still SaleEnded.
    env.expire_blockhash();
    let r = send(&mut env, claim_ix(a.pubkey(), usdc, win, false), &a);
    assert_eq!(
        custom(&r),
        Some(err(SaleError::WrongPhase)),
        "a claim before claim_start_date → WrongPhase (the SaleEnded phase): {r:?}"
    );

    // From claim_start_date the window is open and has not been shortened.
    env.warp_to(p.claim_start_date);
    env.expire_blockhash();
    send(&mut env, claim_ix(a.pubkey(), usdc, win, false), &a)
        .expect("a claim from claim_start_date");
    assert_eq!(
        env.token_balance(&wallet_token_ata(&a.pubkey(), &win)),
        UNDER_TOKENS_PER_WALLET,
        "A received its undersubscribed share"
    );
}

/// The way out of a missed window never expires: past the original ceiling
/// (`sale_end_date + MAX_CLAIM_START_DELAY`) the setter still takes a start within
/// 30 days of `now`, the activation then passes, and the window it opens is
/// full-length - the sweep stays closed until exactly `new_start + duration`.
#[test]
fn activate_after_original_ceiling_recovers_by_rescheduling() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let win = env.win_mint;
    let admin = env.actors.admin.insecure_clone();
    let owner = env.actors.owner.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    build_pool_in_sale(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);
    // Past the original ceiling: the stored claim_start_date is behind now.
    env.warp_to(p.sale_end_date + MAX_CLAIM_START_DELAY + 1);
    fund_token_vault(&mut env, UNDER_DIST);

    // NEGATIVE: the stale schedule cannot be activated.
    env.expire_blockhash();
    let r = send(
        &mut env,
        activate_claim_ix(admin.pubkey(), usdc, None),
        &admin,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::ClaimStartInPast)),
        "activation with a stale claim_start_date → ClaimStartInPast: {r:?}"
    );
    assert!(!env.sale_state().pool_fixed, "the pool is not fixed");

    // The reschedule is bounded by now, not by the sale's end.
    let new_start = env.now() + 1_000;
    env.expire_blockhash();
    send(
        &mut env,
        set_claim_schedule_ix(admin.pubkey(), new_start, p.claim_duration),
        &admin,
    )
    .expect("set_claim_schedule past the original ceiling");
    assert_eq!(env.sale_state().claim_start_date, new_start);

    // POSITIVE: the same activation now passes.
    env.expire_blockhash();
    send(
        &mut env,
        activate_claim_ix(admin.pubkey(), usdc, None),
        &admin,
    )
    .expect("activation after the reschedule");
    assert!(env.sale_state().pool_fixed, "the pool is fixed");

    // After fixation but before new_start it is still SaleEnded.
    env.expire_blockhash();
    let r = send(&mut env, claim_ix(a.pubkey(), usdc, win, false), &a);
    assert_eq!(
        custom(&r),
        Some(err(SaleError::WrongPhase)),
        "a claim before the rescheduled start → WrongPhase: {r:?}"
    );

    // From new_start the window is open.
    env.warp_to(new_start);
    env.expire_blockhash();
    send(&mut env, claim_ix(a.pubkey(), usdc, win, false), &a)
        .expect("a claim from the rescheduled start");
    assert_eq!(
        env.token_balance(&wallet_token_ata(&a.pubkey(), &win)),
        UNDER_TOKENS_PER_WALLET,
        "A received its undersubscribed share"
    );

    // One second before the deadline the window is still open and nothing can be
    // swept.
    let deadline = new_start + p.claim_duration;
    env.warp_to(deadline - 1);
    env.expire_blockhash();
    let r = send(
        &mut env,
        sweep_token_vault_ix(owner.pubkey(), win, owner.pubkey()),
        &owner,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::DeadlineNotPassed)),
        "a sweep inside the rescheduled window → DeadlineNotPassed: {r:?}"
    );
    env.expire_blockhash();
    send(&mut env, claim_ix(b.pubkey(), usdc, win, false), &b)
        .expect("a claim one second before the deadline");
    assert_eq!(
        env.token_balance(&wallet_token_ata(&b.pubkey(), &win)),
        UNDER_TOKENS_PER_WALLET,
        "B received its undersubscribed share"
    );

    // ClaimClosed arrives exactly at new_start + duration: the window was full-length.
    env.warp_to(deadline);
    env.expire_blockhash();
    send(
        &mut env,
        sweep_token_vault_ix(owner.pubkey(), win, owner.pubkey()),
        &owner,
    )
    .expect("a sweep at the deadline");
}
