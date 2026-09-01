//! The worst case of rounding dust (§10.7): directed SVM scenarios that maximize
//! per-wallet rounding under oversubscription. It is verified physically: after the
//! maximum withdrawal of proceeds EVERY claim is served out of refund_vault
//! (I4/I9), the leftover dust does not exceed the reserve, and the sweep collects it.
//!
//! The reference values are independent literals (their derivation is in the
//! comments) or the inequalities of §10.7; the allocation formulas are not reproduced
//! in the test.

use litesvm::types::TransactionResult;
use sale_test_common::{
    activate_claim_ix, claim_ix, init_aggregate_ix, initialize_ix, owner_token_ata,
    proceeds_vault_pda, purchase_ix, refund_vault_pda, sweep_refund_vault_ix, sweep_token_vault_ix,
    token_vault_pda, wallet_token_ata, wallet_usdc_ata, withdraw_proceeds_ix, InitParams, TestEnv,
    WIN_DECIMALS,
};
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

const TOKEN_UNITS: u64 = 10u64.pow(WIN_DECIMALS as u32);

#[allow(clippy::result_large_err)]
fn send(env: &mut TestEnv, ix: Instruction, signer: &Keypair) -> TransactionResult {
    env.expire_blockhash();
    env.send_ixs(&[ix], &signer.pubkey(), &[signer])
}

fn bal0(env: &TestEnv, token_account: &Pubkey) -> u64 {
    if env.account_exists(token_account) {
        env.token_balance(token_account)
    } else {
        0
    }
}

/// init → a pool built from `amounts` (one wallet per deposit) → mint the emission →
/// activate with refund_vault (the precondition being oversubscription). Returns the
/// wallets.
fn build_oversubscribed(env: &mut TestEnv, p: &InitParams, amounts: &[u64]) -> Vec<Keypair> {
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let owner = env.actors.owner.insecure_clone();
    let admin = env.actors.admin.insecure_clone();
    send(env, initialize_ix(owner.pubkey(), usdc, win, p), &owner).expect("initialize");

    env.warp_to(p.sale_start_date);
    let mut wallets = Vec::with_capacity(amounts.len());
    for &amount in amounts {
        let w = env.new_user();
        let ata = env.create_ata(&w.pubkey(), &usdc);
        env.mint_to(&usdc, &ata, amount);
        send(env, init_aggregate_ix(w.pubkey()), &w).expect("init_aggregate");
        send(env, purchase_ix(w.pubkey(), ata, 0, amount), &w).expect("purchase");
        wallets.push(w);
    }

    let pool: u64 = amounts.iter().sum();
    assert_eq!(env.sale_state().deposits_usdc_contributed, pool, "the pool");
    assert!(
        u128::from(pool) * u128::from(TOKEN_UNITS)
            > u128::from(p.emission) * u128::from(p.price_usdc),
        "precondition: oversubscribed"
    );

    env.warp_to(p.sale_end_date);
    env.mint_to(&win, &token_vault_pda(), p.emission);
    send(
        env,
        activate_claim_ix(admin.pubkey(), usdc, Some(refund_vault_pda())),
        &admin,
    )
    .expect("activate_claim");
    wallets
}

/// The owner withdraws all proceeds, then every wallet claims; returns (Σ tokens
/// released, Σ refunds) as observed from the balances.
fn withdraw_all_then_claim_all(
    env: &mut TestEnv,
    p: &InitParams,
    wallets: &[Keypair],
) -> (u128, u128) {
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let owner = env.actors.owner.insecure_clone();

    env.warp_to(p.claim_start_date);
    send(
        env,
        withdraw_proceeds_ix(owner.pubkey(), usdc, owner.pubkey()),
        &owner,
    )
    .expect("withdraw_proceeds (the maximum withdrawal)");
    assert!(
        !env.account_exists(&proceeds_vault_pda()),
        "proceeds_vault is closed"
    );

    let mut tokens_out: u128 = 0;
    let mut refunds_out: u128 = 0;
    for (i, w) in wallets.iter().enumerate() {
        let pk = w.pubkey();
        let usdc_before = bal0(env, &wallet_usdc_ata(&pk, &usdc));
        send(env, claim_ix(pk, usdc, win, true), w)
            .unwrap_or_else(|e| panic!("I4/§10.7: claim #{i} failed: {e:?}"));
        assert!(!env.aggregate_exists(&pk), "aggregate #{i} is closed");
        tokens_out += u128::from(bal0(env, &wallet_token_ata(&pk, &win)));
        refunds_out += u128::from(bal0(env, &wallet_usdc_ata(&pk, &usdc)) - usdc_before);
    }
    (tokens_out, refunds_out)
}

// ===================================================================
// E2 on-chain: an indivisible pool of 7 units across three wallets, with the E2
// literal reference values.
// soft_cap = floor(10 × 30_000 / 10⁸) = 0 < participants → proceeds = 0, and
// refund_vault at activation holds the whole pool (7).
// ===================================================================

