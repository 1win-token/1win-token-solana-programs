//! A property test of the full lifecycle (M2 item 6): random scenarios of deposits →
//! AML refunds → activation → the maximum withdrawal of proceeds → claims in random
//! order → close_batch + sweep. Every step is a real instruction; the assertions run
//! against observable balances and state (tests-bind-to-real-code): the conservation
//! of USDC and tokens, the solvency of every claim after the max withdrawal
//! (I3/I4/I9, §10.7), the activation split (I14), the physical pool before fixation
//! (I2), the immutability of pool_fixed_amount/participants after close (I8), and
//! rent going back to its owners.
//!
//! Determinism: SplitMix64 seeded from BASE_SEED; the seed of a failing scenario is
//! printed in the panic. To reproduce: LIFECYCLE_SEED=<seed> cargo test <test_name> -
//! the batch then runs exactly one scenario with that seed.

use std::panic::{catch_unwind, AssertUnwindSafe};

use anchor_lang::error::ERROR_CODE_OFFSET;
use litesvm::types::TransactionResult;
use sale::errors::SaleError;
use sale_test_common::{
    activate_claim_ix, aggregate_pda, claim_ix, close_aggregates_ix, close_deposits_ix,
    custom_error_code, deposit_pda, deposit_vault_pda, init_aggregate_ix, initialize_ix,
    owner_token_ata, proceeds_vault_pda, purchase_ix, refund_account_pda, refund_ix,
    refund_vault_pda, sweep_deposit_vault_ix, sweep_refund_vault_ix, sweep_token_vault_ix,
    token_vault_pda, wallet_token_ata, wallet_usdc_ata, withdraw_proceeds_ix, TestEnv,
    WIN_DECIMALS,
};
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

/// The determinism base; overridden by the LIFECYCLE_SEED env var (decimal or
/// 0x-hex).
const BASE_SEED: u64 = 0x1B16_5EED_2026_0704;
/// Token units in a whole $1WIN - the environment's scale (10^WIN_DECIMALS), not the
/// program's formula.
const TOKEN_UNITS: u64 = 10u64.pow(WIN_DECIMALS as u32);
/// The ceiling of a single deposit ($1M) and of the target pool ($300M) - they keep
/// the sums well inside u64.
const MAX_DEPOSIT: u64 = 1_000_000_000_000;
const MAX_POOL_TARGET: u128 = 300_000_000_000_000;
/// Pairs per close tx: below the measured legacy-tx ceiling (see close_batch.rs).
const CLOSE_CHUNK: usize = 14;

// ---------- A deterministic PRNG (SplitMix64) ----------

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    /// Uniformly from [lo, hi], inclusive.
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.below(hi - lo + 1)
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }

    fn shuffle<T>(&mut self, xs: &mut [T]) {
        for i in (1..xs.len()).rev() {
            let j = self.below((i + 1) as u64) as usize;
            xs.swap(i, j);
        }
    }
}

// ---------- Scenario generation ----------

#[derive(Debug, Clone, Copy)]
enum Profile {
    /// 1..=12 wallets - small pools and degenerate extremes.
    Small,
    /// 10..=40 wallets - the main mode.
    Medium,
    /// Extreme parameters: a price of 1 / token_units−1, a tiny emission, aggressive AML
    /// refunds.
    Extreme,
    /// The maximum scale: exactly 200 wallets.
    Max200,
}

/// The full plan of a scenario - all the randomness is fixed before execution.
struct Plan {
    price: u64,
    emission: u64,
    /// The deposit amounts per wallet (1..=3 per wallet).
    amounts: Vec<Vec<u64>>,
    /// Which deposits to refund inside the AML window.
    refund: Vec<Vec<bool>>,
    /// Who does not claim (the tail feeding close_aggregates + sweep).
    skip_claim: Vec<bool>,
    claim_order: Vec<usize>,
}

impl Plan {
    fn n_wallets(&self) -> usize {
        self.amounts.len()
    }

