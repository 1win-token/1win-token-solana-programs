//! A participant's claim: `claim` - Arch §9.10, §11; TRD §5.7. A single instruction
//! atomically releases the $1WIN, returns the excess USDC (when the refund is > 0)
//! and unconditionally closes the aggregate. It completes the M1 vertical slice.
//!
//! tests-bind-to-real-code: a real instruction; the allocation reference values are
//! literals derived by hand (the derivation lives in the constants' comments); the
//! assertions run against observable ATA and vault balances and the fact that the
//! aggregate was closed. The economics are scaled down (a small emission) so
//! oversubscription is reachable with two purchases rather than hundreds; the exact
//! E1-E3 reference values are covered by the `math.rs` unit tests.
//!
//! The invariants under test:
//! - I5 (§3): an aggregate is spent exactly once; a repeated claim →
//!   `AccountNotInitialized`.
//! - I3 (§3): the tokens released are ≤ distributable; token_vault drains without a
//!   shortfall.
//! - I4 (§3): under oversubscription the refunds are paid out of refund_vault; after
//!   all the claims exactly the rounding reserve remains (§10.7).
//! - Fail-closed (§9.10): a refund > 0 without refund_vault / the USDC ATA → revert.

use anchor_lang::error::ERROR_CODE_OFFSET;
use sale::errors::SaleError;
use sale_test_common::{
    activate_claim_ix, aggregate_pda, blacklist_add_ix, claim_ix, claim_ix_full,
    expected::DEFAULT_PRICE_USDC, init_aggregate_ix, initialize_ix, pause_ix, purchase_ix,
    refund_vault_pda, token_vault_pda, wallet_token_ata, wallet_usdc_ata, InitParams, TestEnv,
};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

// ----- The reference economics of the claim (derived by hand) -----

/// The price $0.03 = 30_000 USDC units per whole $1WIN (valid: 0 < price < 10^8).
const PRICE: u64 = DEFAULT_PRICE_USDC;
/// An emission of 1000 $1WIN = 10^11 token units (kept small so oversubscription is
/// reachable).
const EMISSION: u64 = 100_000_000_000;
/// Soft Cap = floor(EMISSION × PRICE / 10^8) = 10^11 × 30_000 / 10^8 = 30_000_000 ($30).
const SOFT_CAP: u64 = 30_000_000;

// --- Oversubscribed: 2 wallets × $20, a pool of $40 > the soft cap of $30 ---
const OVER_DEP: u64 = 20_000_000;
/// tokens = floor(net × EMISSION / pool) = floor(20_000_000 × 10^11 / 40_000_000)
/// = floor(2×10^18 / 4×10^7) = 5×10^10 = 50_000_000_000 token-units (= 500 $1WIN).
const OVER_TOKENS_PER: u64 = 50_000_000_000;
/// spent = ceil(tokens × PRICE / 10^8) = ceil(5×10^10 × 30_000 / 10^8)
/// = ceil(1.5×10^15 / 10^8) = 15_000_000 ($15); refund = net − spent = $5.
const OVER_REFUND_PER: u64 = 5_000_000;
/// refund_vault at activation = pool − (soft_cap − reserve); reserve = participants(2).
/// = 40_000_000 − (30_000_000 − 2) = 10_000_002. After the two $5 refunds exactly the
/// reserve = 2 remains (§10.7).
const OVER_RESERVE_DUST: u64 = 2;

// --- Undersubscribed: 2 wallets × $10, a pool of $20 ≤ the soft cap of $30 ---
const UNDER_DEP: u64 = 10_000_000;
/// tokens = floor(net × 10^8 / PRICE) = floor(10_000_000 × 10^8 / 30_000)
/// = floor(10^15 / 3×10^4) = 33_333_333_333 token-units; refund = 0.
const UNDER_TOKENS_PER: u64 = 33_333_333_333;
/// distributable = floor(pool × 10^8 / PRICE) = floor(20_000_000 × 10^8 / 30_000)
/// = 66_666_666_666 = 2 × per-wallet (the whole pool is handed out, the dust goes to
/// the sweep).
const UNDER_DIST: u64 = 66_666_666_666;

