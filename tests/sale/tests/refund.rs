//! The AML refund of a single deposit: `refund(index)` - Arch §9.8; TRD §5.4. The
//! Operator returns the USDC to the original wallet, closes the deposit (the rent
//! goes to wallet, a replay guard), and symmetrically decrements `aggregate.net` and
//! `deposits_usdc_contributed` (I1/I2). The gate is `!pool_fixed` (TS-21), not the
//! phase.
//!
//! tests-bind-to-real-code: real instructions through the builders; the expectations
//! are independent literals (multiples of P3_NET) and arithmetic done by hand; the
//! assertions run against observable effects (ATA and vault balances, the state
//! accounts' fields, the fact of closing) and a SPECIFIC error code.
//!
//! The invariants:
//! - I1/I2 (§3): a refund decrements net and the overall pool by exactly the
//!   deposit's amount.
//! - I5 (§3): a deposit is spent once - a repeated refund fails with 3012.
//! - Hot-key protection (§9.8): the amount and the recipient come from the deposit,
//!   not from the arguments.

use anchor_lang::error::{ErrorCode as AnchorError, ERROR_CODE_OFFSET};
use sale::errors::SaleError;
use sale_test_common::{
    activate_claim_ix, custom_error_code, deposit_vault_pda,
    expected::{DEFAULT_EMISSION, P3_NET},
    init_aggregate_ix, pause_ix, purchase_ix, refund_ix, refund_ix_with_vault, token_vault_pda,
    wallet_usdc_ata, InitParams, TestEnv,
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
/// ATA's address.
fn funded_ata(env: &mut TestEnv, wallet: &Pubkey, amount: u64) -> Pubkey {
    let usdc = env.usdc_mint;
    let ata = env.create_ata(wallet, &usdc);
    env.mint_to(&usdc, &ata, amount);
    ata
}

/// Prepares a sale in the Sale phase with an aggregate and `amounts.len()` deposits
/// of wallet `user` (funding the ATA with the total of all contributions). Returns
/// (InitParams, ATA).
fn sale_with_deposits(env: &mut TestEnv, user: &Keypair, amounts: &[u64]) -> (InitParams, Pubkey) {
    let p = env.init_default_sale();
    env.warp_to(p.sale_start_date);
    let total: u64 = amounts.iter().sum();
    let ata = funded_ata(env, &user.pubkey(), total);
    send(env, init_aggregate_ix(user.pubkey()), user).expect("init_aggregate");
    for (i, &amount) in amounts.iter().enumerate() {
        env.expire_blockhash();
        send(env, purchase_ix(user.pubkey(), ata, i as u64, amount), user)
            .unwrap_or_else(|e| panic!("purchase #{i}: {e:?}"));
    }
    (p, ata)
}

// ===================================================================
// refund - POSITIVE: the refund, the close, the symmetry of the counters (I1/I2,
// §9.8)
// ===================================================================

#[test]
fn refund_returns_usdc_closes_deposit_and_decrements_counters() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let operator = env.actors.operator.insecure_clone();
    let user = env.actors.users[0].insecure_clone();

    // Two deposits: $30 (index 0) and $20 (index 1). net = $50, the vault = $50.
    let a0 = P3_NET * 3;
    let a1 = P3_NET * 2;
    let (_p, ata) = sale_with_deposits(&mut env, &user, &[a0, a1]);
    let vault = deposit_vault_pda();

    assert_eq!(
        env.token_balance(&ata),
        0,
        "the ATA emptied out after the two purchases"
    );
    assert_eq!(
        env.token_balance(&vault),
        a0 + a1,
        "the vault == the total of the contributions"
    );
    assert_eq!(
        env.sale_state().deposits_usdc_contributed,
        a0 + a1,
        "the accounted pool == the total of the contributions"
    );

    // The refund of deposit index 0 ($30).
    env.expire_blockhash();
    send(
        &mut env,
        refund_ix(operator.pubkey(), user.pubkey(), 0, usdc),
        &operator,
    )
    .expect("refund of index 0 passes");

    // Deposit 0 is closed and deposit 1 is untouched.
    assert!(
        !env.deposit_exists(&user.pubkey(), 0),
        "deposit 0 is closed (a replay guard)"
    );
    assert!(
        env.deposit_exists(&user.pubkey(), 1),
        "deposit 1 is untouched"
    );
    assert_eq!(
        env.deposit_entry(&user.pubkey(), 1).amount,
        a1,
        "deposit 1 kept its amount"
    );

    // The USDC came back for exactly the deposit's amount (the recipient and the amount
    // come from the deposit).
    assert_eq!(
        env.token_balance(&ata),
        a0,
        "exactly deposit 0's amount came back to the ATA"
    );
    assert_eq!(
        env.token_balance(&vault),
        a1,
        "the vault decreased by the refunded amount"
    );

    // The symmetry of the counters (I1/I2): net and the pool decreased by the same
    // amount.
    let agg = env.wallet_aggregate(&user.pubkey());
    assert_eq!(
        agg.net_usdc_contributed, a1,
        "net decreased by the refunded amount"
    );
    assert_eq!(
        agg.deposit_count, 2,
        "deposit_count (the index allocator) does not roll back"
    );
    assert!(
        env.aggregate_exists(&user.pubkey()),
        "the aggregate is NOT closed (§5.2)"
    );
    assert_eq!(
        env.sale_state().deposits_usdc_contributed,
        a1,
        "the overall pool decreased by the refunded amount (I2)"
    );
}

