//! The Owner sweeping the vault remainders: `sweep_{refund,deposit,token}_vault` -
//! iteration 5; §9.14. Three independent instructions: each moves its vault's
//! remainder to the Owner's ATA and closes the vault. All three are gated by the
//! terminal phase ClaimClosed (`pool_fixed && now ≥ claim_deadline`).
//!
//! tests-bind-to-real-code: real instructions through the builders; the expectations
//! are independent literals; the assertions run against observable balances, the fact
//! of the vault closing, and the payload of the `Swept{vault, amount}` event; the
//! negatives assert a SPECIFIC code.
//!
//! The invariants under test:
//! - §9.14: a sweep is available only in ClaimClosed; every instruction is idempotent
//!   (a repeat on a closed vault is a clean revert that does not touch the other
//!   vaults).
//! - accounting-integrity: deposit_vault is swept by its actual balance (stray
//!   donations are collected in full; our own accounting is symmetric).
//! - refund_vault exists only under oversubscription; when undersubscribed the sweep
//!   reverts.
//! - §5.5: the Owner only.

use anchor_lang::error::{ErrorCode as AnchorError, ERROR_CODE_OFFSET};
use sale::errors::SaleError;
use sale::events::Swept;
use sale_test_common::{
    activate_claim_ix, claim_ix, custom_error_code, decode_event, deposit_vault_pda,
    expected::DEFAULT_PRICE_USDC, init_aggregate_ix, initialize_ix, owner_token_ata, purchase_ix,
    refund_vault_pda, sweep_deposit_vault_ix, sweep_refund_vault_ix, sweep_token_vault_ix,
    token_vault_pda, wallet_token_ata, wallet_usdc_ata, InitParams, TestEnv,
};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

// ----- The reference economics (the same scale as in claim.rs) -----

const PRICE: u64 = DEFAULT_PRICE_USDC;
const EMISSION: u64 = 100_000_000_000; // 1000 $1WIN

// --- Oversubscribed: 2 wallets × $20, a pool of $40 ---
const OVER_DEP: u64 = 20_000_000;
const OVER_POOL: u64 = 40_000_000;
const OVER_PROCEEDS: u64 = 29_999_998; // soft_cap(30_000_000) − participants(2)
/// refund_vault at activation = pool − proceeds = 40_000_000 − 29_999_998 = 10_000_002.
const OVER_REFUND_VAULT: u64 = OVER_POOL - OVER_PROCEEDS;
/// tokens per wallet = floor(20_000_000 × 10^11 / 40_000_000) = 50_000_000_000; the refund is $5.
const OVER_TOKENS_PER: u64 = 50_000_000_000;
const OVER_REFUND_PER: u64 = 5_000_000;

// --- Undersubscribed: 2 wallets × $10, a pool of $20 ---
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
    p
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

/// Activates the pool (in SaleEnded) without warping into Claim. Returns the
/// InitParams.
fn activate(
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
}

fn deadline_of(p: &InitParams) -> i64 {
    p.claim_start_date + p.claim_duration
}

// ===================================================================
// NEG gate: a sweep during the Claim phase (now < deadline) → DeadlineNotPassed (all
// three)
// ===================================================================

#[test]
fn sweep_in_claim_phase_rejected_deadline_not_passed() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let win = env.win_mint;
    let owner = env.actors.owner.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    // Oversubscribed → refund_vault exists (otherwise the refund sweep would fail
    // earlier, on init).
    activate(
        &mut env,
        &p,
        &[(&a, OVER_DEP), (&b, OVER_DEP)],
        EMISSION,
        true,
    );
    env.warp_to(p.claim_start_date); // the Claim phase, now < deadline

    for (label, ix) in [
        (
            "refund",
            sweep_refund_vault_ix(owner.pubkey(), usdc, owner.pubkey()),
        ),
        (
            "deposit",
            sweep_deposit_vault_ix(owner.pubkey(), usdc, owner.pubkey()),
        ),
        (
            "token",
            sweep_token_vault_ix(owner.pubkey(), win, owner.pubkey()),
        ),
    ] {
        env.expire_blockhash();
        let r = send(&mut env, ix, &owner);
        assert_eq!(
            custom(&r),
            Some(err(SaleError::DeadlineNotPassed)),
            "sweep_{label}_vault in Claim → DeadlineNotPassed: {r:?}"
        );
    }
    // The vaults are untouched.
    assert!(
        env.account_exists(&refund_vault_pda()),
        "refund_vault is alive"
    );
    assert!(
        env.account_exists(&deposit_vault_pda()),
        "deposit_vault is alive"
    );
    assert!(
        env.account_exists(&token_vault_pda()),
        "token_vault is alive"
    );
}