fn err(variant: SaleError) -> u32 {
    ERROR_CODE_OFFSET + variant as u32
}

fn custom(result: &litesvm::types::TransactionResult) -> Option<u32> {
    sale_test_common::custom_error_code(result)
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

fn params_with_emission(env: &TestEnv, emission: u64) -> InitParams {
    let mut p = env.valid_init_params();
    p.price_usdc = PRICE;
    p.emission = emission;
    p // min = $10, max = $10000 - the $10/$20 deposits are within bounds
}

fn init_with(env: &mut TestEnv, p: &InitParams) {
    let owner = env.actors.owner.insecure_clone();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    send(env, initialize_ix(owner.pubkey(), usdc, win, p), &owner).expect("initialize");
}

/// Initializes the sale, enters Sale and makes one deposit from each wallet.
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

fn fund_token_vault(env: &mut TestEnv, amount: u64) {
    let win = env.win_mint;
    env.mint_to(&win, &token_vault_pda(), amount);
}

/// The full path into the Claim phase: the pool is collected, `token_vault` is
/// funded, the pool is fixed (`activate_claim`) and time is warped to
/// `claim_start_date`.
fn reach_claim(
    env: &mut TestEnv,
    p: &InitParams,
    deposits: &[(&Keypair, u64)],
    token_vault_fund: u64,
    oversubscribed: bool,
) {
    build_pool_in_sale(env, p, deposits);
    env.warp_to(p.sale_end_date);
    fund_token_vault(env, token_vault_fund);
    let admin = env.actors.admin.insecure_clone();
    let usdc = env.usdc_mint;
    let refund_vault = oversubscribed.then(refund_vault_pda);
    env.expire_blockhash();
    send(
        env,
        activate_claim_ix(admin.pubkey(), usdc, refund_vault),
        &admin,
    )
    .expect("activate_claim");
    env.warp_to(p.claim_start_date); // the Claim phase
}

// ===================================================================
// The POSITIVE oversubscribed case: tokens + refund + closing the aggregate
// (I3/I4/I5)
// ===================================================================

#[test]
fn claim_oversubscribed_pays_tokens_and_refund_closes_aggregate() {
    let mut env = TestEnv::new();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    reach_claim(
        &mut env,
        &p,
        &[(&a, OVER_DEP), (&b, OVER_DEP)],
        EMISSION,
        true,
    );
    assert_eq!(
        env.token_balance(&refund_vault_pda()),
        40_000_000 - (SOFT_CAP - 2)
    );

    // --- Wallet A claims: tokens plus a refund, and the aggregate is closed ---
    let usdc_before = env.token_balance(&wallet_usdc_ata(&a.pubkey(), &usdc));
    env.expire_blockhash();
    send(&mut env, claim_ix(a.pubkey(), usdc, win, true), &a).expect("claim A (oversubscribed)");

    assert_eq!(
        env.token_balance(&wallet_token_ata(&a.pubkey(), &win)),
        OVER_TOKENS_PER,
        "A received 500 $1WIN"
    );
    assert_eq!(
        env.token_balance(&wallet_usdc_ata(&a.pubkey(), &usdc)) - usdc_before,
        OVER_REFUND_PER,
        "A received a $5 refund"
    );
    assert!(
        !env.aggregate_exists(&a.pubkey()),
        "A's aggregate was closed unconditionally (the I5 replay guard)"
    );
    // A's deposit is NOT touched by the claim (a batch closes it, §9.13).
    assert!(env.deposit_exists(&a.pubkey(), 0), "A's deposit remains");

    // --- Wallet B claims: the last claimer is solvent ---
    env.expire_blockhash();
    send(&mut env, claim_ix(b.pubkey(), usdc, win, true), &b).expect("claim B (the last one)");
    assert_eq!(
        env.token_balance(&wallet_token_ata(&b.pubkey(), &win)),
        OVER_TOKENS_PER
    );

    // I3: token_vault drained with no shortfall (2 × 500 $1WIN == emission).
    assert_eq!(
        env.token_balance(&token_vault_pda()),
        0,
        "token_vault was drained"
    );
    // I4/§10.7: refund_vault is left holding exactly the rounding reserve.
    assert_eq!(
        env.token_balance(&refund_vault_pda()),
        OVER_RESERVE_DUST,
        "refund_vault: the rounding reserve remains"
    );
    // pool_fixed_amount is frozen and closing the aggregates does not touch it (I8).
    assert_eq!(env.sale_state().pool_fixed_amount, 40_000_000);
}

// ===================================================================
// The POSITIVE undersubscribed case: tokens only, with no refund_vault or USDC ATA
// (I3)
// ===================================================================

#[test]
fn claim_undersubscribed_pays_tokens_only_closes_aggregate() {
    let mut env = TestEnv::new();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    reach_claim(
        &mut env,
        &p,
        &[(&a, UNDER_DEP), (&b, UNDER_DEP)],
        UNDER_DIST,
        false,
    );

    // Undersubscribed: refund == 0 → refund_vault and the USDC ATA are NOT passed
    // (with_refund=false).
    env.expire_blockhash();
    send(&mut env, claim_ix(a.pubkey(), usdc, win, false), &a).expect("claim A (undersubscribed)");
    assert_eq!(
        env.token_balance(&wallet_token_ata(&a.pubkey(), &win)),
        UNDER_TOKENS_PER,
        "A received the tokens by the undersubscribed formula"
    );
    assert!(
        !env.aggregate_exists(&a.pubkey()),
        "A's aggregate was closed"
    );
    // There is no refund: A's USDC ATA (which exists from the purchase) stays at zero -
    // an undersubscribed claim credits no USDC.
    assert_eq!(
        env.token_balance(&wallet_usdc_ata(&a.pubkey(), &usdc)),
        0,
        "an undersubscribed claim credited no USDC"
    );

    env.expire_blockhash();
    send(&mut env, claim_ix(b.pubkey(), usdc, win, false), &b).expect("claim B");
    // I3: the whole distributable was handed out with no shortfall.
    assert_eq!(
        env.token_balance(&token_vault_pda()),
        0,
        "token_vault was drained"
    );
    // refund_vault was never created when undersubscribed.
    assert!(
        !env.account_exists(&refund_vault_pda()),
        "refund_vault is absent"
    );
}

// ===================================================================
// I5: a repeated claim fails on the closed aggregate (AccountNotInitialized)
// ===================================================================

#[test]
fn claim_twice_rejected_account_not_initialized() {
    let mut env = TestEnv::new();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    reach_claim(
        &mut env,
        &p,
        &[(&a, UNDER_DEP), (&b, UNDER_DEP)],
        UNDER_DIST,
        false,
    );

    env.expire_blockhash();
    send(&mut env, claim_ix(a.pubkey(), usdc, win, false), &a).expect("the first claim passes");
    assert!(
        !env.aggregate_exists(&a.pubkey()),
        "the aggregate was closed"
    );

    // The repeat WITH A FRESH blockhash: the aggregate is gone → AccountNotInitialized
    // (3012).
    env.expire_blockhash();
    let r = send(&mut env, claim_ix(a.pubkey(), usdc, win, false), &a);
    assert_eq!(
        custom(&r),
        Some(3012),
        "a repeated claim on the closed aggregate → AccountNotInitialized: {r:?}"
    );
}

// ===================================================================
// The deadline boundaries (§4.2, I11) - a single source in current_phase()
// ===================================================================

#[test]
fn claim_before_claim_start_date_rejected_wrong_phase() {
    // The pool is fixed but now < claim_start_date → the SaleEnded phase → WrongPhase.
    let mut env = TestEnv::new();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    build_pool_in_sale(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);
    env.warp_to(p.sale_end_date);
    fund_token_vault(&mut env, UNDER_DIST);
    let admin = env.actors.admin.insecure_clone();
    env.expire_blockhash();
    send(
        &mut env,
        activate_claim_ix(admin.pubkey(), usdc, None),
        &admin,
    )
    .expect("activate");
    // We do NOT warp to claim_start_date - we stay in SaleEnded (pool_fixed).

    env.expire_blockhash();
    let r = send(&mut env, claim_ix(a.pubkey(), usdc, win, false), &a);
    assert_eq!(
        custom(&r),
        Some(err(SaleError::WrongPhase)),
        "a claim before claim_start_date → WrongPhase: {r:?}"
    );
    assert!(
        env.aggregate_exists(&a.pubkey()),
        "the aggregate was not closed (revert)"
    );
}

#[test]
fn claim_boundary_start_minus_one_denied_at_start_ok() {
    // The claim_start_date boundary on SVM: at t−1 the phase is still SaleEnded →
    // WrongPhase; at t the phase is Claim and the claim passes.
    let mut env = TestEnv::new();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    build_pool_in_sale(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);
    env.warp_to(p.sale_end_date);
    fund_token_vault(&mut env, UNDER_DIST);
    let admin = env.actors.admin.insecure_clone();
    env.expire_blockhash();
    send(
        &mut env,
        activate_claim_ix(admin.pubkey(), usdc, None),
        &admin,
    )
    .expect("activate");

    // t = claim_start_date − 1: SaleEnded → WrongPhase.
    env.warp_to(p.claim_start_date - 1);
    env.expire_blockhash();
    let r = send(&mut env, claim_ix(a.pubkey(), usdc, win, false), &a);
    assert_eq!(
        custom(&r),
        Some(err(SaleError::WrongPhase)),
        "a claim at claim_start_date−1 → WrongPhase: {r:?}"
    );
    assert!(
        env.aggregate_exists(&a.pubkey()),
        "the aggregate was not closed (revert)"
    );

    // t = claim_start_date: Claim → it passes.
    env.warp_to(p.claim_start_date);
    env.expire_blockhash();
    send(&mut env, claim_ix(a.pubkey(), usdc, win, false), &a)
        .expect("a claim exactly at claim_start_date passes");
    assert_eq!(
        env.token_balance(&wallet_token_ata(&a.pubkey(), &win)),
        UNDER_TOKENS_PER,
        "the tokens were released on the boundary"
    );
    assert!(
        !env.aggregate_exists(&a.pubkey()),
        "the aggregate was closed"
    );
}

#[test]
fn claim_at_last_second_before_deadline_ok() {
    // The t−1 boundary: at claim_deadline−1 the phase is still Claim and the claim
    // passes.
    let mut env = TestEnv::new();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    reach_claim(
        &mut env,
        &p,
        &[(&a, UNDER_DEP), (&b, UNDER_DEP)],
        UNDER_DIST,
        false,
    );
    let deadline = p.claim_start_date + p.claim_duration;
    env.warp_to(deadline - 1);

    env.expire_blockhash();
    send(&mut env, claim_ix(a.pubkey(), usdc, win, false), &a)
        .expect("a claim at deadline−1 passes");
    assert_eq!(
        env.token_balance(&wallet_token_ata(&a.pubkey(), &win)),
        UNDER_TOKENS_PER
    );
}

#[test]
fn claim_after_deadline_rejected() {
    // The t boundary: at claim_deadline the phase is ClaimClosed → ClaimDeadlinePassed.
    let mut env = TestEnv::new();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    reach_claim(
        &mut env,
        &p,
        &[(&a, UNDER_DEP), (&b, UNDER_DEP)],
        UNDER_DIST,
        false,
    );
    let deadline = p.claim_start_date + p.claim_duration;
    env.warp_to(deadline);

    env.expire_blockhash();
    let r = send(&mut env, claim_ix(a.pubkey(), usdc, win, false), &a);
    assert_eq!(
        custom(&r),
        Some(err(SaleError::ClaimDeadlinePassed)),
        "a claim at the deadline → ClaimDeadlinePassed (a different code from WrongPhase): {r:?}"
    );
    assert!(
        env.aggregate_exists(&a.pubkey()),
        "the aggregate was not closed (revert)"
    );
}

// ===================================================================
// NothingToClaim: a wallet with an aggregate but a zero contribution (§10.6)
// ===================================================================

#[test]
fn claim_with_zero_net_rejected_nothing_to_claim() {
    let mut env = TestEnv::new();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let a = env.actors.users[0].insecure_clone();
    // c initializes an aggregate but does NOT buy (net == 0).
    let c = env.actors.users[2].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    init_with(&mut env, &p);
    env.warp_to(p.sale_start_date);
    // c: only init_aggregate (a participant with a zero contribution).
    env.expire_blockhash();
    send(&mut env, init_aggregate_ix(c.pubkey()), &c).expect("init_aggregate c");
    // a: a real contribution, so that the pool is not empty.
    let ata = funded_ata(&mut env, &a.pubkey(), UNDER_DEP);
    env.expire_blockhash();
    send(&mut env, init_aggregate_ix(a.pubkey()), &a).expect("init_aggregate a");
    env.expire_blockhash();
    send(&mut env, purchase_ix(a.pubkey(), ata, 0, UNDER_DEP), &a).expect("purchase a");

    env.warp_to(p.sale_end_date);
    // distributable for a $10 pool: floor(10_000_000 × 10^8 / 30_000) = 33_333_333_333.
    fund_token_vault(&mut env, 33_333_333_333);
    let admin = env.actors.admin.insecure_clone();
    env.expire_blockhash();
    send(
        &mut env,
        activate_claim_ix(admin.pubkey(), usdc, None),
        &admin,
    )
    .expect("activate");
    env.warp_to(p.claim_start_date);

    env.expire_blockhash();
    let r = send(&mut env, claim_ix(c.pubkey(), usdc, win, false), &c);
    assert_eq!(
        custom(&r),
        Some(err(SaleError::NothingToClaim)),
        "a claim with net == 0 → NothingToClaim: {r:?}"
    );
    assert!(
        env.aggregate_exists(&c.pubkey()),
        "c's aggregate lives until the deadline (§5.2)"
    );
}

// ===================================================================
// Fail-closed: a refund > 0 without refund_vault / the USDC ATA (§9.10)
// ===================================================================

#[test]
fn claim_oversubscribed_without_refund_vault_rejected() {
    let mut env = TestEnv::new();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    reach_claim(
        &mut env,
        &p,
        &[(&a, OVER_DEP), (&b, OVER_DEP)],
        EMISSION,
        true,
    );

    // refund > 0, but neither refund_vault nor the USDC ATA was passed →
    // RefundVaultMissing.
    env.expire_blockhash();
    let r = send(&mut env, claim_ix(a.pubkey(), usdc, win, false), &a);
    assert_eq!(
        custom(&r),
        Some(err(SaleError::RefundVaultMissing)),
        "a refund > 0 without refund_vault → RefundVaultMissing: {r:?}"
    );
    assert!(
        env.aggregate_exists(&a.pubkey()),
        "the aggregate was not closed (revert)"
    );
    // The tokens were not released either: the transaction rolled back entirely.
    assert!(
        !env.account_exists(&wallet_token_ata(&a.pubkey(), &win)),
        "the revert rolled back the token release too"
    );
}

#[test]
fn claim_oversubscribed_without_usdc_ata_rejected() {
    let mut env = TestEnv::new();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    reach_claim(
        &mut env,
        &p,
        &[(&a, OVER_DEP), (&b, OVER_DEP)],
        EMISSION,
        true,
    );

    // refund_vault was passed but the USDC ATA was not → RefundAtaMissing.
    env.expire_blockhash();
    let ix = claim_ix_full(
        a.pubkey(),
        aggregate_pda(&a.pubkey()),
        usdc,
        win,
        Some(refund_vault_pda()),
        None,
    );
    let r = send(&mut env, ix, &a);
    assert_eq!(
        custom(&r),
        Some(err(SaleError::RefundAtaMissing)),
        "a refund > 0 without the USDC ATA → RefundAtaMissing: {r:?}"
    );
    assert!(
        env.aggregate_exists(&a.pubkey()),
        "the aggregate was not closed (revert)"
    );
}

// ===================================================================
// A foreign signer: claiming another wallet's aggregate (§9.10, "the owner only")
// ===================================================================

#[test]
fn claim_by_foreign_signer_rejected() {
    let mut env = TestEnv::new();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    reach_claim(
        &mut env,
        &p,
        &[(&a, OVER_DEP), (&b, OVER_DEP)],
        EMISSION,
        true,
    );

    // b signs but substitutes A's aggregate. seeds = [aggregate, b] ≠ the address of A's
    // aggregate → ConstraintSeeds (2006); a foreign claim is impossible by construction.
    env.expire_blockhash();
    let ix = claim_ix_full(
        b.pubkey(),
        aggregate_pda(&a.pubkey()),
        usdc,
        win,
        Some(refund_vault_pda()),
        Some(wallet_usdc_ata(&b.pubkey(), &usdc)),
    );
    let r = send(&mut env, ix, &b);
    assert_eq!(
        custom(&r),
        Some(2006),
        "a foreign aggregate under one's own signature → ConstraintSeeds: {r:?}"
    );
    assert!(
        env.aggregate_exists(&a.pubkey()),
        "A's aggregate is untouched"
    );
}

// ===================================================================
// A pause does not affect claim (TS-10): claim does not check paused
// ===================================================================

#[test]
fn claim_unaffected_by_pause() {
    // The pause is set in Sale and deliberately not lifted - a lingering paused blocks
    // neither the activation nor the claim.
    let mut env = TestEnv::new();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let admin = env.actors.admin.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    build_pool_in_sale(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);
    env.expire_blockhash();
    send(&mut env, pause_ix(admin.pubkey()), &admin).expect("pause in Sale");

    env.warp_to(p.sale_end_date);
    fund_token_vault(&mut env, UNDER_DIST);
    env.expire_blockhash();
    send(
        &mut env,
        activate_claim_ix(admin.pubkey(), usdc, None),
        &admin,
    )
    .expect("activate_claim passes while paused");
    env.warp_to(p.claim_start_date);
    assert!(env.sale_state().paused, "precondition: paused is lingering");

    env.expire_blockhash();
    send(&mut env, claim_ix(a.pubkey(), usdc, win, false), &a)
        .expect("a claim passes while paused");
    assert_eq!(
        env.token_balance(&wallet_token_ata(&a.pubkey(), &win)),
        UNDER_TOKENS_PER,
        "the pause does not affect the claim"
    );
    assert!(
        !env.aggregate_exists(&a.pubkey()),
        "the aggregate was closed"
    );
}

// ===================================================================
// The blacklist does not affect claim (§5.4): claim holds no blacklist slot
// ===================================================================

#[test]
fn claim_unaffected_by_blacklist() {
    let mut env = TestEnv::new();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let operator = env.actors.operator.insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    build_pool_in_sale(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);
    // We block A after the purchase (blacklist_add works at any time).
    env.expire_blockhash();
    send(
        &mut env,
        blacklist_add_ix(operator.pubkey(), a.pubkey()),
        &operator,
    )
    .expect("blacklist A");
    assert!(env.is_blacklisted(&a.pubkey()), "A is blocked");

    env.warp_to(p.sale_end_date);
    fund_token_vault(&mut env, UNDER_DIST);
    env.expire_blockhash();
    let admin = env.actors.admin.insecure_clone();
    send(
        &mut env,
        activate_claim_ix(admin.pubkey(), usdc, None),
        &admin,
    )
    .expect("activate");
    env.warp_to(p.claim_start_date);

    // The blocked A still claims: the claim instruction does not check the blacklist.
    env.expire_blockhash();
    send(&mut env, claim_ix(a.pubkey(), usdc, win, false), &a)
        .expect("a claim by the blocked wallet passes");
    assert_eq!(
        env.token_balance(&wallet_token_ata(&a.pubkey(), &win)),
        UNDER_TOKENS_PER,
        "the blacklist does not affect the claim"
    );
    assert!(
        !env.aggregate_exists(&a.pubkey()),
        "the aggregate was closed"
    );
}