    fn total_deposited(&self) -> u64 {
        self.amounts.iter().flatten().sum()
    }

    fn net_of(&self, wi: usize) -> u64 {
        self.amounts[wi]
            .iter()
            .zip(&self.refund[wi])
            .filter(|(_, &refunded)| !refunded)
            .map(|(&a, _)| a)
            .sum()
    }

    fn pool_after_refunds(&self) -> u64 {
        (0..self.n_wallets()).map(|wi| self.net_of(wi)).sum()
    }

    fn min_amount(&self) -> u64 {
        self.amounts.iter().flatten().copied().min().unwrap()
    }

    fn max_amount(&self) -> u64 {
        self.amounts.iter().flatten().copied().max().unwrap()
    }
}

fn gen_price(rng: &mut Rng, profile: Profile) -> u64 {
    if matches!(profile, Profile::Extreme) && rng.chance(40) {
        return [1, 2, TOKEN_UNITS - 1][rng.below(3) as usize];
    }
    match rng.below(5) {
        0 => 1,
        1 => TOKEN_UNITS - 1,
        2 => 30_000,
        3 => rng.range(2, 999),
        _ => rng.range(1_000, 10_000_000),
    }
}

fn gen_emission(rng: &mut Rng, profile: Profile) -> u64 {
    if matches!(profile, Profile::Extreme) && rng.chance(40) {
        return rng.range(1, 100);
    }
    match rng.below(4) {
        0 => rng.range(1, 1_000),
        1 => rng.range(1_000, 1_000_000_000),
        2 => rng.range(1_000_000_000, 10_000_000_000_000),
        _ => rng.range(10_000_000_000_000, 20_000_000_000_000_000),
    }
}

fn gen_plan(rng: &mut Rng, profile: Profile) -> Plan {
    let price = gen_price(rng, profile);
    let emission = gen_emission(rng, profile);
    let n = match profile {
        Profile::Small => rng.range(1, 12) as usize,
        Profile::Medium => rng.range(10, 40) as usize,
        Profile::Extreme => rng.range(3, 24) as usize,
        Profile::Max200 => 200,
    };
    let deposit_counts: Vec<usize> = (0..n).map(|_| rng.range(1, 3) as usize).collect();
    let total_deps: u64 = deposit_counts.iter().map(|&c| c as u64).sum();

    // The target pool relative to the soft cap - only so the scenarios land in both
    // branches and on the boundary; the activation still branches on the actual pool.
    let soft_cap = u128::from(emission) * u128::from(price) / u128::from(TOKEN_UNITS);
    let target = if soft_cap == 0 {
        u128::from(rng.range(1_000, 1_000_000_000))
    } else {
        match rng.below(6) {
            0 => soft_cap / 8,
            1 => soft_cap / 2,
            2 => (soft_cap + u128::from(rng.range(0, 2 * total_deps)))
                .saturating_sub(u128::from(total_deps)),
            3 => soft_cap * 2,
            4 => soft_cap * 16,
            _ => soft_cap * 512,
        }
    };
    let target = target.clamp(u128::from(total_deps), MAX_POOL_TARGET);
    let base = u64::try_from(target / u128::from(total_deps))
        .expect("the target fits into u64")
        .max(1);
    let amounts: Vec<Vec<u64>> = deposit_counts
        .iter()
        .map(|&c| {
            (0..c)
                .map(|_| (base / 2 + rng.below(base + 1)).clamp(1, MAX_DEPOSIT))
                .collect()
        })
        .collect();

    let (refund_all_pct, refund_pct) = match profile {
        Profile::Extreme => (20, 40),
        _ => (12, 25),
    };
    let refund: Vec<Vec<bool>> = amounts
        .iter()
        .map(|a| {
            let all = rng.chance(refund_all_pct);
            a.iter().map(|_| all || rng.chance(refund_pct)).collect()
        })
        .collect();

    let skip_pct = if rng.chance(25) { 17 } else { 0 };
    let skip_claim: Vec<bool> = (0..n).map(|_| rng.chance(skip_pct)).collect();
    let mut claim_order: Vec<usize> = (0..n).collect();
    rng.shuffle(&mut claim_order);

    Plan {
        price,
        emission,
        amounts,
        refund,
        skip_claim,
        claim_order,
    }
}

