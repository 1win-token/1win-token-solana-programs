//! The deposit path: `init_aggregate` (creating a wallet's aggregate) and `purchase`
//! (accepting USDC into a contribution) - Arch §9.5, §9.6, §9.6a; TRD §5.3.
//! tests-bind-to-real-code: a real call to the real instructions through the
//! builders; the expectations are independent literals or reference values
//! (`P1_NET`/`P3_NET` plus arithmetic done by hand), NOT a recomputation of the
//! contract's formula. The assertions run against observable effects (the vault's
//! balance, the ATA's balance, the state accounts' fields) and against a SPECIFIC
//! error code.
//!
//! The invariants under test:
//! - I1 (§3): `aggregate.net == Σ of the wallet's deposit amounts`,
//!   `deposit_count == the number of deposits`, and `deposit.index` is monotonic
//!   from 0.
//! - I2 (§3): `sale_state.deposits_usdc_contributed == Σ of net across all wallets`,
//!   and physically `== deposit_vault.amount` along the intake path (there are no
//!   stray inflows in the test).
//! - I13 (§9.6a): with a live Graylist record the limits come ONLY from it and fully
//!   replace the global ones; with none, only the global ones apply.
//!
//! The Sale phase: `init_default_sale()` followed by `warp_to(p.sale_start_date)`
//! (see phase_matrix.rs). PreSale is before `sale_start_date`; SaleEnded is
//! `warp_to(sale_end_date)`.

use anchor_lang::error::{ErrorCode as AnchorError, ERROR_CODE_OFFSET};
use anchor_lang::solana_program::pubkey::bytes_are_curve_point;
use anchor_lang::{AnchorSerialize, Discriminator};
use sale::errors::SaleError;
use sale::state::WalletAggregate;
use sale_test_common::{
    aggregate_pda, blacklist_add_ix, blacklist_pda, blacklist_remove_ix, custom_error_code,
    deposit_pda, deposit_vault_pda,
    expected::{P1_NET, P3_NET},
    graylist_create_ix, graylist_pda, init_aggregate_ix, init_aggregate_ix_with_slots, pause_ix,
    purchase_ix, purchase_ix_with_slots, TestEnv, ACTOR_LAMPORTS,
};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

fn err(variant: SaleError) -> u32 {
    ERROR_CODE_OFFSET + variant as u32
}

fn anchor_err(code: AnchorError) -> u32 {
    code as u32
}

/// A short alias for the re-exported `custom_error_code` (in blacklist.rs it is
/// called directly; here there are many call sites and the alias keeps the lines
/// narrow).
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

/// Creates a USDC ATA for the user and mints `amount` units into it. Returns the
/// ATA's address. (The mint authority is the environment's `payer`, see token.rs.)
fn funded_ata(env: &mut TestEnv, wallet: &Pubkey, amount: u64) -> Pubkey {
    let usdc = env.usdc_mint;
    let ata = env.create_ata(wallet, &usdc);
    env.mint_to(&usdc, &ata, amount);
    ata
}

/// The `deposit_index` of a wallet's next purchase = its current `deposit_count`.
fn next_index(env: &TestEnv, wallet: &Pubkey) -> u64 {
    env.wallet_aggregate(wallet).deposit_count
}

// ===================================================================
// init_aggregate - POSITIVE (§9.5)
// ===================================================================

#[test]
fn init_aggregate_creates_zeroed_aggregate_and_counts_participant() {
    let mut env = TestEnv::new();
    let user_a = env.actors.users[0].insecure_clone();
    let user_b = env.actors.users[1].insecure_clone();
    let p = env.init_default_sale();
    env.warp_to(p.sale_start_date); // entering Sale

    assert_eq!(
        env.sale_state().participants,
        0,
        "precondition: there are no participants yet"
    );
    assert!(
        !env.aggregate_exists(&user_a.pubkey()),
        "A's aggregate does not exist yet"
    );

    // The first wallet.
    send(&mut env, init_aggregate_ix(user_a.pubkey()), &user_a)
        .expect("init_aggregate for user_a passes");
    let agg = env.wallet_aggregate(&user_a.pubkey());
    assert_eq!(
        agg.wallet,
        user_a.pubkey(),
        "the stored wallet == the wallet"
    );
    assert_eq!(agg.net_usdc_contributed, 0, "net = 0 on creation");
    assert_eq!(agg.deposit_count, 0, "deposit_count = 0 on creation");
    assert_eq!(
        env.sale_state().participants,
        1,
        "participants += 1 after the first aggregate"
    );

    // A second, DIFFERENT wallet → participants == 2.
    env.expire_blockhash();
    send(&mut env, init_aggregate_ix(user_b.pubkey()), &user_b)
        .expect("init_aggregate for user_b passes");
    assert_eq!(
        env.sale_state().participants,
        2,
        "participants == 2 after two different wallets"
    );
}

// ===================================================================
// init_aggregate - NEGATIVE, phase / pause / repeat (§9.5)
// ===================================================================

#[test]
fn init_aggregate_rejected_in_presale() {
    let mut env = TestEnv::new();
    let user = env.actors.users[0].insecure_clone();
    let _p = env.init_default_sale();
    // We do NOT warp - we stay in PreSale (now < sale_start_date).

    let r = send(&mut env, init_aggregate_ix(user.pubkey()), &user);
    assert_eq!(
        custom(&r),
        Some(err(SaleError::WrongPhase)),
        "init_aggregate in PreSale → WrongPhase: {r:?}"
    );
    assert!(
        !env.aggregate_exists(&user.pubkey()),
        "a failed init creates no aggregate"
    );
    assert_eq!(
        env.sale_state().participants,
        0,
        "participants is untouched"
    );
}

#[test]
fn init_aggregate_rejected_in_sale_ended() {
    let mut env = TestEnv::new();
    let user = env.actors.users[0].insecure_clone();
    let p = env.init_default_sale();
    env.warp_to(p.sale_end_date); // sale_end_date → SaleEnded (the boundary, phase.rs)

    let r = send(&mut env, init_aggregate_ix(user.pubkey()), &user);
    assert_eq!(
        custom(&r),
        Some(err(SaleError::WrongPhase)),
        "init_aggregate in SaleEnded → WrongPhase: {r:?}"
    );
    assert!(
        !env.aggregate_exists(&user.pubkey()),
        "a failed init creates no aggregate"
    );
}