/// refund_vault after the claims: 7 − (2 + 2 + 0) = 3 (= Σ of the spent amounts
/// withheld).
const P2_REFUND_LEFTOVER: u64 = 3;
const _: () = assert!(
    P2_REFUND_LEFTOVER
        == expected::P2_POOL
            - (expected::P2_REFUNDS[0] + expected::P2_REFUNDS[1] + expected::P2_REFUNDS[2])
);

#[test]
fn p2_rounding_dust_exact_on_chain() {
    let mut env = TestEnv::new();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let owner = env.actors.owner.insecure_clone();

    let mut p = env.valid_init_params();
    p.price_usdc = expected::DEFAULT_PRICE_USDC;
    p.emission = expected::P2_EMISSION;
    p.min_per_tx = 1;
    p.max_per_tx = 3;
    let wallets = build_oversubscribed(&mut env, &p, &expected::P2_NETS);

    // proceeds = saturating_sub(soft_cap 0, participants 3) = 0; refund = the pool.
    assert_eq!(env.token_balance(&proceeds_vault_pda()), 0, "proceeds = 0");
    assert_eq!(
        env.token_balance(&refund_vault_pda()),
        expected::P2_POOL,
        "refund_vault holds the whole pool"
    );

    env.warp_to(p.claim_start_date);
    send(
        &mut env,
        withdraw_proceeds_ix(owner.pubkey(), usdc, owner.pubkey()),
        &owner,
    )
    .expect("withdraw_proceeds (0)");
    assert_eq!(bal0(&env, &wallet_usdc_ata(&owner.pubkey(), &usdc)), 0);

    for (i, w) in wallets.iter().enumerate() {
        let pk = w.pubkey();
        let usdc_before = bal0(&env, &wallet_usdc_ata(&pk, &usdc));
        send(&mut env, claim_ix(pk, usdc, win, true), w)
            .unwrap_or_else(|e| panic!("claim #{i} failed: {e:?}"));
        assert_eq!(
            env.token_balance(&wallet_token_ata(&pk, &win)),
            expected::P2_TOKENS[i],
            "the tokens of wallet #{i} (E2)"
        );
        assert_eq!(
            bal0(&env, &wallet_usdc_ata(&pk, &usdc)) - usdc_before,
            expected::P2_REFUNDS[i],
            "the refund of wallet #{i} (E2)"
        );
    }
    assert_eq!(
        env.token_balance(&token_vault_pda()),
        expected::P2_REMAINDER,
        "the emission dust = E2_REMAINDER"
    );
    assert_eq!(
        env.token_balance(&refund_vault_pda()),
        P2_REFUND_LEFTOVER,
        "the refund_vault remainder = what was withheld"
    );

    // ClaimClosed: the sweeps hand both kinds of dust to the Owner.
    env.warp_to(p.claim_start_date + p.claim_duration);
    send(
        &mut env,
        sweep_refund_vault_ix(owner.pubkey(), usdc, owner.pubkey()),
        &owner,
    )
    .expect("sweep_refund_vault");
    send(
        &mut env,
        sweep_token_vault_ix(owner.pubkey(), win, owner.pubkey()),
        &owner,
    )
    .expect("sweep_token_vault");
    assert_eq!(
        bal0(&env, &wallet_usdc_ata(&owner.pubkey(), &usdc)),
        P2_REFUND_LEFTOVER,
        "the Owner received what was withheld"
    );
    assert_eq!(
        bal0(&env, &owner_token_ata(&owner.pubkey(), &win)),
        expected::P2_REMAINDER,
        "the Owner received the emission dust"
    );
}

// ===================================================================
// The maximum per-wallet dust: a price of token_units − 1 (the top of the valid
// range) across 60 wallets with mutually incommensurable amounts.
// ===================================================================

const MAXP_N: usize = 60;
/// The top of the valid price range (0 < price < token_units, §10.7).
const MAXP_PRICE: u64 = TOKEN_UNITS - 1; // 99_999_999
/// 100 whole $1WIN.
const MAXP_EMISSION: u64 = 10_000_000_000;
/// soft_cap = floor(10¹⁰ × 99_999_999 / 10⁸) = 99_999_999 × 100 = 9_999_999_900.
/// proceeds = soft_cap − participants = 9_999_999_900 − 60 = 9_999_999_840.
const MAXP_PROCEEDS: u64 = 9_999_999_840;
/// Σ amounts = 60 × 400_000_001 + 6_700_417 × (0+1+…+59 = 1770)
/// = 24_000_000_060 + 11_859_738_090 = 35_859_738_150 (~3.6× oversubscription).
const MAXP_POOL: u64 = 35_859_738_150;
/// refund_vault at activation = pool − proceeds = 25_859_738_310.
const MAXP_REFUND_START: u64 = 25_859_738_310;
const _: () = assert!(MAXP_REFUND_START == MAXP_POOL - MAXP_PROCEEDS);