// ---------- Execution helpers ----------

#[allow(clippy::result_large_err)]
fn send(env: &mut TestEnv, ix: Instruction, signer: &Keypair) -> TransactionResult {
    env.expire_blockhash();
    env.send_ixs(&[ix], &signer.pubkey(), &[signer])
}

/// A token account's balance; 0 if the account does not exist (yet or any more).
fn bal0(env: &TestEnv, token_account: &Pubkey) -> u64 {
    if env.account_exists(token_account) {
        env.token_balance(token_account)
    } else {
        0
    }
}

// ---------- The scenario ----------

fn run_scenario(seed: u64, profile: Profile) {
    let mut rng = Rng::new(seed);
    let plan = gen_plan(&mut rng, profile);
    let mut env = TestEnv::new();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let owner = env.actors.owner.insecure_clone();
    let admin = env.actors.admin.insecure_clone();
    let operator = env.actors.operator.insecure_clone();

    let mut p = env.valid_init_params();
    p.price_usdc = plan.price;
    p.emission = plan.emission;
    p.min_per_tx = plan.min_amount();
    p.max_per_tx = plan.max_amount();
    send(
        &mut env,
        initialize_ix(owner.pubkey(), usdc, win, &p),
        &owner,
    )
    .expect("initialize");

    // --- collecting the pool ---
    env.warp_to(p.sale_start_date);
    let wallets: Vec<Keypair> = (0..plan.n_wallets()).map(|_| env.new_user()).collect();
    for (wi, amounts) in plan.amounts.iter().enumerate() {
        let pk = wallets[wi].pubkey();
        let total: u64 = amounts.iter().sum();
        let ata = env.create_ata(&pk, &usdc);
        env.mint_to(&usdc, &ata, total);
        send(&mut env, init_aggregate_ix(pk), &wallets[wi]).expect("init_aggregate");
        for (di, &amount) in amounts.iter().enumerate() {
            send(
                &mut env,
                purchase_ix(pk, ata, di as u64, amount),
                &wallets[wi],
            )
            .expect("purchase");
        }
    }
    let deposited_total = plan.total_deposited();
    assert_eq!(
        env.sale_state().deposits_usdc_contributed,
        deposited_total,
        "the pool == Σ of the deposits"
    );
    assert_eq!(
        env.sale_state().participants,
        plan.n_wallets() as u64,
        "participants == N"
    );

    // --- AML refunds inside the SaleEnded window (after sale_end_date, before fixation) ---
    env.warp_to(p.sale_end_date);
    for (wi, flags) in plan.refund.iter().enumerate() {
        for (di, &do_refund) in flags.iter().enumerate() {
            if do_refund {
                send(
                    &mut env,
                    refund_ix(operator.pubkey(), wallets[wi].pubkey(), di as u64, usdc),
                    &operator,
                )
                .expect("refund");
            }
        }
    }
    let pool = plan.pool_after_refunds();
    assert_eq!(
        env.sale_state().deposits_usdc_contributed,
        pool,
        "the pool after the AML refunds"
    );
    // I2: the physical vault equals the accounted pool before fixation.
    assert_eq!(
        env.token_balance(&deposit_vault_pda()),
        pool,
        "deposit_vault == the pool (I2)"
    );
    for (wi, w) in wallets.iter().enumerate() {
        assert_eq!(
            env.wallet_aggregate(&w.pubkey()).net_usdc_contributed,
            plan.net_of(wi),
            "the net of wallet #{wi}"
        );
    }

    // --- activation: the branch is chosen by cross-multiplication (a branching
    // predicate, not a formula) ---
    let oversub = u128::from(pool) * u128::from(TOKEN_UNITS)
        > u128::from(plan.emission) * u128::from(plan.price);
    // The mint amount is the operational rule of TRD §5.6; when undersubscribed it is
    // ≤ emission by the branch's construction.
    let minted_tokens = if oversub {
        plan.emission
    } else {
        u64::try_from(u128::from(pool) * u128::from(TOKEN_UNITS) / u128::from(plan.price))
            .expect("the undersubscribed mint is ≤ the emission")
    };
    if minted_tokens > 0 {
        env.mint_to(&win, &token_vault_pda(), minted_tokens);
    }
    send(
        &mut env,
        activate_claim_ix(admin.pubkey(), usdc, oversub.then(refund_vault_pda)),
        &admin,
    )
    .expect("activate_claim");

    let st = env.sale_state();
    assert_eq!(st.pool_fixed_amount, pool, "the actual pool was fixed");
    let participants = st.participants;
    let proceeds_at_activation = env.token_balance(&proceeds_vault_pda());
    let refund_at_activation = if oversub {
        env.token_balance(&refund_vault_pda())
    } else {
        assert!(
            !env.account_exists(&refund_vault_pda()),
            "undersubscribed: refund_vault was not created"
        );
        0
    };
    // I14: the physical split covers the whole pool, in both branches.
    assert_eq!(
        proceeds_at_activation + refund_at_activation,
        pool,
        "I14: proceeds + refund == pool_fixed"
    );
    assert_eq!(
        env.token_balance(&deposit_vault_pda()),
        0,
        "deposit_vault was drained by the activation"
    );

    // --- the maximum withdrawal of proceeds BEFORE the claims (I9) ---
    env.warp_to(p.claim_start_date);
    let owner_usdc = wallet_usdc_ata(&owner.pubkey(), &usdc);
    send(
        &mut env,
        withdraw_proceeds_ix(owner.pubkey(), usdc, owner.pubkey()),
        &owner,
    )
    .expect("withdraw_proceeds");
    assert_eq!(
        bal0(&env, &owner_usdc),
        proceeds_at_activation,
        "exactly the proceeds were withdrawn (I9)"
    );
    assert!(
        !env.account_exists(&proceeds_vault_pda()),
        "proceeds_vault is closed"
    );

    // --- the claims, in random order ---
    let nothing = ERROR_CODE_OFFSET + SaleError::NothingToClaim as u32;
    let mut tokens_out: u128 = 0;
    let mut refunds_out: u128 = 0;
    for &wi in &plan.claim_order {
        if plan.skip_claim[wi] {
            continue;
        }
        let pk = wallets[wi].pubkey();
        let net = plan.net_of(wi);
        if net == 0 {
            let r = send(&mut env, claim_ix(pk, usdc, win, oversub), &wallets[wi]);
            assert_eq!(
                custom_error_code(&r),
                Some(nothing),
                "net == 0 → NothingToClaim: {r:?}"
            );
            assert!(
                env.aggregate_exists(&pk),
                "the aggregate survived NothingToClaim"
            );
            continue;
        }
        let usdc_before = bal0(&env, &wallet_usdc_ata(&pk, &usdc));
        // I3/I4/§10.7: after the maximum withdrawal of proceeds a claim must pass.
        send(&mut env, claim_ix(pk, usdc, win, oversub), &wallets[wi])
            .unwrap_or_else(|e| panic!("solvency: the claim of #{wi} (net = {net}) failed: {e:?}"));
        assert!(
            !env.aggregate_exists(&pk),
            "the claim closed aggregate #{wi}"
        );
        let tokens_got = bal0(&env, &wallet_token_ata(&pk, &win));
        let refund_got = bal0(&env, &wallet_usdc_ata(&pk, &usdc)) - usdc_before;
        assert!(refund_got <= net, "the refund ≤ net (#{wi})");
        if !oversub {
            assert_eq!(refund_got, 0, "undersubscribed: a refund of 0 (#{wi})");
        }
        tokens_out += u128::from(tokens_got);
        refunds_out += u128::from(refund_got);
    }

    // --- the vaults after the claims ---
    assert_eq!(
        u128::from(env.token_balance(&token_vault_pda())),
        u128::from(minted_tokens) - tokens_out,
        "I3: token_vault == minted − released"
    );
    let any_skipped = plan.skip_claim.iter().any(|&s| s);
    if oversub {
        let left = u128::from(env.token_balance(&refund_vault_pda()));
        assert_eq!(
            left,
            u128::from(refund_at_activation) - refunds_out,
            "refund_vault == the start minus the refunds"
        );
        if !any_skipped {
            // §10.7: after all the claims the remainder - the reserve plus the dust - is below
            // 2 × participants units.
            assert!(
                left <= 2 * u128::from(participants),
                "the dust {left} exceeded 2×participants ({participants})"
            );
        }
    }

    // --- the deadline: both batch closes become available only afterwards ---
    env.warp_to(p.claim_start_date + p.claim_duration);

    // --- close_deposits: the rent of the open deposits goes back to their wallets ---
    let mut pairs = Vec::new();
    let mut deposit_rent = vec![0u64; wallets.len()];
    for (wi, flags) in plan.refund.iter().enumerate() {
        for (di, &refunded) in flags.iter().enumerate() {
            if !refunded {
                let dep = deposit_pda(&wallets[wi].pubkey(), di as u64);
                deposit_rent[wi] += env.lamports_of(&dep);
                pairs.push((dep, wallets[wi].pubkey()));
            }
        }
    }
    let before: Vec<u64> = wallets
        .iter()
        .map(|w| env.lamports_of(&w.pubkey()))
        .collect();
    for chunk in pairs.chunks(CLOSE_CHUNK) {
        send(
            &mut env,
            close_deposits_ix(operator.pubkey(), chunk),
            &operator,
        )
        .expect("close_deposits");
    }
    for (wi, w) in wallets.iter().enumerate() {
        assert_eq!(
            env.lamports_of(&w.pubkey()) - before[wi],
            deposit_rent[wi],
            "the deposits' rent → wallet #{wi}"
        );
    }

    // --- close_aggregates for the tails ---
    let mut agg_pairs = Vec::new();
    let mut agg_rent = vec![0u64; wallets.len()];
    for (wi, w) in wallets.iter().enumerate() {
        let pk = w.pubkey();
        if env.aggregate_exists(&pk) {
            agg_rent[wi] = env.lamports_of(&aggregate_pda(&pk));
            agg_pairs.push((aggregate_pda(&pk), pk));
        }
    }
    let before: Vec<u64> = wallets
        .iter()
        .map(|w| env.lamports_of(&w.pubkey()))
        .collect();
    for chunk in agg_pairs.chunks(CLOSE_CHUNK) {
        send(
            &mut env,
            close_aggregates_ix(operator.pubkey(), chunk),
            &operator,
        )
        .expect("close_aggregates");
    }
    for (wi, w) in wallets.iter().enumerate() {
        assert_eq!(
            env.lamports_of(&w.pubkey()) - before[wi],
            agg_rent[wi],
            "the aggregate's rent → wallet #{wi}"
        );
        assert!(
            !env.aggregate_exists(&w.pubkey()),
            "aggregate #{wi} is closed"
        );
    }

    // I8: the closes did not touch the fixed fields.
    let st = env.sale_state();
    assert_eq!(
        st.pool_fixed_amount, pool,
        "I8: pool_fixed_amount is unchanged"
    );
    assert_eq!(
        st.participants, participants,
        "I8: participants is unchanged"
    );

    // --- the Owner's sweeps ---
    send(
        &mut env,
        sweep_deposit_vault_ix(owner.pubkey(), usdc, owner.pubkey()),
        &owner,
    )
    .expect("sweep_deposit_vault");
    if oversub {
        send(
            &mut env,
            sweep_refund_vault_ix(owner.pubkey(), usdc, owner.pubkey()),
            &owner,
        )
        .expect("sweep_refund_vault");
    }
    send(
        &mut env,
        sweep_token_vault_ix(owner.pubkey(), win, owner.pubkey()),
        &owner,
    )
    .expect("sweep_token_vault");
    assert!(
        !env.account_exists(&deposit_vault_pda()),
        "deposit_vault is closed"
    );
    assert!(
        !env.account_exists(&token_vault_pda()),
        "token_vault is closed"
    );
    assert!(
        !env.account_exists(&refund_vault_pda()),
        "refund_vault is closed / never existed"
    );

    // --- global conservation against the real balances ---
    let owner_usdc_total = u128::from(bal0(&env, &owner_usdc));
    let owner_tokens = u128::from(bal0(&env, &owner_token_ata(&owner.pubkey(), &win)));
    let mut users_usdc: u128 = 0;
    let mut users_tokens: u128 = 0;
    for (wi, w) in wallets.iter().enumerate() {
        users_usdc += u128::from(bal0(&env, &wallet_usdc_ata(&w.pubkey(), &usdc)));
        // The AML refunds sit in the per-deposit refund accounts, not in the ATA.
        for (di, &refunded) in plan.refund[wi].iter().enumerate() {
            if refunded {
                users_usdc += u128::from(bal0(&env, &refund_account_pda(&w.pubkey(), di as u64)));
            }
        }
        users_tokens += u128::from(bal0(&env, &wallet_token_ata(&w.pubkey(), &win)));
    }
    assert_eq!(
        users_usdc + owner_usdc_total,
        u128::from(deposited_total),
        "USDC conservation: refunds + proceeds + sweeps == Σ of the deposits"
    );
    assert_eq!(
        users_tokens + owner_tokens,
        u128::from(minted_tokens),
        "token conservation: released + swept == minted"
    );
}