// ===================================================================
// NEG gate: a sweep BEFORE the pool is fixed → DeadlineNotPassed. Without pool_fixed
// the ClaimClosed phase is unreachable even past the nominal claim deadline - the
// gate rests on fixation rather than on time alone; live deposits cannot be taken
// away.
// ===================================================================

#[test]
fn sweep_before_pool_fixed_rejected_deadline_not_passed() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let win = env.win_mint;
    let owner = env.actors.owner.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    // The pool is collected and activate_claim was NOT called; time is PAST the nominal
    // deadline.
    build_pool_in_sale(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);
    env.warp_to(deadline_of(&p) + 1);
    assert!(
        !env.sale_state().pool_fixed,
        "precondition: the pool is not fixed"
    );

    // The deposit and token vaults exist from init → what is reached is precisely the
    // phase gate.
    for (label, ix) in [
        (
            "deposit",
            sweep_deposit_vault_ix(owner.pubkey(), usdc, owner.pubkey()),
        ),
        (
            "token",
            sweep_token_vault_ix(owner.pubkey(), win, owner.pubkey()),
        ),
    ] {
        env.expire_blockhash();
        let r = send(&mut env, ix, &owner);
        assert_eq!(
            custom(&r),
            Some(err(SaleError::DeadlineNotPassed)),
            "sweep_{label}_vault before fixation → DeadlineNotPassed: {r:?}"
        );
    }
    assert_eq!(
        env.token_balance(&deposit_vault_pda()),
        UNDER_POOL,
        "the deposits in the vault are untouched"
    );

    // refund_vault does not exist before fixation → the structural rejection comes even
    // earlier.
    env.expire_blockhash();
    let r = send(
        &mut env,
        sweep_refund_vault_ix(owner.pubkey(), usdc, owner.pubkey()),
        &owner,
    );
    assert_eq!(
        custom(&r),
        Some(anchor_err(AnchorError::AccountNotInitialized)),
        "sweep_refund_vault before fixation → AccountNotInitialized (3012): {r:?}"
    );
}

// ===================================================================
// NEG role (§5.5): a non-owner → Unauthorized (representatively, on the refund
// sweep)
// ===================================================================

#[test]
fn sweep_by_non_owner_rejected_unauthorized() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let admin = env.actors.admin.insecure_clone(); // NOT the owner
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    activate(
        &mut env,
        &p,
        &[(&a, OVER_DEP), (&b, OVER_DEP)],
        EMISSION,
        true,
    );
    env.warp_to(deadline_of(&p)); // ClaimClosed - the phase is right, we are checking the role

    env.expire_blockhash();
    let r = send(
        &mut env,
        sweep_refund_vault_ix(admin.pubkey(), usdc, admin.pubkey()),
        &admin,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::Unauthorized)),
        "a sweep by a non-owner → Unauthorized: {r:?}"
    );
    assert!(
        env.account_exists(&refund_vault_pda()),
        "refund_vault is untouched"
    );
}

// POSITIVE refund (oversubscribed): the remainder plus the reserve → the Owner; the
// vault is closed (§9.14)
// ===================================================================

#[test]
fn sweep_refund_vault_drains_dust_and_closes() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let win = env.win_mint;
    let owner = env.actors.owner.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    activate(
        &mut env,
        &p,
        &[(&a, OVER_DEP), (&b, OVER_DEP)],
        EMISSION,
        true,
    );
    env.warp_to(p.claim_start_date);

    // Only A claims (receiving a $5 refund); B does NOT claim → refund_vault is left
    // holding B's refund ($5) plus the reserve (2 units).
    env.expire_blockhash();
    send(&mut env, claim_ix(a.pubkey(), usdc, win, true), &a).expect("claim A");
    assert_eq!(
        env.token_balance(&wallet_token_ata(&a.pubkey(), &win)),
        OVER_TOKENS_PER,
        "A received the tokens"
    );
    // refund_vault = the start minus A's payout
    let refund_left = env.token_balance(&refund_vault_pda());
    assert_eq!(
        refund_left,
        OVER_REFUND_VAULT - OVER_REFUND_PER,
        "refund_vault = the start minus A's refund (B's refund plus the reserve are still inside)"
    );

    env.warp_to(deadline_of(&p)); // ClaimClosed
    let owner_ata = wallet_usdc_ata(&owner.pubkey(), &usdc);

    env.expire_blockhash();
    let r = send(
        &mut env,
        sweep_refund_vault_ix(owner.pubkey(), usdc, owner.pubkey()),
        &owner,
    );
    r.as_ref().expect("sweep_refund_vault passes");

    assert_eq!(
        env.token_balance(&owner_ata),
        refund_left,
        "the Owner received the whole refund_vault remainder"
    );
    assert!(
        !env.account_exists(&refund_vault_pda()),
        "refund_vault is closed"
    );
    // The Swept event's payload: vault == refund_vault, amount == the remainder.
    let ev: Swept = decode_event(&r).expect("the Swept event");
    assert_eq!(ev.vault, refund_vault_pda(), "Swept.vault == refund_vault");
    assert_eq!(ev.amount, refund_left, "Swept.amount == the remainder");
}