#[test]
fn refund_full_zeroes_net_but_keeps_aggregate() {
    // A full refund of the only deposit zeroes net, but the aggregate remains (§5.2 /
    // TS-22): it is closed by claim or by a batch, not by refund.
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let operator = env.actors.operator.insecure_clone();
    let user = env.actors.users[0].insecure_clone();
    let amount = P3_NET * 4;
    let (_p, _ata) = sale_with_deposits(&mut env, &user, &[amount]);

    env.expire_blockhash();
    send(
        &mut env,
        refund_ix(operator.pubkey(), user.pubkey(), 0, usdc),
        &operator,
    )
    .expect("refund passes");

    assert!(
        !env.deposit_exists(&user.pubkey(), 0),
        "the deposit is closed"
    );
    assert!(
        env.aggregate_exists(&user.pubkey()),
        "the aggregate is alive at net == 0"
    );
    assert_eq!(
        env.wallet_aggregate(&user.pubkey()).net_usdc_contributed,
        0,
        "net was zeroed"
    );
    assert_eq!(
        env.sale_state().deposits_usdc_contributed,
        0,
        "the pool was zeroed"
    );
}

// ===================================================================
// refund - a pause does not block it (TS-10): refund does not check paused
// ===================================================================

#[test]
fn refund_unaffected_by_pause() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let admin = env.actors.admin.insecure_clone();
    let operator = env.actors.operator.insecure_clone();
    let user = env.actors.users[0].insecure_clone();
    let amount = P3_NET * 3;
    let (_p, ata) = sale_with_deposits(&mut env, &user, &[amount]);

    env.expire_blockhash();
    send(&mut env, pause_ix(admin.pubkey()), &admin).expect("pause in Sale");
    assert!(env.sale_state().paused, "precondition: the pause is active");

    env.expire_blockhash();
    send(
        &mut env,
        refund_ix(operator.pubkey(), user.pubkey(), 0, usdc),
        &operator,
    )
    .expect("refund passes while paused");

    assert!(
        !env.deposit_exists(&user.pubkey(), 0),
        "the deposit is closed"
    );
    assert_eq!(
        env.token_balance(&ata),
        amount,
        "the USDC was returned while paused"
    );
    assert_eq!(
        env.sale_state().deposits_usdc_contributed,
        0,
        "the pool decreased"
    );
    assert!(
        env.sale_state().paused,
        "the pause was not lifted by the refund"
    );
}

// ===================================================================
// refund - the AML window: valid in SaleEnded before fixation (the !pool_fixed gate)
// ===================================================================

#[test]
fn refund_works_in_sale_ended_before_activation() {
    // The AML window (§5.5): the SaleEnded phase with the pool not yet fixed - refund
    // works. refund's gate is `!pool_fixed`, not the phase.
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let operator = env.actors.operator.insecure_clone();
    let user = env.actors.users[0].insecure_clone();
    let amount = P3_NET * 5;
    let (p, ata) = sale_with_deposits(&mut env, &user, &[amount]);

    env.warp_to(p.sale_end_date); // SaleEnded, pool_fixed == false
    assert!(
        !env.sale_state().pool_fixed,
        "precondition: the pool is not fixed"
    );

    env.expire_blockhash();
    send(
        &mut env,
        refund_ix(operator.pubkey(), user.pubkey(), 0, usdc),
        &operator,
    )
    .expect("refund in SaleEnded before fixation passes");

    assert!(
        !env.deposit_exists(&user.pubkey(), 0),
        "the deposit is closed"
    );
    assert_eq!(env.token_balance(&ata), amount, "the USDC was returned");
    assert_eq!(
        env.sale_state().deposits_usdc_contributed,
        0,
        "the pool decreased"
    );
}