#[test]
fn init_aggregate_rejected_when_paused() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let user = env.actors.users[0].insecure_clone();
    let p = env.init_default_sale();
    env.warp_to(p.sale_start_date);

    // Admin sets the pause (pause_ix is allowed in Sale).
    send(&mut env, pause_ix(admin.pubkey()), &admin).expect("admin: pause passes");
    assert!(
        env.sale_state().paused,
        "precondition: the pause is in place"
    );

    env.expire_blockhash();
    let r = send(&mut env, init_aggregate_ix(user.pubkey()), &user);
    assert_eq!(
        custom(&r),
        Some(err(SaleError::SalePaused)),
        "init_aggregate while paused → SalePaused: {r:?}"
    );
    assert!(
        !env.aggregate_exists(&user.pubkey()),
        "no aggregate is created while paused"
    );
}

#[test]
fn init_aggregate_double_same_wallet_fails() {
    // A repeated init of the same wallet fails structurally on the `init` of an occupied
    // account - System `account already in use` = Custom(0) (the same fact as the
    // double-add in blacklist.rs). participants must NOT grow a second time.
    let mut env = TestEnv::new();
    let user = env.actors.users[0].insecure_clone();
    let p = env.init_default_sale();
    env.warp_to(p.sale_start_date);

    send(&mut env, init_aggregate_ix(user.pubkey()), &user).expect("the first init passes");
    assert_eq!(env.sale_state().participants, 1, "participants == 1");

    env.expire_blockhash();
    let r = send(&mut env, init_aggregate_ix(user.pubkey()), &user);
    assert_eq!(
        custom(&r),
        Some(0),
        "a repeated init → System `account already in use` = Custom(0): {r:?}"
    );
    assert_eq!(
        env.sale_state().participants,
        1,
        "participants did not grow on the failed repeat"
    );
}

// ===================================================================
// init_aggregate — Blacklist (§9.5)
// ===================================================================

#[test]
fn init_aggregate_clean_passes_blacklisted_rejected() {
    // Both branches of the guard in one config: a clean wallet creates an aggregate (OK)
    // while a blocked one gets WalletBlacklisted, has no aggregate, and participants
    // does not grow.
    let mut env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();
    let clean = env.actors.users[0].insecure_clone();
    let blocked = env.actors.users[1].insecure_clone();
    let p = env.init_default_sale();
    env.warp_to(p.sale_start_date);

    send(
        &mut env,
        blacklist_add_ix(operator.pubkey(), blocked.pubkey()),
        &operator,
    )
    .expect("operator: blacklist_add");
    assert!(
        env.is_blacklisted(&blocked.pubkey()),
        "blocked is in the blacklist"
    );
    assert!(
        !env.is_blacklisted(&clean.pubkey()),
        "clean is not in the blacklist"
    );

    // POSITIVE: the clean wallet creates an aggregate.
    env.expire_blockhash();
    send(&mut env, init_aggregate_ix(clean.pubkey()), &clean)
        .expect("the clean wallet creates an aggregate");
    assert!(
        env.aggregate_exists(&clean.pubkey()),
        "the clean wallet's aggregate was created"
    );
    assert_eq!(env.sale_state().participants, 1, "participants == 1");

    // NEGATIVE: the blocked one is rejected.
    env.expire_blockhash();
    let r = send(&mut env, init_aggregate_ix(blocked.pubkey()), &blocked);
    assert_eq!(
        custom(&r),
        Some(err(SaleError::WalletBlacklisted)),
        "the blocked wallet → WalletBlacklisted: {r:?}"
    );
    assert!(
        !env.aggregate_exists(&blocked.pubkey()),
        "no aggregate is created for the blocked wallet"
    );
    assert_eq!(
        env.sale_state().participants,
        1,
        "participants did not grow on the rejected wallet"
    );
}

#[test]
fn init_aggregate_passes_after_blacklist_remove() {
    // A full cycle on one wallet: block → init rejected → remove the record (close
    // returns the slot's owner to System) → init passes.
    let mut env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();
    let user = env.actors.users[0].insecure_clone();
    let p = env.init_default_sale();
    env.warp_to(p.sale_start_date);

    send(
        &mut env,
        blacklist_add_ix(operator.pubkey(), user.pubkey()),
        &operator,
    )
    .expect("operator: blacklist_add");

    env.expire_blockhash();
    let r = send(&mut env, init_aggregate_ix(user.pubkey()), &user);
    assert_eq!(
        custom(&r),
        Some(err(SaleError::WalletBlacklisted)),
        "while the record is live → WalletBlacklisted: {r:?}"
    );
    assert_eq!(
        env.sale_state().participants,
        0,
        "participants is untouched"
    );

    env.expire_blockhash();
    send(
        &mut env,
        blacklist_remove_ix(operator.pubkey(), user.pubkey()),
        &operator,
    )
    .expect("operator: blacklist_remove");
    assert!(
        !env.is_blacklisted(&user.pubkey()),
        "the record was removed"
    );

    env.expire_blockhash();
    send(&mut env, init_aggregate_ix(user.pubkey()), &user)
        .expect("after the record is removed init passes");
    assert!(
        env.aggregate_exists(&user.pubkey()),
        "the aggregate was created"
    );
    assert_eq!(env.sale_state().participants, 1, "participants == 1");
}

#[test]
fn init_aggregate_blacklist_slot_for_wrong_wallet_fails_constraint_seeds() {
    // Bypassing the gate by substituting the slot: a blocked wallet submits ANOTHER
    // wallet's empty slot. The slot's address derives from the user's seeds →
    // ConstraintSeeds.
    let mut env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();
    let user = env.actors.users[0].insecure_clone();
    let other = Pubkey::new_unique();
    let p = env.init_default_sale();
    env.warp_to(p.sale_start_date);

    send(
        &mut env,
        blacklist_add_ix(operator.pubkey(), user.pubkey()),
        &operator,
    )
    .expect("operator: blacklist_add");

    env.expire_blockhash();
    let r = send(
        &mut env,
        init_aggregate_ix_with_slots(user.pubkey(), blacklist_pda(&other)),
        &user,
    );
    assert_eq!(
        custom(&r),
        Some(anchor_err(AnchorError::ConstraintSeeds)),
        "the blacklist slot is not derived from user → ConstraintSeeds (2006): {r:?}"
    );
    assert!(
        !env.aggregate_exists(&user.pubkey()),
        "no aggregate is created on a blacklist-slot mismatch"
    );
    assert_eq!(
        env.sale_state().participants,
        0,
        "participants is untouched"
    );
}

// ===================================================================
// purchase - the happy path and the I1/I2 accounting (§9.6, §3)
// ===================================================================