// ===================================================================
// N/A refund (undersubscribed): refund_vault was never created →
// AccountNotInitialized
// ===================================================================

#[test]
fn sweep_refund_vault_undersubscribed_rejected_account_not_initialized() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let owner = env.actors.owner.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    activate(
        &mut env,
        &p,
        &[(&a, UNDER_DEP), (&b, UNDER_DEP)],
        UNDER_DIST,
        false,
    );
    env.warp_to(deadline_of(&p)); // ClaimClosed
    assert!(
        !env.account_exists(&refund_vault_pda()),
        "precondition: refund_vault was never created (undersubscribed)"
    );

    env.expire_blockhash();
    let r = send(
        &mut env,
        sweep_refund_vault_ix(owner.pubkey(), usdc, owner.pubkey()),
        &owner,
    );
    assert_eq!(
        custom(&r),
        Some(anchor_err(AnchorError::AccountNotInitialized)),
        "sweep_refund_vault when undersubscribed → AccountNotInitialized (3012): {r:?}"
    );
}

// ===================================================================
// POSITIVE deposit: a stray donation is swept by balance; the vault is closed
// ===================================================================

#[test]
fn sweep_deposit_vault_takes_stray_donation() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let owner = env.actors.owner.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    // Undersubscribed: after the activation the whole pool went into proceeds →
    // deposit_vault is empty.
    activate(
        &mut env,
        &p,
        &[(&a, UNDER_DEP), (&b, UNDER_DEP)],
        UNDER_DIST,
        false,
    );
    assert_eq!(
        env.token_balance(&deposit_vault_pda()),
        0,
        "deposit_vault was drained by the activation (the accounting is symmetric)"
    );
    assert_eq!(
        env.sale_state().deposits_usdc_contributed,
        UNDER_POOL,
        "the accounted pool counter does NOT depend on the vault's balance"
    );

    // A stray donation straight to the deposit_vault address (bypassing the program's
    // logic).
    const STRAY: u64 = 777_777;
    env.mint_to(&usdc, &deposit_vault_pda(), STRAY);
    assert_eq!(
        env.token_balance(&deposit_vault_pda()),
        STRAY,
        "the vault holds only the donation"
    );

    env.warp_to(deadline_of(&p)); // ClaimClosed
    let owner_ata = wallet_usdc_ata(&owner.pubkey(), &usdc);
    let before = if env.account_exists(&owner_ata) {
        env.token_balance(&owner_ata)
    } else {
        0
    };

    env.expire_blockhash();
    let r = send(
        &mut env,
        sweep_deposit_vault_ix(owner.pubkey(), usdc, owner.pubkey()),
        &owner,
    );
    r.as_ref().expect("sweep_deposit_vault passes");

    assert_eq!(
        env.token_balance(&owner_ata) - before,
        STRAY,
        "the Owner received EXACTLY the donation"
    );
    assert!(
        !env.account_exists(&deposit_vault_pda()),
        "deposit_vault is closed"
    );
    let ev: Swept = decode_event(&r).expect("the Swept event");
    assert_eq!(
        ev.vault,
        deposit_vault_pda(),
        "Swept.vault == deposit_vault"
    );
    assert_eq!(ev.amount, STRAY, "Swept.amount == the donation");
}

// ===================================================================
// POSITIVE token: the emission remainder after the claims → the Owner; the vault is
// closed
// ===================================================================

