//! End-to-end solvency (I9 + I4, §10.7) - the key M2 positive. Oversubscription
//! across ≥ 3 wallets: Admin activates, the Owner withdraws ALL proceeds
//! (proceeds_vault is drained), and then EVERY wallet claims successfully (tokens +
//! refund). It proves that the maximum withdrawal of proceeds does not starve the
//! refunds - the rounding reserve is held on the refund_vault side (§10.7). After the
//! last claim refund_vault holds exactly the reserve; a sweep drains it in
//! ClaimClosed.
//!
//! tests-bind-to-real-code: real instructions; the allocation and refund reference
//! values are independent literals (their derivation is in the comments); the
//! assertions run against observable balances and facts of closing.
//!
//! The invariants under test:
//! - I9 (§10.7): withdraw_proceeds withdraws exactly soft_cap − participants;
//!   refund_vault, as a separate vault, holds pool − proceeds → there is enough for
//!   everyone's refund.
//! - I4 (§3): every claimer receives their refund out of refund_vault; the last one
//!   is solvent.
//! - §10.7: after all the claims exactly the reserve = participants remains, and the
//!   sweep collects it.

use anchor_lang::error::{ErrorCode as AnchorError, ERROR_CODE_OFFSET};
use sale::errors::SaleError;
use sale_test_common::{
    activate_claim_ix, claim_ix, custom_error_code, expected::DEFAULT_PRICE_USDC,
    init_aggregate_ix, initialize_ix, proceeds_vault_pda, purchase_ix, refund_ix, refund_vault_pda,
    sweep_refund_vault_ix, token_vault_pda, wallet_token_ata, wallet_usdc_ata,
    withdraw_proceeds_ix, TestEnv,
};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

// ----- The reference economics: 3 wallets × $20, a pool of $60 > the soft cap of $30 -----

const PRICE: u64 = DEFAULT_PRICE_USDC;
/// An emission of 1000 $1WIN = 10^11 token units.
const EMISSION: u64 = 100_000_000_000;
/// Soft Cap = floor(10^11 × 30_000 / 10^8) = 30_000_000 ($30).
const SOFT_CAP: u64 = 30_000_000;
const DEP: u64 = 20_000_000; // $20 per wallet
const POOL: u64 = 60_000_000; // $60 (3 × $20)
const PARTICIPANTS: u64 = 3;

/// proceeds = soft_cap − participants = 30_000_000 − 3 = 29_999_997.
const PROCEEDS: u64 = 29_999_997;
/// refund_vault at activation = pool − proceeds = 60_000_000 − 29_999_997 = 30_000_003.
const REFUND_VAULT_START: u64 = POOL - PROCEEDS;

/// tokens per wallet = floor(20_000_000 × 10^11 / 60_000_000)
/// = floor(2×10^18 / 6×10^7) = floor(3.3333…×10^10) = 33_333_333_333.
const TOKENS_PER: u64 = 33_333_333_333;
/// spent = ceil(33_333_333_333 × 30_000 / 10^8) = ceil(9_999_999.9999) = 10_000_000;
/// refund = net − spent = 20_000_000 − 10_000_000 = 10_000_000 ($10) per wallet.
const REFUND_PER: u64 = 10_000_000;
/// The reserve after 3 refunds: 30_000_003 − 3×10_000_000 = 3 = participants (§10.7).
const RESERVE_DUST: u64 = 3;
/// The token_vault remainder: emission − Σ tokens = 10^11 − 3×33_333_333_333
/// = 100_000_000_000 − 99_999_999_999 = 1 (the round-down dust).
const TOKEN_DUST: u64 = 1;

// Cross-checking the independent literals at compile time (it catches a typo without
// reproducing the formula).
const _: () = assert!(PROCEEDS == SOFT_CAP - PARTICIPANTS);
const _: () = assert!(REFUND_VAULT_START == POOL - PROCEEDS);
const _: () = assert!(RESERVE_DUST == REFUND_VAULT_START - PARTICIPANTS * REFUND_PER);