// ---------- The batch runner with seed context ----------

fn seed_override() -> Option<u64> {
    let v = std::env::var("LIFECYCLE_SEED").ok()?;
    let v = v.trim();
    Some(match v.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16).expect("LIFECYCLE_SEED: hex"),
        None => v.parse().expect("LIFECYCLE_SEED: a number"),
    })
}

fn derive_seed(salt: u64, i: u64) -> u64 {
    Rng::new(BASE_SEED ^ (salt << 32) ^ i).next()
}

fn run_with_context(seed: u64, profile: Profile, idx: u64) {
    let result = catch_unwind(AssertUnwindSafe(|| run_scenario(seed, profile)));
    if let Err(payload) = result {
        let msg = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .unwrap_or("<a non-string panic>");
        panic!("scenario {profile:?} #{idx} failed; to reproduce: LIFECYCLE_SEED={seed:#x}: {msg}");
    }
}

fn run_batch(profile: Profile, count: u64, salt: u64) {
    if let Some(seed) = seed_override() {
        run_with_context(seed, profile, 0);
        return;
    }
    for i in 0..count {
        run_with_context(derive_seed(salt, i), profile, i);
    }
}

// ---------- The tests ----------

#[test]
fn lifecycle_random_small_pools() {
    run_batch(Profile::Small, 20, 1);
}