#[test]
fn refund_after_pool_fixed_rejected() {
    // The mirror of the AML window: after the pool is fixed refund reverts with
    // `PoolAlreadyFixed` and the deposit remains.
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let admin = env.actors.admin.insecure_clone();
    let operator = env.actors.operator.insecure_clone();
    let user = env.actors.users[0].insecure_clone();
    let amount = P3_NET * 5;
    let (p, _ata) = sale_with_deposits(&mut env, &user, &[amount]);

    env.warp_to(p.sale_end_date);
    // Undersubscribed under the default economics (a pool of $50 << the soft cap of
    // $3.9M). We fund token_vault generously (I12 is a `≥`): DEFAULT_EMISSION covers any
    // undersubscribed distributable, so we do not compute it exactly.
    let win = env.win_mint;
    env.mint_to(&win, &token_vault_pda(), DEFAULT_EMISSION);
    env.expire_blockhash();
    send(
        &mut env,
        activate_claim_ix(admin.pubkey(), usdc, None),
        &admin,
    )
    .expect("activate_claim (undersubscribed) passes");
    assert!(
        env.sale_state().pool_fixed,
        "precondition: the pool is fixed"
    );

    env.expire_blockhash();
    let r = send(
        &mut env,
        refund_ix(operator.pubkey(), user.pubkey(), 0, usdc),
        &operator,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::PoolAlreadyFixed)),
        "refund after fixation → PoolAlreadyFixed: {r:?}"
    );
    assert!(
        env.deposit_exists(&user.pubkey(), 0),
        "the deposit was not closed"
    );
}

// ===================================================================
// refund - the phase guard (L4): PreSale is rejected with WrongPhase
// ===================================================================

#[test]
fn refund_in_presale_rejected_wrong_phase() {
    // The mirror of the phase table of TRD §5.2: refund is permitted only in
    // Sale/SaleEnded. The order of refund's gates: `!pool_fixed` first, then
    // `require!(phase ∈ {Sale, SaleEnded}, WrongPhase)`. The new guard rejects exactly
    // the PreSale edge.
    //
    // A defensive (mirror) guard: in production time only moves forward, so a live
    // deposit implies `now ≥ sale_start_date` - the state "a deposit exists but the
    // phase is PreSale" is unreachable by natural means. Here it is produced
    // ARTIFICIALLY, by rolling the Clock back through `svm.set_sysvar` (bypassing the
    // forward-only `warp_to`). The test documents that the guard holds.
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let operator = env.actors.operator.insecure_clone();
    let user = env.actors.users[0].insecure_clone();
    let amount = P3_NET * 3;

    // The deposit was created normally in Sale.
    let (p, _ata) = sale_with_deposits(&mut env, &user, &[amount]);
    assert!(
        env.deposit_exists(&user.pubkey(), 0),
        "the deposit was created in Sale"
    );

    // An artificial Clock rollback below sale_start_date → the PreSale phase, with the
    // deposit alive.
    let mut clock = env.clock();
    clock.unix_timestamp = p.sale_start_date - 1;
    env.svm.set_sysvar(&clock);

    env.expire_blockhash();
    let r = send(
        &mut env,
        refund_ix(operator.pubkey(), user.pubkey(), 0, usdc),
        &operator,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::WrongPhase)),
        "refund in PreSale → WrongPhase: {r:?}"
    );
    assert!(
        env.deposit_exists(&user.pubkey(), 0),
        "the deposit was not closed (revert)"
    );
}

// ===================================================================
// refund - replay (I5): a repeated refund of a closed deposit → 3012
// ===================================================================