#[test]
fn purchase_records_deposit_and_increments_counters() {
    // init_aggregate plus purchase($10,000 = P1_NET). The assertions cover REAL effects:
    // the vault's balance +amount, the ATA's balance −amount, and the deposit, aggregate
    // and state fields.
    let mut env = TestEnv::new();
    let user = env.actors.users[0].insecure_clone();
    let p = env.init_default_sale();
    env.warp_to(p.sale_start_date);

    let amount = P1_NET; // $10,000 - the valid per-tx maximum
    let ata = funded_ata(&mut env, &user.pubkey(), amount);
    let vault = deposit_vault_pda();

    assert_eq!(
        env.token_balance(&vault),
        0,
        "the vault is empty before the purchase"
    );
    assert_eq!(
        env.token_balance(&ata),
        amount,
        "the ATA holds exactly amount"
    );

    send(&mut env, init_aggregate_ix(user.pubkey()), &user).expect("init_aggregate");
    env.expire_blockhash();
    send(&mut env, purchase_ix(user.pubkey(), ata, 0, amount), &user).expect("purchase passes");

    // Balances (the observable effect of the transfer CPI).
    assert_eq!(
        env.token_balance(&vault),
        amount,
        "deposit_vault grew by exactly amount"
    );
    assert_eq!(env.token_balance(&ata), 0, "the ATA decreased by amount");

    // The deposit.
    let dep = env.deposit_entry(&user.pubkey(), 0);
    assert_eq!(dep.wallet, user.pubkey(), "deposit.wallet == the wallet");
    assert_eq!(dep.amount, amount, "deposit.amount == amount");
    assert_eq!(dep.index, 0, "deposit.index == 0");

    // The aggregate (I1).
    let agg = env.wallet_aggregate(&user.pubkey());
    assert_eq!(agg.net_usdc_contributed, amount, "net == amount");
    assert_eq!(agg.deposit_count, 1, "deposit_count == 1");

    // The state (I2).
    assert_eq!(
        env.sale_state().deposits_usdc_contributed,
        amount,
        "deposits_usdc_contributed == amount"
    );
}

#[test]
fn purchase_three_times_accumulates_net_and_indices_i1() {
    // I1: 3 purchases in a row by one wallet (indices 0, 1, 2). net == the sum,
    // deposit_count == 3, and every depo.index == i with the right .amount.
    let mut env = TestEnv::new();
    let user = env.actors.users[0].insecure_clone();
    let p = env.init_default_sale();
    env.warp_to(p.sale_start_date);

    // Three DIFFERENT amounts (all valid per tx): so that .amount differs by index.
    let amounts: [u64; 3] = [P3_NET, P3_NET * 2, P3_NET * 3];
    let total: u64 = amounts.iter().sum();
    let ata = funded_ata(&mut env, &user.pubkey(), total);
    let vault = deposit_vault_pda();

    send(&mut env, init_aggregate_ix(user.pubkey()), &user).expect("init_aggregate");

    let mut running = 0u64;
    for (i, &amount) in amounts.iter().enumerate() {
        let idx = next_index(&env, &user.pubkey());
        assert_eq!(idx as usize, i, "the next index == i before the purchase");
        env.expire_blockhash();
        send(
            &mut env,
            purchase_ix(user.pubkey(), ata, idx, amount),
            &user,
        )
        .unwrap_or_else(|e| panic!("purchase #{i} passes: {e:?}"));

        running += amount;
        let dep = env.deposit_entry(&user.pubkey(), i as u64);
        assert_eq!(dep.index, i as u64, "deposit #{i}.index == {i}");
        assert_eq!(dep.amount, amount, "deposit #{i}.amount is correct");
        let agg = env.wallet_aggregate(&user.pubkey());
        assert_eq!(
            agg.deposit_count,
            (i + 1) as u64,
            "deposit_count after #{i}"
        );
        assert_eq!(
            agg.net_usdc_contributed, running,
            "net accumulated after #{i}"
        );
    }

    let agg = env.wallet_aggregate(&user.pubkey());
    assert_eq!(
        agg.net_usdc_contributed, total,
        "the final net == the total of them all"
    );
    assert_eq!(agg.deposit_count, 3, "deposit_count == 3");
    assert_eq!(
        env.token_balance(&vault),
        total,
        "the vault == the total of all the purchases"
    );
}

#[test]
fn purchase_two_wallets_sum_into_state_and_vault_i2() {
    // I2: two wallets buy → deposits_usdc_contributed == the sum of both nets, and
    // physically == deposit_vault.amount.
    let mut env = TestEnv::new();
    let user_a = env.actors.users[0].insecure_clone();
    let user_b = env.actors.users[1].insecure_clone();
    let p = env.init_default_sale();
    env.warp_to(p.sale_start_date);

    let amount_a = P3_NET * 4;
    let amount_b = P3_NET * 7;
    let ata_a = funded_ata(&mut env, &user_a.pubkey(), amount_a);
    let ata_b = funded_ata(&mut env, &user_b.pubkey(), amount_b);
    let vault = deposit_vault_pda();

    send(&mut env, init_aggregate_ix(user_a.pubkey()), &user_a).expect("init A");
    env.expire_blockhash();
    send(&mut env, init_aggregate_ix(user_b.pubkey()), &user_b).expect("init B");

    env.expire_blockhash();
    send(
        &mut env,
        purchase_ix(user_a.pubkey(), ata_a, 0, amount_a),
        &user_a,
    )
    .expect("purchase A");
    env.expire_blockhash();
    send(
        &mut env,
        purchase_ix(user_b.pubkey(), ata_b, 0, amount_b),
        &user_b,
    )
    .expect("purchase B");

    let expected_total = amount_a + amount_b;
    assert_eq!(
        env.wallet_aggregate(&user_a.pubkey()).net_usdc_contributed,
        amount_a,
        "net A"
    );
    assert_eq!(
        env.wallet_aggregate(&user_b.pubkey()).net_usdc_contributed,
        amount_b,
        "net B"
    );
    assert_eq!(
        env.sale_state().deposits_usdc_contributed,
        expected_total,
        "deposits_usdc_contributed == net_A + net_B"
    );
    assert_eq!(
        env.token_balance(&vault),
        expected_total,
        "deposit_vault.amount == deposits_usdc_contributed (I2)"
    );
}

// ===================================================================
// purchase - the global limits, the boundaries min−1 / min / max / max+1 (§9.6a)
// valid_init_params: min_per_tx = P3_NET, max_per_tx = P1_NET.
// ===================================================================