fn custom(result: &litesvm::types::TransactionResult) -> Option<u32> {
    custom_error_code(result)
}

fn err(variant: SaleError) -> u32 {
    ERROR_CODE_OFFSET + variant as u32
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

#[test]
fn oversubscribed_max_proceeds_withdrawal_never_starves_refunds() {
    let mut env = TestEnv::new();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let owner = env.actors.owner.insecure_clone();
    let admin = env.actors.admin.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let c = env.actors.users[2].insecure_clone();

    let mut p = env.valid_init_params();
    p.price_usdc = PRICE;
    p.emission = EMISSION;

    // --- init + collecting the pool ($60 from three wallets) ---
    let owner_kp = owner.insecure_clone();
    send(
        &mut env,
        initialize_ix(owner_kp.pubkey(), usdc, win, &p),
        &owner_kp,
    )
    .expect("initialize");
    env.warp_to(p.sale_start_date);
    for w in [&a, &b, &c] {
        let ata = funded_ata(&mut env, &w.pubkey(), DEP);
        env.expire_blockhash();
        send(&mut env, init_aggregate_ix(w.pubkey()), w).expect("init_aggregate");
        env.expire_blockhash();
        send(&mut env, purchase_ix(w.pubkey(), ata, 0, DEP), w).expect("purchase");
    }
    assert_eq!(
        env.sale_state().deposits_usdc_contributed,
        POOL,
        "the pool = $60"
    );
    assert_eq!(
        env.sale_state().participants,
        PARTICIPANTS,
        "3 participants"
    );

    // --- activation (oversubscribed) ---
    env.warp_to(p.sale_end_date);
    env.mint_to(&win, &token_vault_pda(), EMISSION); // distributable = the whole emission
    env.expire_blockhash();
    send(
        &mut env,
        activate_claim_ix(admin.pubkey(), usdc, Some(refund_vault_pda())),
        &admin,
    )
    .expect("activate_claim");
    assert_eq!(
        env.token_balance(&proceeds_vault_pda()),
        PROCEEDS,
        "proceeds_vault = soft_cap − participants"
    );
    assert_eq!(
        env.token_balance(&refund_vault_pda()),
        REFUND_VAULT_START,
        "refund_vault = pool − proceeds"
    );

    // --- the Owner withdraws ALL proceeds BEFORE the claims (the maximum withdrawal, §10.7) ---
    env.warp_to(p.claim_start_date);
    env.expire_blockhash();
    send(
        &mut env,
        withdraw_proceeds_ix(owner.pubkey(), usdc, owner.pubkey()),
        &owner,
    )
    .expect("withdraw_proceeds (all the proceeds)");
    assert_eq!(
        env.token_balance(&wallet_usdc_ata(&owner.pubkey(), &usdc)),
        PROCEEDS,
        "the Owner withdrew all the proceeds"
    );
    assert!(
        !env.account_exists(&proceeds_vault_pda()),
        "proceeds_vault is closed"
    );

    // --- EVERY wallet claims: tokens plus a refund out of refund_vault ---
    for (i, w) in [&a, &b, &c].iter().enumerate() {
        let usdc_before = env.token_balance(&wallet_usdc_ata(&w.pubkey(), &usdc));
        env.expire_blockhash();
        send(&mut env, claim_ix(w.pubkey(), usdc, win, true), w)
            .unwrap_or_else(|e| panic!("the claim of wallet #{i} must pass: {e:?}"));
        assert_eq!(
            env.token_balance(&wallet_token_ata(&w.pubkey(), &win)),
            TOKENS_PER,
            "wallet #{i}: tokens by the oversubscription formula"
        );
        assert_eq!(
            env.token_balance(&wallet_usdc_ata(&w.pubkey(), &usdc)) - usdc_before,
            REFUND_PER,
            "wallet #{i}: a $10 refund out of refund_vault"
        );
        assert!(
            !env.aggregate_exists(&w.pubkey()),
            "wallet #{i}: the aggregate is closed"
        );
    }

    // I4/§10.7: the last claimer is solvent; refund_vault is left holding exactly the
    // reserve ≥ 0.
    assert_eq!(
        env.token_balance(&refund_vault_pda()),
        RESERVE_DUST,
        "after all the refunds the reserve = participants remains"
    );
    // I3: token_vault is drained down to the rounding dust.
    assert_eq!(
        env.token_balance(&token_vault_pda()),
        TOKEN_DUST,
        "token_vault: the remainder is the floor-rounding dust"
    );

    // --- ClaimClosed: the sweep collects the refund_vault reserve ---
    let deadline = p.claim_start_date + p.claim_duration;
    env.warp_to(deadline);
    let owner_before = env.token_balance(&wallet_usdc_ata(&owner.pubkey(), &usdc));
    env.expire_blockhash();
    send(
        &mut env,
        sweep_refund_vault_ix(owner.pubkey(), usdc, owner.pubkey()),
        &owner,
    )
    .expect("sweep_refund_vault (the reserve)");
    assert_eq!(
        env.token_balance(&wallet_usdc_ata(&owner.pubkey(), &usdc)) - owner_before,
        RESERVE_DUST,
        "the Owner swept exactly the reserve"
    );
    assert!(
        !env.account_exists(&refund_vault_pda()),
        "refund_vault is closed after the sweep"
    );
}

// ===================================================================
// The flip side of solvency: withdraw_proceeds does NOT touch refund_vault - after
// all the proceeds are withdrawn the refunds are still paid out (already covered by
// the main test; here is a pinpoint negative on trying to siphon the refunds through
// a withdrawal).
// ===================================================================

#[test]
fn withdraw_proceeds_cannot_be_repeated_to_drain_refund_reserve() {
    // The maximum withdrawal of proceeds is one-shot: on a repeat the vault is gone. The
    // reserve stays out of withdraw_proceeds' reach by construction (different PDA
    // vaults, I9).
    let mut env = TestEnv::new();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let owner = env.actors.owner.insecure_clone();
    let admin = env.actors.admin.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let c = env.actors.users[2].insecure_clone();

    let mut p = env.valid_init_params();
    p.price_usdc = PRICE;
    p.emission = EMISSION;
    send(
        &mut env,
        initialize_ix(owner.pubkey(), usdc, win, &p),
        &owner,
    )
    .expect("initialize");
    env.warp_to(p.sale_start_date);
    for w in [&a, &b, &c] {
        let ata = funded_ata(&mut env, &w.pubkey(), DEP);
        env.expire_blockhash();
        send(&mut env, init_aggregate_ix(w.pubkey()), w).expect("init_aggregate");
        env.expire_blockhash();
        send(&mut env, purchase_ix(w.pubkey(), ata, 0, DEP), w).expect("purchase");
    }
    env.warp_to(p.sale_end_date);
    env.mint_to(&win, &token_vault_pda(), EMISSION);
    env.expire_blockhash();
    send(
        &mut env,
        activate_claim_ix(admin.pubkey(), usdc, Some(refund_vault_pda())),
        &admin,
    )
    .expect("activate_claim");
    env.warp_to(p.claim_start_date);

    env.expire_blockhash();
    send(
        &mut env,
        withdraw_proceeds_ix(owner.pubkey(), usdc, owner.pubkey()),
        &owner,
    )
    .expect("the first withdraw_proceeds");

    // The repeat with a fresh blockhash: proceeds_vault is closed →
    // AccountNotInitialized.
    env.expire_blockhash();
    let r = send(
        &mut env,
        withdraw_proceeds_ix(owner.pubkey(), usdc, owner.pubkey()),
        &owner,
    );
    assert_eq!(
        custom(&r),
        Some(AnchorError::AccountNotInitialized as u32),
        "a repeated withdraw_proceeds → AccountNotInitialized (3012): {r:?}"
    );
    // refund_vault is full and untouched by the proceeds withdrawal.
    assert_eq!(
        env.token_balance(&refund_vault_pda()),
        REFUND_VAULT_START,
        "refund_vault is untouched by withdraw_proceeds (I9)"
    );
}

// ===================================================================
// L3 (a regression): a wallet refunded down to net==0 does NOT starve the refunds of
// the other claimers. `participants` is incremented on init_aggregate and NEVER
// decreases (Arch §5.1 - a deliberately conservative over-estimate of the reserve);
// a fully refunded wallet stays counted in `participants` and keeps its (empty)
// aggregate. The test proves this only INCREASES the refund_vault reserve rather
// than taking USDC away from B/C, and that refund_vault stays solvent.
//
// tests-bind-to-real-code: real refund/activate/claim calls; the reference values are
// independent literals (their derivation is in the comments), and the assertions run
// against observable balances and facts.
// ===================================================================

// ----- The L3 economics: A/B/C × $20; A is fully refunded → a fixed pool of $40 -----

/// The pool at activation after A's refund = $60 − $20 = $40 (that is
/// pool_fixed_amount).
const L3_POOL_FIXED: u64 = 40_000_000;
/// Oversubscribed ($40 × 10^8 = 4×10^15 > emission × price = 10^11 × 30_000 = 3×10^15).
/// proceeds = soft_cap − participants = 30_000_000 − 3 = 29_999_997.
const L3_PROCEEDS: u64 = 29_999_997;
/// refund_vault at activation = pool_fixed − proceeds = 40_000_000 − 29_999_997 = 10_000_003.
const L3_REFUND_VAULT_START: u64 = 10_000_003;

/// tokens for B/C = floor(net × emission / pool_fixed)
/// = floor(20_000_000 × 10^11 / 40_000_000) = floor(2×10^18 / 4×10^7) = 50_000_000_000.
const L3_TOKENS_PER: u64 = 50_000_000_000;
/// spent = ceil(50_000_000_000 × 30_000 / 10^8) = ceil(1.5×10^7) = 15_000_000;
/// refund = net − spent = 20_000_000 − 15_000_000 = 5_000_000 ($5) per wallet.
const L3_REFUND_PER: u64 = 5_000_000;
/// The reserve after the two refunds: 10_000_003 − 2×5_000_000 = 3 = participants
/// (§10.7).
const L3_RESERVE_DUST: u64 = 3;

// Cross-checking the independent literals at compile time (it catches a typo without
// reproducing the formula).
const _: () = assert!(L3_POOL_FIXED == POOL - DEP);
const _: () = assert!(L3_PROCEEDS == SOFT_CAP - PARTICIPANTS);
const _: () = assert!(L3_REFUND_VAULT_START == L3_POOL_FIXED - L3_PROCEEDS);
const _: () = assert!(L3_RESERVE_DUST == L3_REFUND_VAULT_START - 2 * L3_REFUND_PER);
const _: () = assert!(L3_RESERVE_DUST == PARTICIPANTS);

#[test]
fn fully_refunded_wallet_does_not_starve_other_claimers() {
    let mut env = TestEnv::new();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let owner = env.actors.owner.insecure_clone();
    let admin = env.actors.admin.insecure_clone();
    let operator = env.actors.operator.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let c = env.actors.users[2].insecure_clone();

    let mut p = env.valid_init_params();
    p.price_usdc = PRICE;
    p.emission = EMISSION;

    // --- init + collecting the pool ($60 from three wallets) ---
    send(
        &mut env,
        initialize_ix(owner.pubkey(), usdc, win, &p),
        &owner,
    )
    .expect("initialize");
    env.warp_to(p.sale_start_date);
    for w in [&a, &b, &c] {
        let ata = funded_ata(&mut env, &w.pubkey(), DEP);
        env.expire_blockhash();
        send(&mut env, init_aggregate_ix(w.pubkey()), w).expect("init_aggregate");
        env.expire_blockhash();
        send(&mut env, purchase_ix(w.pubkey(), ata, 0, DEP), w).expect("purchase");
    }
    assert_eq!(
        env.sale_state().deposits_usdc_contributed,
        POOL,
        "the pool = $60"
    );
    assert_eq!(
        env.sale_state().participants,
        PARTICIPANTS,
        "3 participants"
    );

    // --- the Operator fully refunds A's only deposit (net → 0) ---
    env.expire_blockhash();
    send(
        &mut env,
        refund_ix(operator.pubkey(), a.pubkey(), 0, usdc),
        &operator,
    )
    .expect("refund A");
    assert_eq!(
        env.sale_state().deposits_usdc_contributed,
        L3_POOL_FIXED,
        "the pool dropped to $40 after A's refund"
    );
    assert_eq!(
        env.sale_state().participants,
        PARTICIPANTS,
        "participants did NOT decrease (§5.1)"
    );
    assert!(
        env.aggregate_exists(&a.pubkey()),
        "A's aggregate remains (net == 0)"
    );
    assert_eq!(
        env.wallet_aggregate(&a.pubkey()).net_usdc_contributed,
        0,
        "A's net was zeroed"
    );

    // --- activation: the $40 pool is still oversubscribed vs the $30 soft cap ---
    env.warp_to(p.sale_end_date);
    env.mint_to(&win, &token_vault_pda(), EMISSION);
    env.expire_blockhash();
    send(
        &mut env,
        activate_claim_ix(admin.pubkey(), usdc, Some(refund_vault_pda())),
        &admin,
    )
    .expect("activate_claim");
    assert_eq!(
        env.sale_state().pool_fixed_amount,
        L3_POOL_FIXED,
        "a pool of $40 was fixed"
    );
    assert_eq!(
        env.token_balance(&proceeds_vault_pda()),
        L3_PROCEEDS,
        "proceeds = soft_cap − participants (the reserve counts the refunded A too)"
    );
    assert_eq!(
        env.token_balance(&refund_vault_pda()),
        L3_REFUND_VAULT_START,
        "refund_vault = pool_fixed − proceeds"
    );

    // --- B and C claim: each gets a full allocation plus exactly a $5 refund ---
    env.warp_to(p.claim_start_date);
    for (label, w) in [("B", &b), ("C", &c)] {
        let usdc_before = env.token_balance(&wallet_usdc_ata(&w.pubkey(), &usdc));
        env.expire_blockhash();
        send(&mut env, claim_ix(w.pubkey(), usdc, win, true), w)
            .unwrap_or_else(|e| panic!("the claim of {label} must pass: {e:?}"));
        assert_eq!(
            env.token_balance(&wallet_token_ata(&w.pubkey(), &win)),
            L3_TOKENS_PER,
            "{label}: tokens by the oversubscription formula (a $40 pool, not $60)"
        );
        assert_eq!(
            env.token_balance(&wallet_usdc_ata(&w.pubkey(), &usdc)) - usdc_before,
            L3_REFUND_PER,
            "{label}: a refund of exactly $5 (A took nothing away)"
        );
        assert!(
            !env.aggregate_exists(&w.pubkey()),
            "{label}: the aggregate is closed"
        );
    }

    // --- A claims → NothingToClaim (net == 0); the aggregate is NOT touched ---
    env.expire_blockhash();
    let r = send(&mut env, claim_ix(a.pubkey(), usdc, win, true), &a);
    assert_eq!(
        custom(&r),
        Some(err(SaleError::NothingToClaim)),
        "claim A (net == 0) → NothingToClaim: {r:?}"
    );
    assert!(
        env.aggregate_exists(&a.pubkey()),
        "A's aggregate was not closed by the failed claim"
    );
    assert_eq!(
        env.wallet_aggregate(&a.pubkey()).net_usdc_contributed,
        0,
        "A's net is still 0"
    );
    assert!(
        !env.account_exists(&wallet_token_ata(&a.pubkey(), &win)),
        "A got no token ATA (the claim rolled back through init_if_needed)"
    );

    // --- refund_vault is solvent: exactly the reserve = participants remains ---
    assert_eq!(
        env.token_balance(&refund_vault_pda()),
        L3_RESERVE_DUST,
        "after B/C refund_vault holds exactly the reserve = participants (solvent)"
    );
}