#[test]
fn sweep_token_vault_drains_leftover_and_closes() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let win = env.win_mint;
    let owner = env.actors.owner.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    // We fund token_vault with the emission plus 1 extra token unit: after all the
    // claims exactly the emission is handed out (oversubscribed) and the extra unit is
    // left for the sweep.
    const EXTRA: u64 = 1;
    activate(
        &mut env,
        &p,
        &[(&a, OVER_DEP), (&b, OVER_DEP)],
        EMISSION + EXTRA,
        true,
    );
    env.warp_to(p.claim_start_date);

    env.expire_blockhash();
    send(&mut env, claim_ix(a.pubkey(), usdc, win, true), &a).expect("claim A");
    env.expire_blockhash();
    send(&mut env, claim_ix(b.pubkey(), usdc, win, true), &b).expect("claim B");
    // Both received OVER_TOKENS_PER; 2×50_000_000_000 = EMISSION was handed out.
    let leftover = env.token_balance(&token_vault_pda());
    assert_eq!(leftover, EXTRA, "the extra unit remained in token_vault");

    env.warp_to(deadline_of(&p)); // ClaimClosed
    let owner_ata = owner_token_ata(&owner.pubkey(), &win);

    env.expire_blockhash();
    let r = send(
        &mut env,
        sweep_token_vault_ix(owner.pubkey(), win, owner.pubkey()),
        &owner,
    );
    r.as_ref().expect("sweep_token_vault passes");

    assert_eq!(
        env.token_balance(&owner_ata),
        EXTRA,
        "the Owner received the emission remainder"
    );
    assert!(
        !env.account_exists(&token_vault_pda()),
        "token_vault is closed"
    );
    let ev: Swept = decode_event(&r).expect("the Swept event");
    assert_eq!(ev.vault, token_vault_pda(), "Swept.vault == token_vault");
    assert_eq!(ev.amount, EXTRA, "Swept.amount == the remainder");
}

// ===================================================================
// IDEMPOTENCE: a repeated refund sweep → AccountNotInitialized, while the deposit
// and token vaults remain sweepable (per-vault independence, §9.14).
// ===================================================================

#[test]
fn sweep_is_idempotent_per_vault() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let win = env.win_mint;
    let owner = env.actors.owner.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    // Oversubscribed: all three vaults exist in ClaimClosed (nobody claimed →
    // refund_vault is full, token_vault is full, and deposit_vault is empty but exists).
    activate(
        &mut env,
        &p,
        &[(&a, OVER_DEP), (&b, OVER_DEP)],
        EMISSION,
        true,
    );
    env.warp_to(deadline_of(&p)); // ClaimClosed

    // The first refund sweep: it passes and the vault is closed.
    env.expire_blockhash();
    send(
        &mut env,
        sweep_refund_vault_ix(owner.pubkey(), usdc, owner.pubkey()),
        &owner,
    )
    .expect("the first sweep_refund_vault");
    assert!(
        !env.account_exists(&refund_vault_pda()),
        "refund_vault is closed"
    );

    // A repeat of the refund sweep WITH A FRESH blockhash → AccountNotInitialized.
    env.expire_blockhash();
    let r = send(
        &mut env,
        sweep_refund_vault_ix(owner.pubkey(), usdc, owner.pubkey()),
        &owner,
    );
    assert_eq!(
        custom(&r),
        Some(anchor_err(AnchorError::AccountNotInitialized)),
        "a repeated sweep_refund_vault → AccountNotInitialized (3012): {r:?}"
    );

    // The failed repeat did NOT jam the other vaults: deposit and token are swept.
    env.expire_blockhash();
    send(
        &mut env,
        sweep_deposit_vault_ix(owner.pubkey(), usdc, owner.pubkey()),
        &owner,
    )
    .expect("sweep_deposit_vault after the failed refund repeat");
    assert!(
        !env.account_exists(&deposit_vault_pda()),
        "deposit_vault is closed"
    );

    env.expire_blockhash();
    send(
        &mut env,
        sweep_token_vault_ix(owner.pubkey(), win, owner.pubkey()),
        &owner,
    )
    .expect("sweep_token_vault after the failed refund repeat");
    assert!(
        !env.account_exists(&token_vault_pda()),
        "token_vault is closed"
    );
    // The Owner collected the whole refund_vault plus the entire undistributed emission.
    assert_eq!(
        env.token_balance(&owner_token_ata(&owner.pubkey(), &win)),
        EMISSION,
        "the whole token_vault (nobody claimed) is with the Owner"
    );
    assert_eq!(
        env.token_balance(&wallet_usdc_ata(&owner.pubkey(), &usdc)),
        OVER_REFUND_VAULT,
        "the whole refund_vault is with the Owner (the reserve included, nobody took a refund)"
    );
}