#[test]
fn purchase_global_limit_boundaries() {
    let mut env = TestEnv::new();
    let user = env.actors.users[0].insecure_clone();
    let p = env.init_default_sale();
    env.warp_to(p.sale_start_date);

    // We mint with room to spare: min-1 + min + max + (max+1).
    let total_budget = (P3_NET - 1) + P3_NET + P1_NET + (P1_NET + 1);
    let ata = funded_ata(&mut env, &user.pubkey(), total_budget);
    send(&mut env, init_aggregate_ix(user.pubkey()), &user).expect("init_aggregate");

    // min−1 → BelowMin (nothing is debited and no deposit is created).
    env.expire_blockhash();
    let r = send(
        &mut env,
        purchase_ix(user.pubkey(), ata, 0, P3_NET - 1),
        &user,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::BelowMin)),
        "amount = min−1 → BelowMin: {r:?}"
    );
    assert!(
        !env.deposit_exists(&user.pubkey(), 0),
        "BelowMin creates no deposit"
    );
    assert_eq!(
        env.wallet_aggregate(&user.pubkey()).deposit_count,
        0,
        "deposit_count did not grow on BelowMin"
    );

    // exactly min (P3_NET) → OK, index 0.
    env.expire_blockhash();
    send(&mut env, purchase_ix(user.pubkey(), ata, 0, P3_NET), &user).expect("amount = min passes");
    assert_eq!(env.deposit_entry(&user.pubkey(), 0).amount, P3_NET);

    // exactly max (P1_NET) → OK, index 1.
    env.expire_blockhash();
    send(&mut env, purchase_ix(user.pubkey(), ata, 1, P1_NET), &user).expect("amount = max passes");
    assert_eq!(env.deposit_entry(&user.pubkey(), 1).amount, P1_NET);

    // max+1 → AboveMax, index 2 is not created.
    env.expire_blockhash();
    let r = send(
        &mut env,
        purchase_ix(user.pubkey(), ata, 2, P1_NET + 1),
        &user,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::AboveMax)),
        "amount = max+1 → AboveMax: {r:?}"
    );
    assert!(
        !env.deposit_exists(&user.pubkey(), 2),
        "AboveMax creates no deposit"
    );
    assert_eq!(
        env.wallet_aggregate(&user.pubkey()).deposit_count,
        2,
        "after two successful purchases deposit_count == 2"
    );
}

// ===================================================================
// purchase - phase / pause (§9.6, §4.2)
// ===================================================================

#[test]
fn purchase_rejected_in_sale_ended() {
    // `purchase`'s phase gate on an existing aggregate: we create the aggregate in Sale,
    // MOVE time into SaleEnded and try to purchase → WrongPhase.
    //
    // The PreSale branch of purchase cannot be covered by a SEPARATE test: an aggregate
    // is only created in Sale (init_aggregate gates the phase itself), while purchase
    // requires an existing aggregate (a `mut` account, resolved BEFORE the body). In
    // PreSale there is no aggregate → the failure is AccountNotInitialized during
    // account resolution, and the body with its phase check never runs. The phase
    // protection of the entry into PreSale is covered at the init_aggregate level
    // (init_aggregate_rejected_in_presale).
    let mut env = TestEnv::new();
    let user = env.actors.users[0].insecure_clone();
    let p = env.init_default_sale();
    env.warp_to(p.sale_start_date);

    let amount = P3_NET;
    let ata = funded_ata(&mut env, &user.pubkey(), amount);
    send(&mut env, init_aggregate_ix(user.pubkey()), &user).expect("init_aggregate in Sale");

    // We move into SaleEnded (sale_end_date) with the aggregate already existing.
    env.warp_to(p.sale_end_date);
    env.expire_blockhash();
    let r = send(&mut env, purchase_ix(user.pubkey(), ata, 0, amount), &user);
    assert_eq!(
        custom(&r),
        Some(err(SaleError::WrongPhase)),
        "purchase in SaleEnded → WrongPhase: {r:?}"
    );
    assert!(
        !env.deposit_exists(&user.pubkey(), 0),
        "no deposit is created in SaleEnded"
    );
    assert_eq!(env.token_balance(&ata), amount, "no USDC was debited");
}

#[test]
fn purchase_boundary_end_minus_one_ok_at_end_denied() {
    // The sale_end_date boundary: at t−1, the last second of Sale, a purchase passes;
    // at t it is SaleEnded → WrongPhase.
    let mut env = TestEnv::new();
    let user = env.actors.users[0].insecure_clone();
    let p = env.init_default_sale();
    env.warp_to(p.sale_start_date);

    let amount = P3_NET;
    let ata = funded_ata(&mut env, &user.pubkey(), amount * 2);
    send(&mut env, init_aggregate_ix(user.pubkey()), &user).expect("init_aggregate");

    // t = end − 1: still Sale → it passes.
    env.warp_to(p.sale_end_date - 1);
    env.expire_blockhash();
    send(&mut env, purchase_ix(user.pubkey(), ata, 0, amount), &user)
        .expect("a purchase at end−1 (the last second of Sale) passes");
    assert_eq!(
        env.deposit_entry(&user.pubkey(), 0).amount,
        amount,
        "the deposit was created at end−1"
    );

    // t = end: SaleEnded → WrongPhase.
    env.warp_to(p.sale_end_date);
    env.expire_blockhash();
    let r = send(&mut env, purchase_ix(user.pubkey(), ata, 1, amount), &user);
    assert_eq!(
        custom(&r),
        Some(err(SaleError::WrongPhase)),
        "a purchase exactly at end → WrongPhase: {r:?}"
    );
    assert!(
        !env.deposit_exists(&user.pubkey(), 1),
        "no deposit is created at end"
    );
}

#[test]
fn purchase_rejected_when_paused() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let user = env.actors.users[0].insecure_clone();
    let p = env.init_default_sale();
    env.warp_to(p.sale_start_date);

    let amount = P3_NET;
    let ata = funded_ata(&mut env, &user.pubkey(), amount);
    send(&mut env, init_aggregate_ix(user.pubkey()), &user).expect("init_aggregate");

    env.expire_blockhash();
    send(&mut env, pause_ix(admin.pubkey()), &admin).expect("admin: pause");

    env.expire_blockhash();
    let r = send(&mut env, purchase_ix(user.pubkey(), ata, 0, amount), &user);
    assert_eq!(
        custom(&r),
        Some(err(SaleError::SalePaused)),
        "a purchase while paused → SalePaused: {r:?}"
    );
    assert_eq!(
        env.token_balance(&ata),
        amount,
        "no USDC was debited while paused"
    );
}

// ===================================================================
// purchase - no aggregate (§9.6: the aggregate must exist)
// ===================================================================

#[test]
fn purchase_without_aggregate_fails_account_not_initialized() {
    let mut env = TestEnv::new();
    let user = env.actors.users[0].insecure_clone();
    let p = env.init_default_sale();
    env.warp_to(p.sale_start_date);

    // init_aggregate was never called - there is no aggregate.
    let amount = P3_NET;
    let ata = funded_ata(&mut env, &user.pubkey(), amount);
    assert!(
        !env.aggregate_exists(&user.pubkey()),
        "precondition: there is no aggregate"
    );

    let r = send(&mut env, purchase_ix(user.pubkey(), ata, 0, amount), &user);
    assert_eq!(
        custom(&r),
        Some(anchor_err(AnchorError::AccountNotInitialized)),
        "a purchase with no aggregate → AccountNotInitialized (3012): {r:?}"
    );
    assert_eq!(env.token_balance(&ata), amount, "no USDC was debited");
}