fn maxp_amounts() -> Vec<u64> {
    // An odd step, incommensurable with the price: every wallet gets its own fractional
    // part.
    (0..MAXP_N as u64)
        .map(|i| 400_000_001 + 6_700_417 * i)
        .collect()
}

#[test]
fn max_price_sixty_wallets_reserve_covers_dust() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let owner = env.actors.owner.insecure_clone();

    let amounts = maxp_amounts();
    assert_eq!(
        amounts.iter().sum::<u64>(),
        MAXP_POOL,
        "checking the pool literal"
    );

    let mut p = env.valid_init_params();
    p.price_usdc = MAXP_PRICE;
    p.emission = MAXP_EMISSION;
    p.min_per_tx = *amounts.first().unwrap();
    p.max_per_tx = *amounts.last().unwrap();
    let wallets = build_oversubscribed(&mut env, &p, &amounts);

    assert_eq!(
        env.token_balance(&proceeds_vault_pda()),
        MAXP_PROCEEDS,
        "proceeds = soft_cap − participants"
    );
    assert_eq!(
        env.token_balance(&refund_vault_pda()),
        MAXP_REFUND_START,
        "refund_vault = pool − proceeds"
    );

    let (tokens_out, refunds_out) = withdraw_all_then_claim_all(&mut env, &p, &wallets);
    assert_eq!(
        bal0(&env, &wallet_usdc_ata(&owner.pubkey(), &usdc)),
        MAXP_PROCEEDS,
        "the Owner withdrew all proceeds before the claims (I9)"
    );

    // §10.7: the token floor dust is < N (at most 1 unit per wallet).
    let token_dust = u128::from(MAXP_EMISSION) - tokens_out;
    assert_eq!(
        u128::from(env.token_balance(&token_vault_pda())),
        token_dust,
        "I3: the remainder == emission − released"
    );
    assert!(token_dust < MAXP_N as u128, "the token floor dust is < N");

    // §10.7: the refund_vault remainder (the reserve plus the ceil dust) stays within
    // 2 × participants.
    let left = u128::from(MAXP_REFUND_START) - refunds_out;
    assert_eq!(
        u128::from(env.token_balance(&refund_vault_pda())),
        left,
        "refund_vault == the start minus the refunds"
    );
    assert!(
        left <= 2 * MAXP_N as u128,
        "the remainder {left} exceeded 2×participants"
    );
}

// ===================================================================
// Extreme oversubscription with minimal contributions: everyone gets tokens == 0 →
// a full refund each (§10.6), and refund_vault drains to exactly 0.
// ===================================================================

const ONEU_N: usize = 40;

#[test]
fn one_unit_deposits_full_refund_zero_token_claims() {
    let mut env = TestEnv::new();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let owner = env.actors.owner.insecure_clone();

    // A pool of 40 units against soft_cap = floor(1 × 1 / 10⁸) = 0: oversubscribed, and
    // every share is < 1 token unit → tokens = 0, spent = 0, refund = net = 1.
    let amounts = vec![1u64; ONEU_N];
    let mut p = env.valid_init_params();
    p.price_usdc = 1;
    p.emission = 1;
    p.min_per_tx = 1;
    p.max_per_tx = 1;
    let wallets = build_oversubscribed(&mut env, &p, &amounts);

    // proceeds = saturating_sub(0, 40) = 0; refund_vault holds the whole pool of 40.
    assert_eq!(env.token_balance(&proceeds_vault_pda()), 0, "proceeds = 0");
    assert_eq!(
        env.token_balance(&refund_vault_pda()),
        ONEU_N as u64,
        "refund_vault holds the pool"
    );

    let (tokens_out, refunds_out) = withdraw_all_then_claim_all(&mut env, &p, &wallets);
    assert_eq!(tokens_out, 0, "nobody had enough for 1 token unit");
    assert_eq!(
        refunds_out, ONEU_N as u128,
        "everyone got their 1 unit back"
    );
    for (i, w) in wallets.iter().enumerate() {
        assert_eq!(
            bal0(&env, &wallet_usdc_ata(&w.pubkey(), &usdc)),
            1,
            "wallet #{i} received a full refund"
        );
    }
    assert_eq!(
        env.token_balance(&refund_vault_pda()),
        0,
        "refund_vault drained to exactly 0 - solvent with no slack"
    );
    assert_eq!(
        env.token_balance(&token_vault_pda()),
        1,
        "the whole emission was left as dust"
    );

    env.warp_to(p.claim_start_date + p.claim_duration);
    send(
        &mut env,
        sweep_token_vault_ix(owner.pubkey(), win, owner.pubkey()),
        &owner,
    )
    .expect("sweep_token_vault");
    assert_eq!(
        bal0(&env, &owner_token_ata(&owner.pubkey(), &win)),
        1,
        "the emission dust was swept to the Owner"
    );
}