#[test]
fn refund_replay_fails_account_not_initialized() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let operator = env.actors.operator.insecure_clone();
    let user = env.actors.users[0].insecure_clone();
    let amount = P3_NET * 3;
    let (_p, ata) = sale_with_deposits(&mut env, &user, &[amount]);

    env.expire_blockhash();
    send(
        &mut env,
        refund_ix(operator.pubkey(), user.pubkey(), 0, usdc),
        &operator,
    )
    .expect("the first refund passes");
    assert_eq!(
        env.token_balance(&ata),
        amount,
        "after the first refund the ATA holds amount"
    );

    // The repeat WITH A FRESH blockhash (otherwise AlreadyProcessed before execution).
    env.expire_blockhash();
    let r = send(
        &mut env,
        refund_ix(operator.pubkey(), user.pubkey(), 0, usdc),
        &operator,
    );
    assert_eq!(
        custom(&r),
        Some(anchor_err(AnchorError::AccountNotInitialized)),
        "a repeated refund of the closed deposit → AccountNotInitialized (3012): {r:?}"
    );
    assert_eq!(
        env.token_balance(&ata),
        amount,
        "the repeat did not hand out the USDC a second time"
    );
    assert_eq!(
        env.sale_state().deposits_usdc_contributed,
        0,
        "the pool did not go negative"
    );
}

// ===================================================================
// refund - the role (§8): the Operator only
// ===================================================================

#[test]
fn refund_by_non_operator_rejected() {
    // A signature by a non-operator (admin in the operator slot) → has_one operator →
    // Unauthorized.
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let admin = env.actors.admin.insecure_clone();
    let user = env.actors.users[0].insecure_clone();
    let amount = P3_NET * 3;
    let (_p, _ata) = sale_with_deposits(&mut env, &user, &[amount]);

    env.expire_blockhash();
    let r = send(
        &mut env,
        refund_ix(admin.pubkey(), user.pubkey(), 0, usdc),
        &admin,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::Unauthorized)),
        "a refund by a non-operator → Unauthorized: {r:?}"
    );
    assert!(
        env.deposit_exists(&user.pubkey(), 0),
        "the deposit is untouched"
    );
}

// ===================================================================
// refund - vault substitution (accounts-untrusted): a foreign deposit_vault
// ===================================================================

#[test]
fn refund_foreign_deposit_vault_fails_constraint_address() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let operator = env.actors.operator.insecure_clone();
    let user = env.actors.users[0].insecure_clone();
    let attacker = env.actors.users[1].insecure_clone();
    let amount = P3_NET * 3;
    let (_p, _ata) = sale_with_deposits(&mut env, &user, &[amount]);
    // A foreign USDC account of the same mint - a candidate for substituting the vault.
    let fake_vault = funded_ata(&mut env, &attacker.pubkey(), amount);

    env.expire_blockhash();
    let r = send(
        &mut env,
        refund_ix_with_vault(operator.pubkey(), user.pubkey(), 0, usdc, fake_vault),
        &operator,
    );
    assert_eq!(
        custom(&r),
        Some(anchor_err(AnchorError::ConstraintAddress)),
        "substituting deposit_vault → ConstraintAddress (2012): {r:?}"
    );
    assert!(
        env.deposit_exists(&user.pubkey(), 0),
        "the deposit was not closed"
    );
    assert_eq!(
        env.token_balance(&fake_vault),
        amount,
        "the foreign vault is untouched"
    );
}

// ===================================================================
// refund - recreating the recipient's closed ATA (init_if_needed, §9.8)
// ===================================================================

#[test]
fn refund_recreates_closed_recipient_ata() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let operator = env.actors.operator.insecure_clone();
    let user = env.actors.users[0].insecure_clone();
    let amount = P3_NET * 3;
    let (_p, ata) = sale_with_deposits(&mut env, &user, &[amount]);

    // The user closed their USDC account (its balance was 0 after the purchase).
    assert_eq!(
        env.token_balance(&ata),
        0,
        "the ATA emptied out after the purchase"
    );
    let close_ix = spl_token_interface::instruction::close_account(
        &spl_token_interface::id(),
        &ata,
        &user.pubkey(),
        &user.pubkey(),
        &[],
    )
    .expect("close_account ix");
    env.expire_blockhash();
    send(&mut env, close_ix, &user).expect("the user closed the ATA");
    assert!(!env.account_exists(&ata), "the ATA is closed");

    // refund recreates the ATA and deposits the refund into it (the payer is the
    // operator).
    env.expire_blockhash();
    send(
        &mut env,
        refund_ix(operator.pubkey(), user.pubkey(), 0, usdc),
        &operator,
    )
    .expect("refund recreates the ATA and returns the USDC");

    assert_eq!(
        wallet_usdc_ata(&user.pubkey(), &usdc),
        ata,
        "the ATA's address is canonical"
    );
    assert!(env.account_exists(&ata), "the ATA was recreated");
    assert_eq!(
        env.token_balance(&ata),
        amount,
        "the refund landed in the recreated ATA"
    );
}