// ===================================================================
// purchase — Blacklist (§9.6)
// ===================================================================

#[test]
fn purchase_clean_passes_blacklisted_rejected() {
    // Both branches of the guard in one config: a clean wallet buys (OK) while a blocked
    // one gets WalletBlacklisted.
    let mut env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();
    let clean = env.actors.users[0].insecure_clone();
    let blocked = env.actors.users[1].insecure_clone();
    let p = env.init_default_sale();
    env.warp_to(p.sale_start_date);

    let amount = P3_NET;
    let ata_clean = funded_ata(&mut env, &clean.pubkey(), amount);
    let ata_blocked = funded_ata(&mut env, &blocked.pubkey(), amount);

    send(&mut env, init_aggregate_ix(clean.pubkey()), &clean).expect("init clean");
    env.expire_blockhash();
    send(&mut env, init_aggregate_ix(blocked.pubkey()), &blocked).expect("init blocked");

    // We block `blocked` (as operator). The clean wallet is left alone.
    env.expire_blockhash();
    send(
        &mut env,
        blacklist_add_ix(operator.pubkey(), blocked.pubkey()),
        &operator,
    )
    .expect("operator: blacklist_add");
    assert!(
        env.is_blacklisted(&blocked.pubkey()),
        "blocked is in the blacklist"
    );
    assert!(
        !env.is_blacklisted(&clean.pubkey()),
        "clean is not in the blacklist"
    );

    // POSITIVE: the clean wallet buys.
    env.expire_blockhash();
    send(
        &mut env,
        purchase_ix(clean.pubkey(), ata_clean, 0, amount),
        &clean,
    )
    .expect("the clean wallet buys");
    assert_eq!(
        env.deposit_entry(&clean.pubkey(), 0).amount,
        amount,
        "the clean wallet's deposit was created"
    );

    // NEGATIVE: the blocked one is rejected and no USDC is debited.
    env.expire_blockhash();
    let r = send(
        &mut env,
        purchase_ix(blocked.pubkey(), ata_blocked, 0, amount),
        &blocked,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::WalletBlacklisted)),
        "the blocked wallet → WalletBlacklisted: {r:?}"
    );
    assert!(
        !env.deposit_exists(&blocked.pubkey(), 0),
        "no deposit is created for the blocked wallet"
    );
    assert_eq!(
        env.token_balance(&ata_blocked),
        amount,
        "the blocked wallet's USDC was not debited"
    );
}

#[test]
fn purchase_passes_again_after_blacklist_remove() {
    // The list check relies on remove-through-close returning the slot's owner to
    // System. We run a full cycle on one wallet: block → the purchase is rejected →
    // remove → the purchase passes.
    let mut env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();
    let user = env.actors.users[0].insecure_clone();
    let p = env.init_default_sale();
    env.warp_to(p.sale_start_date);

    let amount = P3_NET;
    let ata = funded_ata(&mut env, &user.pubkey(), amount);
    send(&mut env, init_aggregate_ix(user.pubkey()), &user).expect("init_aggregate");

    env.expire_blockhash();
    send(
        &mut env,
        blacklist_add_ix(operator.pubkey(), user.pubkey()),
        &operator,
    )
    .expect("operator: blacklist_add");

    env.expire_blockhash();
    let r = send(&mut env, purchase_ix(user.pubkey(), ata, 0, amount), &user);
    assert_eq!(
        custom(&r),
        Some(err(SaleError::WalletBlacklisted)),
        "while the record is live → WalletBlacklisted: {r:?}"
    );

    env.expire_blockhash();
    send(
        &mut env,
        blacklist_remove_ix(operator.pubkey(), user.pubkey()),
        &operator,
    )
    .expect("operator: blacklist_remove");
    assert!(
        !env.is_blacklisted(&user.pubkey()),
        "the record was removed"
    );

    env.expire_blockhash();
    send(&mut env, purchase_ix(user.pubkey(), ata, 0, amount), &user)
        .expect("after the record is removed the purchase passes");
    assert_eq!(
        env.deposit_entry(&user.pubkey(), 0).amount,
        amount,
        "the deposit was created after unblocking"
    );
    assert_eq!(
        env.token_balance(&ata),
        0,
        "the USDC was debited into the vault"
    );
}

#[test]
fn purchase_blacklist_slot_for_wrong_wallet_fails_constraint_seeds() {
    // Substituting the blacklist slot: we pass a slot derived from a DIFFERENT wallet
    // than the user account (a seed mismatch) → ConstraintSeeds. The slot is empty
    // (nobody was blocked) - the address check on an UncheckedAccount runs purely on the
    // seeds.
    let mut env = TestEnv::new();
    let user = env.actors.users[0].insecure_clone();
    let other = Pubkey::new_unique();
    let p = env.init_default_sale();
    env.warp_to(p.sale_start_date);

    let amount = P3_NET;
    let ata = funded_ata(&mut env, &user.pubkey(), amount);
    send(&mut env, init_aggregate_ix(user.pubkey()), &user).expect("init_aggregate");

    env.expire_blockhash();
    let r = send(
        &mut env,
        purchase_ix_with_slots(
            user.pubkey(),
            aggregate_pda(&user.pubkey()),
            deposit_pda(&user.pubkey(), 0),
            blacklist_pda(&other), // a slot not derived from user
            graylist_pda(&user.pubkey()),
            ata,
            deposit_vault_pda(),
            amount,
        ),
        &user,
    );
    assert_eq!(
        custom(&r),
        Some(anchor_err(AnchorError::ConstraintSeeds)),
        "the blacklist slot is not derived from user → ConstraintSeeds (2006): {r:?}"
    );
    assert!(
        !env.deposit_exists(&user.pubkey(), 0),
        "no deposit is created on a blacklist-slot mismatch"
    );
}