#[test]
fn lifecycle_random_medium_pools() {
    run_batch(Profile::Medium, 20, 2);
}

#[test]
fn lifecycle_random_extreme_params() {
    run_batch(Profile::Extreme, 20, 3);
}

#[test]
fn lifecycle_max_scale_200_wallets() {
    run_batch(Profile::Max200, 2, 4);
}

/// A degenerate edge: every deposit was refunded inside the AML window →
/// pool_fixed == 0 (undersubscribed). The activation passes, there is nothing to
/// claim, there are no proceeds, and the wallets got everything back before fixation.
#[test]
fn lifecycle_full_refund_pool_zero() {
    const DEP: u64 = 20_000_000; // $20

    let mut env = TestEnv::new();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let owner = env.actors.owner.insecure_clone();
    let admin = env.actors.admin.insecure_clone();
    let operator = env.actors.operator.insecure_clone();
    let users: Vec<Keypair> = env
        .actors
        .users
        .iter()
        .map(|u| u.insecure_clone())
        .collect();

    let p = env.init_default_sale();
    env.warp_to(p.sale_start_date);
    for u in &users {
        let ata = env.create_ata(&u.pubkey(), &usdc);
        env.mint_to(&usdc, &ata, DEP);
        send(&mut env, init_aggregate_ix(u.pubkey()), u).expect("init_aggregate");
        send(&mut env, purchase_ix(u.pubkey(), ata, 0, DEP), u).expect("purchase");
    }

    env.warp_to(p.sale_end_date);
    for u in &users {
        send(
            &mut env,
            refund_ix(operator.pubkey(), u.pubkey(), 0, usdc),
            &operator,
        )
        .expect("refund");
        assert_eq!(
            env.token_balance(&refund_account_pda(&u.pubkey(), 0)),
            DEP,
            "the AML refund returned the whole deposit into the wallet's refund account"
        );
    }
    assert_eq!(
        env.sale_state().deposits_usdc_contributed,
        0,
        "the pool is empty"
    );
    assert_eq!(
        env.token_balance(&deposit_vault_pda()),
        0,
        "the vault is empty (I2)"
    );

    // Undersubscribed (0 is not oversubscribed): refund_vault is not passed and no mint
    // is needed.
    send(
        &mut env,
        activate_claim_ix(admin.pubkey(), usdc, None),
        &admin,
    )
    .expect("activate_claim on an empty pool");
    assert_eq!(env.sale_state().pool_fixed_amount, 0, "0 was fixed");
    assert_eq!(
        env.token_balance(&proceeds_vault_pda()),
        0,
        "there are no proceeds"
    );

    env.warp_to(p.claim_start_date);
    let nothing = ERROR_CODE_OFFSET + SaleError::NothingToClaim as u32;
    for u in &users {
        let r = send(&mut env, claim_ix(u.pubkey(), usdc, win, false), u);
        assert_eq!(
            custom_error_code(&r),
            Some(nothing),
            "net == 0 → NothingToClaim: {r:?}"
        );
    }
    send(
        &mut env,
        withdraw_proceeds_ix(owner.pubkey(), usdc, owner.pubkey()),
        &owner,
    )
    .expect("withdraw_proceeds (0)");
    assert_eq!(
        bal0(&env, &wallet_usdc_ata(&owner.pubkey(), &usdc)),
        0,
        "proceeds of 0"
    );

    env.warp_to(p.claim_start_date + p.claim_duration);
    let pairs: Vec<(Pubkey, Pubkey)> = users
        .iter()
        .map(|u| (aggregate_pda(&u.pubkey()), u.pubkey()))
        .collect();
    send(
        &mut env,
        close_aggregates_ix(operator.pubkey(), &pairs),
        &operator,
    )
    .expect("close_aggregates");
    send(
        &mut env,
        sweep_deposit_vault_ix(owner.pubkey(), usdc, owner.pubkey()),
        &owner,
    )
    .expect("sweep_deposit_vault");
    send(
        &mut env,
        sweep_token_vault_ix(owner.pubkey(), win, owner.pubkey()),
        &owner,
    )
    .expect("sweep_token_vault");
    for u in &users {
        assert!(
            !env.aggregate_exists(&u.pubkey()),
            "the aggregate is closed"
        );
        assert_eq!(
            env.token_balance(&refund_account_pda(&u.pubkey(), 0)),
            DEP,
            "the wallet kept all of its money"
        );
    }
}
