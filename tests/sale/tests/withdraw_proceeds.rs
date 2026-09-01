//! The Owner withdrawing the proceeds: `withdraw_proceeds` - iteration 5; §5.5, §9.9.
//! In one instruction the Owner moves the whole `proceeds_vault` remainder to its own
//! USDC ATA and closes the vault (the rent goes to rent_collector). It is available
//! in the Claim and ClaimClosed phases (a double gate: `pool_fixed` +
//! `now ≥ claim_start_date`, not the enum phase).
//!
//! tests-bind-to-real-code: a real instruction through the builder; the expected
//! `proceeds` are independent literals (soft_cap − participants under
//! oversubscription; pool_fixed_amount when undersubscribed) derived by hand - NOT a
//! recomputation of math.rs; the assertions run against observable balances, the fact
//! of the vault closing and the event's payload.
//!
//! The invariants under test:
//! - §9.9: the ENTIRE proceeds_vault remainder is withdrawn and the vault is closed
//!   (a replay guard).
//! - §5.5: the Owner only; the rent recipient is supplied by the trusted Owner.
//! - Phase: before fixation → PoolNotFixed; before claim_start_date → ClaimNotStarted.
//! - accounts-untrusted (I9): another vault cannot be slipped into the proceeds_vault
//!   slot.

use anchor_lang::error::{ErrorCode as AnchorError, ERROR_CODE_OFFSET};
use sale::errors::SaleError;
use sale::events::ProceedsWithdrawn;
use sale_test_common::{
    activate_claim_ix, custom_error_code, decode_event, deposit_vault_pda,
    expected::DEFAULT_PRICE_USDC, init_aggregate_ix, initialize_ix, proceeds_vault_pda,
    purchase_ix, refund_vault_pda, token_vault_pda, wallet_usdc_ata, withdraw_proceeds_ix,
    InitParams, TestEnv,
};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

// ----- The reference economics (the same scale as in activate_claim.rs / claim.rs) -----

/// The price $0.03 = 30_000 USDC units per whole $1WIN (valid: 0 < price < 10^8).
const PRICE: u64 = DEFAULT_PRICE_USDC;
/// An emission of 1000 $1WIN = 10^11 token units (kept small so oversubscription is
/// reachable).
const EMISSION: u64 = 100_000_000_000;
/// Soft Cap = floor(EMISSION × PRICE / 10^8) = 10^11 × 30_000 / 10^8 = 30_000_000 ($30).
const SOFT_CAP: u64 = 30_000_000;

// --- Oversubscribed: 2 wallets × $20, a pool of $40 > the soft cap of $30 ---
const OVER_DEP: u64 = 20_000_000;
const OVER_POOL: u64 = 40_000_000;
/// proceeds(oversubscribed) = soft_cap − participants = 30_000_000 − 2 = 29_999_998
/// (an independent literal: NOT a recomputation of soft_cap_usdc from math.rs).
const OVER_PROCEEDS: u64 = 29_999_998;
/// Cross-checking two independent literals at compile time (it catches a typo in
/// either one without reproducing the contract's formula).
const _: () = assert!(OVER_PROCEEDS == SOFT_CAP - 2);

// --- Undersubscribed: 2 wallets × $10, a pool of $20 ≤ the soft cap of $30 ---
const UNDER_DEP: u64 = 10_000_000;
const UNDER_POOL: u64 = 20_000_000;
/// distributable(undersubscribed) = floor(20_000_000 × 10^8 / 30_000) = 66_666_666_666.
const UNDER_DIST: u64 = 66_666_666_666;

fn err(variant: SaleError) -> u32 {
    ERROR_CODE_OFFSET + variant as u32
}