#[test]
fn purchase_live_foreign_blacklist_slot_fails_constraint_seeds() {
    // The slot is a REALLY existing blacklist marker of ANOTHER wallet. The address is
    // checked against the user's seeds and the marker being live plays no part: a
    // foreign PDA cannot be slipped into the user's slot → ConstraintSeeds.
    let mut env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();
    let user = env.actors.users[0].insecure_clone();
    let other = Pubkey::new_unique();
    let p = env.init_default_sale();
    env.warp_to(p.sale_start_date);

    let amount = P3_NET;
    let ata = funded_ata(&mut env, &user.pubkey(), amount);
    send(&mut env, init_aggregate_ix(user.pubkey()), &user).expect("init_aggregate");

    // We create a LIVE marker for `other`.
    env.expire_blockhash();
    send(
        &mut env,
        blacklist_add_ix(operator.pubkey(), other),
        &operator,
    )
    .expect("blacklist_add other");
    assert!(env.is_blacklisted(&other), "other's marker is live");

    env.expire_blockhash();
    let r = send(
        &mut env,
        purchase_ix_with_slots(
            user.pubkey(),
            aggregate_pda(&user.pubkey()),
            deposit_pda(&user.pubkey(), 0),
            blacklist_pda(&other), // a live foreign marker under the user's slot
            graylist_pda(&user.pubkey()),
            ata,
            deposit_vault_pda(),
            amount,
        ),
        &user,
    );
    assert_eq!(
        custom(&r),
        Some(anchor_err(AnchorError::ConstraintSeeds)),
        "a live foreign blacklist slot → ConstraintSeeds: {r:?}"
    );
}

// ===================================================================
// purchase — Graylist (§9.6a, I13)
// ===================================================================

#[test]
fn purchase_graylist_limits_fully_replace_global() {
    // I13: with a live Graylist record the limits come ONLY from it. Demonstrated in
    // both directions within one config:
    //  (a) an amount that passes the graylist but NOT the global limits (below
    //      global.min) - it passes, because the global ones do not apply;
    //  (b) an amount that passes the global limits (== global.min) but NOT the graylist
    //      (above graylist.max) - it is rejected with AboveMax.
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let user = env.actors.users[0].insecure_clone();
    let p = env.init_default_sale();

    // The global limits: min = P3_NET, max = P1_NET. The graylist is narrower than the
    // global ones:
    //   gl_min = 1            (< global.min)
    //   gl_max = P3_NET / 2   (< global.min ⇒ the graylist window lies entirely BELOW the global one)
    //   gl_total = P1_NET
    let gl_min = 1u64;
    let gl_max = P3_NET / 2;
    let gl_total = P1_NET;
    send(
        &mut env,
        graylist_create_ix(admin.pubkey(), user.pubkey(), gl_min, gl_max, gl_total),
        &admin,
    )
    .expect("admin: graylist_create (in PreSale, the phase does not gate it)");

    env.warp_to(p.sale_start_date);
    let ata = funded_ata(&mut env, &user.pubkey(), P1_NET);
    send(&mut env, init_aggregate_ix(user.pubkey()), &user).expect("init_aggregate");

    // (a) amount = gl_max: BELOW global.min (P3_NET) but inside the graylist window → OK.
    let pass_only_graylist = gl_max;
    assert!(
        pass_only_graylist < P3_NET,
        "test control: the amount is below the global min"
    );
    env.expire_blockhash();
    send(
        &mut env,
        purchase_ix(user.pubkey(), ata, 0, pass_only_graylist),
        &user,
    )
    .expect(
        "an amount inside the graylist window (below global.min) passes → the graylist limits replace the global ones",
    );
    assert_eq!(
        env.deposit_entry(&user.pubkey(), 0).amount,
        pass_only_graylist,
        "the deposit was created under the graylist limits"
    );

    // (b) amount = P3_NET: it passes the GLOBAL limits (== global.min) but is ABOVE
    // graylist.max → AboveMax (the global ones do not apply).
    let pass_only_global = P3_NET;
    assert!(
        pass_only_global > gl_max,
        "test control: the amount is above graylist.max"
    );
    env.expire_blockhash();
    let r = send(
        &mut env,
        purchase_ix(user.pubkey(), ata, 1, pass_only_global),
        &user,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::AboveMax)),
        "an amount above graylist.max (yet fine globally) → AboveMax: {r:?}"
    );
    assert!(
        !env.deposit_exists(&user.pubkey(), 1),
        "a purchase rejected by graylist.max creates no deposit"
    );
}

#[test]
fn purchase_below_graylist_min_rejected() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let user = env.actors.users[0].insecure_clone();
    let p = env.init_default_sale();

    // A graylist with min > global.min, so that the min boundary is the graylist's one.
    let gl_min = P3_NET * 2;
    let gl_max = P3_NET * 5;
    let gl_total = P1_NET;
    send(
        &mut env,
        graylist_create_ix(admin.pubkey(), user.pubkey(), gl_min, gl_max, gl_total),
        &admin,
    )
    .expect("graylist_create");

    env.warp_to(p.sale_start_date);
    let ata = funded_ata(&mut env, &user.pubkey(), P1_NET);
    send(&mut env, init_aggregate_ix(user.pubkey()), &user).expect("init_aggregate");

    // gl_min − 1: BELOW graylist.min even though it is ABOVE global.min (P3_NET) →
    // BelowMin.
    env.expire_blockhash();
    let r = send(
        &mut env,
        purchase_ix(user.pubkey(), ata, 0, gl_min - 1),
        &user,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::BelowMin)),
        "below graylist.min → BelowMin: {r:?}"
    );
    assert!(
        !env.deposit_exists(&user.pubkey(), 0),
        "no deposit was created"
    );
}

#[test]
fn purchase_above_graylist_max_rejected() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let user = env.actors.users[0].insecure_clone();
    let p = env.init_default_sale();

    let gl_min = P3_NET;
    let gl_max = P3_NET * 3;
    let gl_total = P1_NET;
    send(
        &mut env,
        graylist_create_ix(admin.pubkey(), user.pubkey(), gl_min, gl_max, gl_total),
        &admin,
    )
    .expect("graylist_create");

    env.warp_to(p.sale_start_date);
    let ata = funded_ata(&mut env, &user.pubkey(), P1_NET);
    send(&mut env, init_aggregate_ix(user.pubkey()), &user).expect("init_aggregate");

    // gl_max + 1: ABOVE graylist.max but BELOW global.max (P1_NET) → AboveMax.
    env.expire_blockhash();
    let r = send(
        &mut env,
        purchase_ix(user.pubkey(), ata, 0, gl_max + 1),
        &user,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::AboveMax)),
        "above graylist.max → AboveMax: {r:?}"
    );
    assert!(
        !env.deposit_exists(&user.pubkey(), 0),
        "no deposit was created"
    );
}