fn anchor_err(code: AnchorError) -> u32 {
    code as u32
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
// POSITIVE, oversubscribed: soft_cap − participants is withdrawn and the vault is
// closed (§9.9)
// ===================================================================

#[test]
fn withdraw_oversubscribed_pays_soft_cap_minus_participants_and_closes_vault() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let owner = env.actors.owner.insecure_clone();
    let rent_collector = env.actors.admin.insecure_clone(); // the trusted rent recipient
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

    // Precondition: proceeds_vault holds exactly soft_cap − participants.
    assert_eq!(
        env.token_balance(&proceeds_vault_pda()),
        OVER_PROCEEDS,
        "proceeds_vault = soft_cap − participants"
    );
    let owner_ata = wallet_usdc_ata(&owner.pubkey(), &usdc);

    env.expire_blockhash();
    let r = send(
        &mut env,
        withdraw_proceeds_ix(owner.pubkey(), usdc, rent_collector.pubkey()),
        &owner,
    );
    r.as_ref()
        .expect("withdraw_proceeds (oversubscribed) passes");

    // The Owner received exactly the proceeds; the vault is closed (a replay guard).
    assert_eq!(
        env.token_balance(&owner_ata),
        OVER_PROCEEDS,
        "the Owner received all the proceeds"
    );
    assert!(
        !env.account_exists(&proceeds_vault_pda()),
        "proceeds_vault is closed after the withdrawal"
    );
    // The event's payload: amount == proceeds (an observable quantity, but the event is
    // an effect too).
    let ev: ProceedsWithdrawn = decode_event(&r).expect("the ProceedsWithdrawn event");
    assert_eq!(
        ev.amount, OVER_PROCEEDS,
        "ProceedsWithdrawn.amount == proceeds"
    );

    // refund_vault is untouched by the proceeds withdrawal (I9: only proceeds_vault is
    // reachable).
    let refund_left = env.token_balance(&refund_vault_pda());
    assert_eq!(
        refund_left,
        OVER_POOL - OVER_PROCEEDS,
        "refund_vault is untouched: pool − proceeds stays behind for the refunds"
    );
}

// ===================================================================
// POSITIVE, undersubscribed: the whole pool_fixed_amount is withdrawn and the vault
// is closed (§9.9)
// ===================================================================

#[test]
fn withdraw_undersubscribed_pays_full_pool_and_closes_vault() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let owner = env.actors.owner.insecure_clone();
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

    // Undersubscribed: the whole pool settled in proceeds_vault (refund_vault was never
    // created).
    assert_eq!(
        env.token_balance(&proceeds_vault_pda()),
        UNDER_POOL,
        "proceeds_vault holds the whole pool (undersubscribed)"
    );
    assert_eq!(
        env.sale_state().pool_fixed_amount,
        UNDER_POOL,
        "pool_fixed_amount == the pool"
    );
    let owner_ata = wallet_usdc_ata(&owner.pubkey(), &usdc);

    env.expire_blockhash();
    let r = send(
        &mut env,
        withdraw_proceeds_ix(owner.pubkey(), usdc, owner.pubkey()),
        &owner,
    );
    r.as_ref()
        .expect("withdraw_proceeds (undersubscribed) passes");

    assert_eq!(
        env.token_balance(&owner_ata),
        UNDER_POOL,
        "the Owner received the whole pool"
    );
    assert!(
        !env.account_exists(&proceeds_vault_pda()),
        "proceeds_vault is closed"
    );
    let ev: ProceedsWithdrawn = decode_event(&r).expect("the ProceedsWithdrawn event");
    assert_eq!(
        ev.amount, UNDER_POOL,
        "ProceedsWithdrawn.amount == pool_fixed_amount"
    );
}

// ===================================================================
// POSITIVE in ClaimClosed: the withdrawal is available past the deadline too (Claim
// + ClaimClosed)
// ===================================================================

#[test]
fn withdraw_available_in_claim_closed() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let owner = env.actors.owner.insecure_clone();
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
    // We warp PAST the deadline → ClaimClosed.
    let deadline = p.claim_start_date + p.claim_duration;
    env.warp_to(deadline + 1);
    assert!(
        env.token_balance(&proceeds_vault_pda()) == UNDER_POOL,
        "precondition: the proceeds are still in the vault"
    );

    env.expire_blockhash();
    send(
        &mut env,
        withdraw_proceeds_ix(owner.pubkey(), usdc, owner.pubkey()),
        &owner,
    )
    .expect("withdraw_proceeds in ClaimClosed passes");

    assert_eq!(
        env.token_balance(&wallet_usdc_ata(&owner.pubkey(), &usdc)),
        UNDER_POOL,
        "the Owner withdrew the proceeds in ClaimClosed"
    );
    assert!(
        !env.account_exists(&proceeds_vault_pda()),
        "proceeds_vault is closed"
    );
}

// ===================================================================
// NEG role (§5.5): a non-owner → Unauthorized
// ===================================================================

#[test]
fn withdraw_by_non_owner_rejected_unauthorized() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let admin = env.actors.admin.insecure_clone(); // NOT the owner
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

    // admin in the owner slot → has_one owner → Unauthorized.
    env.expire_blockhash();
    let r = send(
        &mut env,
        withdraw_proceeds_ix(admin.pubkey(), usdc, admin.pubkey()),
        &admin,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::Unauthorized)),
        "a withdrawal by a non-owner → Unauthorized: {r:?}"
    );
    assert!(
        env.account_exists(&proceeds_vault_pda()),
        "proceeds_vault is untouched (revert)"
    );
}

// ===================================================================
// NEG phase: the pool is not fixed (SaleEnded before activation) → PoolNotFixed
// ===================================================================

#[test]
fn withdraw_before_pool_fixed_rejected() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let owner = env.actors.owner.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    // The pool is collected and it is SaleEnded, but activate_claim was NOT called → no
    // proceeds_vault was created.
    build_pool_in_sale(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);
    env.warp_to(p.sale_end_date);
    assert!(
        !env.sale_state().pool_fixed,
        "precondition: the pool is not fixed"
    );
    assert!(
        !env.account_exists(&proceeds_vault_pda()),
        "precondition: proceeds_vault does not exist yet"
    );

    // proceeds_vault is absent → Anchor fails with AccountNotInitialized before the body.
    // That is precisely the "the pool is not fixed" protection at the structural level
    // (only activate_claim creates the vault). The PoolNotFixed body gate is unreachable
    // while the vault has not been created by fixation; we pin the real behaviour with a
    // specific code.
    env.expire_blockhash();
    let r = send(
        &mut env,
        withdraw_proceeds_ix(owner.pubkey(), usdc, owner.pubkey()),
        &owner,
    );
    assert_eq!(
        custom(&r),
        Some(anchor_err(AnchorError::AccountNotInitialized)),
        "a withdrawal before fixation: proceeds_vault is absent → AccountNotInitialized (3012): {r:?}"
    );
}

// ===================================================================
// NEG phase: the pool is fixed but now < claim_start_date → ClaimNotStarted
// ===================================================================

#[test]
fn withdraw_before_claim_start_date_rejected_claim_not_started() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let owner = env.actors.owner.insecure_clone();
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
    .expect("activate");
    // We do NOT warp to claim_start_date: pool_fixed == true but now < claim_start_date.
    assert!(
        env.sale_state().pool_fixed,
        "precondition: the pool is fixed"
    );
    assert!(
        env.now() < env.sale_state().claim_start_date,
        "precondition: now < claim_start_date"
    );

    env.expire_blockhash();
    let r = send(
        &mut env,
        withdraw_proceeds_ix(owner.pubkey(), usdc, owner.pubkey()),
        &owner,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::ClaimNotStarted)),
        "a withdrawal before claim_start_date (the pool being fixed) → ClaimNotStarted: {r:?}"
    );
    assert!(
        env.account_exists(&proceeds_vault_pda()),
        "proceeds_vault is untouched (revert)"
    );
    assert_eq!(
        env.token_balance(&proceeds_vault_pda()),
        UNDER_POOL,
        "the proceeds stayed in the vault"
    );
}

// ===================================================================
// The claim_start_date boundary on SVM: t−1 → ClaimNotStarted, t → passes
// ===================================================================

#[test]
fn withdraw_boundary_claim_start_date_minus_one_denied_at_start_ok() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let owner = env.actors.owner.insecure_clone();
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
    .expect("activate");

    // t = claim_start_date − 1: ClaimNotStarted, the proceeds stay in the vault.
    env.warp_to(p.claim_start_date - 1);
    env.expire_blockhash();
    let r = send(
        &mut env,
        withdraw_proceeds_ix(owner.pubkey(), usdc, owner.pubkey()),
        &owner,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::ClaimNotStarted)),
        "a withdrawal at claim_start_date−1 → ClaimNotStarted: {r:?}"
    );
    assert_eq!(
        env.token_balance(&proceeds_vault_pda()),
        UNDER_POOL,
        "the proceeds stayed in the vault (revert)"
    );

    // t = claim_start_date: it passes and the vault is closed.
    env.warp_to(p.claim_start_date);
    env.expire_blockhash();
    send(
        &mut env,
        withdraw_proceeds_ix(owner.pubkey(), usdc, owner.pubkey()),
        &owner,
    )
    .expect("a withdrawal exactly at claim_start_date passes");
    assert_eq!(
        env.token_balance(&wallet_usdc_ata(&owner.pubkey(), &usdc)),
        UNDER_POOL,
        "the Owner received the proceeds on the boundary"
    );
    assert!(
        !env.account_exists(&proceeds_vault_pda()),
        "proceeds_vault is closed"
    );
}