#[test]
fn purchase_graylist_max_total_boundary() {
    // max_total: a series of purchases. net+amount == max_total passes (the boundary);
    // the next one, making net+amount > max_total, gives WalletTotalExceeded.
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let user = env.actors.users[0].insecure_clone();
    let p = env.init_default_sale();

    // max_total is a tight boundary: exactly two purchases of P3_NET reach
    // max_total = 2*P3_NET and the third overshoots. The limits are self-consistent
    // (0 < min ≤ max ≤ max_total): min = max = P3_NET (per tx exactly P3_NET) and
    // max_total = 2*P3_NET.
    let gl_min = P3_NET;
    let gl_max = P3_NET;
    let gl_total = P3_NET * 2; // exactly the total of two P3_NET purchases
    send(
        &mut env,
        graylist_create_ix(admin.pubkey(), user.pubkey(), gl_min, gl_max, gl_total),
        &admin,
    )
    .expect("graylist_create");

    env.warp_to(p.sale_start_date);
    let ata = funded_ata(&mut env, &user.pubkey(), P3_NET * 4);
    send(&mut env, init_aggregate_ix(user.pubkey()), &user).expect("init_aggregate");

    // Purchase #1: net 0 → P3_NET (≤ max_total) - OK.
    env.expire_blockhash();
    send(&mut env, purchase_ix(user.pubkey(), ata, 0, P3_NET), &user).expect("purchase #1 passes");

    // Purchase #2: net P3_NET → 2*P3_NET == max_total (the ≤ boundary) - OK.
    env.expire_blockhash();
    send(&mut env, purchase_ix(user.pubkey(), ata, 1, P3_NET), &user)
        .expect("purchase #2: net+amount == max_total - the boundary passes");
    assert_eq!(
        env.wallet_aggregate(&user.pubkey()).net_usdc_contributed,
        gl_total,
        "net reached max_total exactly"
    );

    // Purchase #3: net 2*P3_NET → 3*P3_NET > max_total → WalletTotalExceeded.
    env.expire_blockhash();
    let r = send(&mut env, purchase_ix(user.pubkey(), ata, 2, P3_NET), &user);
    assert_eq!(
        custom(&r),
        Some(err(SaleError::WalletTotalExceeded)),
        "net+amount > max_total → WalletTotalExceeded: {r:?}"
    );
    assert!(
        !env.deposit_exists(&user.pubkey(), 2),
        "a purchase overshooting max_total creates no deposit"
    );
    assert_eq!(
        env.wallet_aggregate(&user.pubkey()).net_usdc_contributed,
        gl_total,
        "net did not change after the rejected purchase"
    );
}

#[test]
fn purchase_graylist_slot_for_wrong_wallet_fails_constraint_seeds() {
    // Substituting the graylist slot: a slot from ANOTHER wallet → ConstraintSeeds (the
    // address is checked against the user's seeds).
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let user = env.actors.users[0].insecure_clone();
    let other = Pubkey::new_unique();
    let p = env.init_default_sale();

    // A live graylist record for `other` - so that the slot points at a real account.
    send(
        &mut env,
        graylist_create_ix(admin.pubkey(), other, P3_NET, P3_NET * 5, P1_NET),
        &admin,
    )
    .expect("graylist_create other");

    env.warp_to(p.sale_start_date);
    let amount = P3_NET;
    let ata = funded_ata(&mut env, &user.pubkey(), amount);
    send(&mut env, init_aggregate_ix(user.pubkey()), &user).expect("init_aggregate");

    env.expire_blockhash();
    let r = send(
        &mut env,
        purchase_ix_with_slots(
            user.pubkey(),
            aggregate_pda(&user.pubkey()),
            deposit_pda(&user.pubkey(), 0),
            blacklist_pda(&user.pubkey()),
            graylist_pda(&other), // a slot not derived from user
            ata,
            deposit_vault_pda(),
            amount,
        ),
        &user,
    );
    assert_eq!(
        custom(&r),
        Some(anchor_err(AnchorError::ConstraintSeeds)),
        "the graylist slot is not derived from user → ConstraintSeeds: {r:?}"
    );
    assert!(
        !env.deposit_exists(&user.pubkey(), 0),
        "no deposit is created on a graylist-slot mismatch"
    );
}

// ===================================================================
// purchase - account substitution (§9.6, accounts-untrusted)
// ===================================================================

#[test]
fn purchase_foreign_deposit_vault_fails_constraint_address() {
    // deposit_vault is bound by `address = sale_state.deposit_vault`. We pass a FOREIGN
    // token account (an ATA of the same usdc mint) instead of the canonical vault →
    // ConstraintAddress: draining through a side vault is impossible.
    let mut env = TestEnv::new();
    let user = env.actors.users[0].insecure_clone();
    let attacker = env.actors.users[1].insecure_clone();
    let p = env.init_default_sale();
    env.warp_to(p.sale_start_date);

    let amount = P3_NET;
    let ata = funded_ata(&mut env, &user.pubkey(), amount);
    // A foreign USDC account (the same mint, a different owner) - a candidate for
    // substituting the vault.
    let fake_vault = funded_ata(&mut env, &attacker.pubkey(), 0);
    send(&mut env, init_aggregate_ix(user.pubkey()), &user).expect("init_aggregate");

    env.expire_blockhash();
    let r = send(
        &mut env,
        purchase_ix_with_slots(
            user.pubkey(),
            aggregate_pda(&user.pubkey()),
            deposit_pda(&user.pubkey(), 0),
            blacklist_pda(&user.pubkey()),
            graylist_pda(&user.pubkey()),
            ata,
            fake_vault, // not the canonical deposit_vault
            amount,
        ),
        &user,
    );
    assert_eq!(
        custom(&r),
        Some(anchor_err(AnchorError::ConstraintAddress)),
        "substituting deposit_vault → ConstraintAddress (2012): {r:?}"
    );
    assert_eq!(env.token_balance(&ata), amount, "no USDC was debited");
    assert_eq!(
        env.token_balance(&fake_vault),
        0,
        "the foreign vault was not credited"
    );
    assert!(
        !env.deposit_exists(&user.pubkey(), 0),
        "no deposit is created when the vault is substituted"
    );
}

#[test]
fn purchase_wrong_deposit_index_slot_fails_constraint_seeds() {
    // Substituting the deposit PDA: deposit_count == 0, yet we pass the deposit slot for
    // index 1. The init account's seeds derive from aggregate.deposit_count (0) and the
    // address passed in (index 1) does not match → ConstraintSeeds.
    let mut env = TestEnv::new();
    let user = env.actors.users[0].insecure_clone();
    let p = env.init_default_sale();
    env.warp_to(p.sale_start_date);

    let amount = P3_NET;
    let ata = funded_ata(&mut env, &user.pubkey(), amount);
    send(&mut env, init_aggregate_ix(user.pubkey()), &user).expect("init_aggregate");
    assert_eq!(
        env.wallet_aggregate(&user.pubkey()).deposit_count,
        0,
        "precondition: deposit_count == 0 (the canonical slot is index 0)"
    );

    env.expire_blockhash();
    let r = send(
        &mut env,
        purchase_ix_with_slots(
            user.pubkey(),
            aggregate_pda(&user.pubkey()),
            deposit_pda(&user.pubkey(), 1), // a slot for index 1 while count == 0
            blacklist_pda(&user.pubkey()),
            graylist_pda(&user.pubkey()),
            ata,
            deposit_vault_pda(),
            amount,
        ),
        &user,
    );
    assert_eq!(
        custom(&r),
        Some(anchor_err(AnchorError::ConstraintSeeds)),
        "a deposit slot for the wrong index → ConstraintSeeds: {r:?}"
    );
    assert!(
        !env.deposit_exists(&user.pubkey(), 1),
        "the index-1 deposit is not created"
    );
    assert_eq!(env.token_balance(&ata), amount, "no USDC was debited");
}

// ===================================================================
// The deposit path - a buyer with a private key of their own only (§9.5, §9.6)
// ===================================================================
//
// Both tests run in an environment without signature verification: a genuine
// signature cannot be produced for an address with no private key, yet what is
// needed is a real instruction call with such a signer. The program executes as the
// same `sale.so`; from its point of view the picture is the same as a multisig's
// `invoke_signed` - `is_signer == true` and the address is off-curve. So that the
// environment does not itself become the cause of a rejection, each test also runs
// the positive branch with an ordinary wallet alongside.

/// An address with no private key. `find_program_address` by construction yields a
/// point OFF the Ed25519 curve (it discards on-curve addresses), which is exactly
/// what a multisig or custody wallet's vault is.
fn off_curve_wallet() -> Pubkey {
    Pubkey::find_program_address(&[b"multisig-vault"], &sale::id()).0
}

/// The bytes of a program-owned `WalletAggregate` account: the discriminator plus
/// Borsh(body).
fn aggregate_account_bytes(a: &WalletAggregate) -> Vec<u8> {
    let mut bytes = WalletAggregate::DISCRIMINATOR.to_vec();
    a.serialize(&mut bytes).expect("serialize WalletAggregate");
    bytes
}

#[test]
fn init_aggregate_on_curve_passes_off_curve_rejected() {
    let mut env = TestEnv::without_sigverify();
    let user = env.actors.users[0].insecure_clone();
    let payer = env.payer.insecure_clone();
    let pda = off_curve_wallet();
    let p = env.init_default_sale();
    env.warp_to(p.sale_start_date);

    assert!(
        bytes_are_curve_point(user.pubkey()),
        "precondition: an ordinary wallet lies on the curve"
    );
    assert!(
        !bytes_are_curve_point(pda),
        "precondition: the chosen PDA lies off the curve"
    );
    // Lamports for the aggregate's rent: the rejection must come from the wallet's kind,
    // not from the payer running short of funds.
    env.svm.airdrop(&pda, ACTOR_LAMPORTS).expect("airdrop PDA");

    // The positive branch: an ordinary wallet.
    send(&mut env, init_aggregate_ix(user.pubkey()), &user)
        .expect("an ordinary wallet creates an aggregate");
    assert!(
        env.aggregate_exists(&user.pubkey()),
        "the aggregate was created"
    );
    assert_eq!(
        env.sale_state().participants,
        1,
        "the participant was counted"
    );

    // The negative branch: the same call with a PDA as the buyer.
    env.expire_blockhash();
    let r = env.send_ixs_unsigned(&[init_aggregate_ix(pda)], &payer.pubkey());
    assert_eq!(
        custom(&r),
        Some(err(SaleError::WalletNotOnCurve)),
        "a PDA as the buyer → WalletNotOnCurve: {r:?}"
    );
    assert!(
        !env.aggregate_exists(&pda),
        "the PDA's aggregate was not created"
    );
    assert_eq!(
        env.sale_state().participants,
        1,
        "participants did not grow from the rejected call"
    );
}

#[test]
fn purchase_on_curve_passes_off_curve_rejected() {
    let mut env = TestEnv::without_sigverify();
    let user = env.actors.users[0].insecure_clone();
    let payer = env.payer.insecure_clone();
    let pda = off_curve_wallet();
    let p = env.init_default_sale();
    env.warp_to(p.sale_start_date);

    assert!(
        !bytes_are_curve_point(pda),
        "precondition: the chosen PDA lies off the curve"
    );

    let amount = P3_NET;
    let ata_user = funded_ata(&mut env, &user.pubkey(), amount);
    let ata_pda = funded_ata(&mut env, &pda, amount);
    let vault = deposit_vault_pda();
    env.svm.airdrop(&pda, ACTOR_LAMPORTS).expect("airdrop PDA");

    // A PDA will never obtain an aggregate honestly - init_aggregate rejects it. We
    // place a ready one so that purchase's check is exercised on its own rather than
    // through a neighbouring instruction's rejection.
    let aggregate = Pubkey::find_program_address(&[b"aggregate", pda.as_ref()], &sale::id()).0;
    env.forge_program_account(
        &aggregate,
        aggregate_account_bytes(&WalletAggregate {
            wallet: pda,
            net_usdc_contributed: 0,
            deposit_count: 0,
        }),
    );
    assert_eq!(
        aggregate,
        aggregate_pda(&pda),
        "the aggregate at its own address"
    );

    // The positive branch: an ordinary wallet buys.
    send(&mut env, init_aggregate_ix(user.pubkey()), &user).expect("init_aggregate");
    env.expire_blockhash();
    send(
        &mut env,
        purchase_ix(user.pubkey(), ata_user, 0, amount),
        &user,
    )
    .expect("an ordinary wallet buys");
    assert_eq!(
        env.token_balance(&vault),
        amount,
        "the vault grew by the purchase"
    );

    // The negative branch: the PDA has both an aggregate and USDC - the rejection comes
    // from the wallet's kind.
    env.expire_blockhash();
    let r = env.send_ixs_unsigned(&[purchase_ix(pda, ata_pda, 0, amount)], &payer.pubkey());
    assert_eq!(
        custom(&r),
        Some(err(SaleError::WalletNotOnCurve)),
        "a PDA as the buyer → WalletNotOnCurve: {r:?}"
    );
    assert!(
        !env.deposit_exists(&pda, 0),
        "the PDA's deposit was not created"
    );
    assert_eq!(
        env.token_balance(&ata_pda),
        amount,
        "the PDA's USDC was not debited"
    );
    assert_eq!(
        env.token_balance(&vault),
        amount,
        "the vault did not change"
    );
    assert_eq!(
        env.wallet_aggregate(&pda).net_usdc_contributed,
        0,
        "the net of the PDA's aggregate stayed zero"
    );
    assert_eq!(
        env.sale_state().deposits_usdc_contributed,
        amount,
        "the overall accounting did not change"
    );
}