// ===================================================================
// NEG replay: a repeated withdrawal after the vault is closed →
// AccountNotInitialized
// ===================================================================

#[test]
fn withdraw_twice_rejected_account_not_initialized() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let owner = env.actors.owner.insecure_clone();
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
    send(
        &mut env,
        withdraw_proceeds_ix(owner.pubkey(), usdc, owner.pubkey()),
        &owner,
    )
    .expect("the first withdrawal passes");
    assert!(
        !env.account_exists(&proceeds_vault_pda()),
        "the vault was closed by the first withdrawal"
    );
    let after_first = env.token_balance(&wallet_usdc_ata(&owner.pubkey(), &usdc));

    // The repeat WITH A FRESH blockhash: the vault is gone → AccountNotInitialized
    // (3012).
    env.expire_blockhash();
    let r = send(
        &mut env,
        withdraw_proceeds_ix(owner.pubkey(), usdc, owner.pubkey()),
        &owner,
    );
    assert_eq!(
        custom(&r),
        Some(anchor_err(AnchorError::AccountNotInitialized)),
        "a repeated withdrawal from the closed vault → AccountNotInitialized (3012): {r:?}"
    );
    assert_eq!(
        env.token_balance(&wallet_usdc_ata(&owner.pubkey(), &usdc)),
        after_first,
        "the repeat did not hand out the proceeds a second time"
    );
}

// ===================================================================
// NEG vault substitution (accounts-untrusted, I9): refund_vault in the
// proceeds_vault slot
// ===================================================================

#[test]
fn withdraw_with_refund_vault_in_proceeds_slot_rejected_constraint_seeds() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let owner = env.actors.owner.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    // Oversubscribed: refund_vault EXISTS and holds funds - a tempting target.
    reach_claim(
        &mut env,
        &p,
        &[(&a, OVER_DEP), (&b, OVER_DEP)],
        EMISSION,
        true,
    );
    let refund_before = env.token_balance(&refund_vault_pda());
    assert!(refund_before > 0, "precondition: refund_vault is not empty");

    // We assemble the ix by hand: refund_vault is slipped into the proceeds_vault slot
    // (seeds = [b"proceeds_vault"]). The address does not derive from the
    // "proceeds_vault" seeds → ConstraintSeeds; draining refund_vault through the
    // proceeds slot is impossible.
    let mut ix = withdraw_proceeds_ix(owner.pubkey(), usdc, owner.pubkey());
    // proceeds_vault is the 3rd account of the context (sale_state, owner,
    // proceeds_vault, ...).
    let slot = ix
        .accounts
        .iter_mut()
        .find(|m| m.pubkey == proceeds_vault_pda())
        .expect("the proceeds_vault slot");
    slot.pubkey = refund_vault_pda();

    env.expire_blockhash();
    let r = send(&mut env, ix, &owner);
    assert_eq!(
        custom(&r),
        Some(anchor_err(AnchorError::ConstraintSeeds)),
        "refund_vault in the proceeds_vault slot → ConstraintSeeds (2006): {r:?}"
    );
    assert_eq!(
        env.token_balance(&refund_vault_pda()),
        refund_before,
        "refund_vault is untouched"
    );
    assert!(
        env.account_exists(&proceeds_vault_pda()),
        "proceeds_vault is untouched"
    );
}

// ===================================================================
// NEG vault substitution: deposit_vault (already drained but existing) in the
// proceeds slot
// ===================================================================

#[test]
fn withdraw_with_deposit_vault_in_proceeds_slot_rejected_constraint_seeds() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let owner = env.actors.owner.insecure_clone();
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

    let mut ix = withdraw_proceeds_ix(owner.pubkey(), usdc, owner.pubkey());
    let slot = ix
        .accounts
        .iter_mut()
        .find(|m| m.pubkey == proceeds_vault_pda())
        .expect("the proceeds_vault slot");
    slot.pubkey = deposit_vault_pda();

    env.expire_blockhash();
    let r = send(&mut env, ix, &owner);
    assert_eq!(
        custom(&r),
        Some(anchor_err(AnchorError::ConstraintSeeds)),
        "deposit_vault in the proceeds_vault slot → ConstraintSeeds (2006): {r:?}"
    );
    assert!(
        env.account_exists(&proceeds_vault_pda()),
        "proceeds_vault is untouched"
    );
}
