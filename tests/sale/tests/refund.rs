//! The AML refund of a single deposit: `refund(index)` - Arch §9.8; TRD §5.4. The
//! Operator returns the USDC into a fresh account the wallet owns - the refund
//! account, seeds `[b"refund_account", wallet, index]` - closes the deposit into
//! that same account (its rent follows the USDC; a replay guard), and symmetrically
//! decrements `aggregate.net` and `deposits_usdc_contributed` (I1/I2). The gate is
//! `!pool_fixed` (TS-21), not the phase. The wallet is an address only: nothing of
//! it is read or written.
//!
//! tests-bind-to-real-code: real instructions through the builders; the expectations
//! are independent literals (multiples of P3_NET) and arithmetic done by hand; the
//! assertions run against observable effects (refund-account and vault balances,
//! lamports, the state accounts' fields, the fact of closing, the event payload) and
//! a SPECIFIC error code.
//!
//! The invariants:
//! - I1/I2 (§3): a refund decrements net and the overall pool by exactly the
//!   deposit's amount.
//! - I5 (§3): a deposit is spent once - a repeated refund fails with 3012.
//! - Hot-key protection (§9.8): the amount and the recipient come from the deposit,
//!   not from the arguments; any account but the deposit's own refund PDA is
//!   rejected, and the operator holds no right over the account it paid for.
//! - The destination cannot be sabotaged (§9.8): no state of the wallet's existing
//!   accounts, and none of the wallet account itself, can block a forced refund.

use anchor_lang::error::{ErrorCode as AnchorError, ERROR_CODE_OFFSET};
use sale::errors::SaleError;
use sale::events::Refunded;
use sale_test_common::{
    activate_claim_ix, custom_error_code, decode_event, deposit_pda, deposit_vault_pda,
    expected::{DEFAULT_EMISSION, P3_NET},
    init_aggregate_ix, pause_ix, purchase_ix, refund_account_pda, refund_ix, refund_ix_with_slots,
    refund_ix_with_vault, token_vault_pda, InitParams, TestEnv,
};
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_program_pack::Pack;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use spl_token_interface::instruction::AuthorityType;
use spl_token_interface::state::Account as SplAccount;

/// SPL Token's `OwnerMismatch`: the signer is not the account's authority.
const SPL_OWNER_MISMATCH: u32 = 4;

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
fn send(env: &mut TestEnv, ix: Instruction, signer: &Keypair) -> litesvm::types::TransactionResult {
    env.send_ixs(&[ix], &signer.pubkey(), &[signer])
}

/// The rent-exempt balance of an SPL token account (165 B) - what the operator pays
/// for the refund account.
fn token_account_rent(env: &TestEnv) -> u64 {
    env.svm.minimum_balance_for_rent_exemption(SplAccount::LEN)
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

/// An aggregate and a single deposit of `amount` for `user` in a sale that is
/// already running (the ATA is funded with exactly `amount`). Returns the ATA.
fn add_user_deposit(env: &mut TestEnv, user: &Keypair, amount: u64) -> Pubkey {
    let ata = funded_ata(env, &user.pubkey(), amount);
    env.expire_blockhash();
    send(env, init_aggregate_ix(user.pubkey()), user).expect("init_aggregate");
    env.expire_blockhash();
    send(env, purchase_ix(user.pubkey(), ata, 0, amount), user).expect("purchase");
    ata
}

/// `SetAuthority(AccountOwner, new_owner)` on `token_account`, signed by its current
/// owner - allowed on any legacy SPL token account, the ATA included.
fn rotate_owner(env: &mut TestEnv, token_account: &Pubkey, current: &Keypair, new_owner: &Pubkey) {
    let ix = spl_token_interface::instruction::set_authority(
        &spl_token_interface::id(),
        token_account,
        Some(new_owner),
        AuthorityType::AccountOwner,
        &current.pubkey(),
        &[],
    )
    .expect("set_authority ix");
    env.expire_blockhash();
    send(env, ix, current).expect("SetAuthority(AccountOwner)");
}

/// The user closes their (empty) ATA: the account no longer exists.
fn close_ata(env: &mut TestEnv, ata: &Pubkey, user: &Keypair) {
    env.expire_blockhash();
    send(env, spl_close_ix(ata, &user.pubkey(), &user.pubkey()), user)
        .expect("the user closed the ATA");
    assert!(!env.account_exists(ata), "the ATA is closed");
}

/// An SPL `Transfer` of `amount` out of `from` into `to` under `authority`.
fn spl_transfer_ix(from: &Pubkey, to: &Pubkey, authority: &Pubkey, amount: u64) -> Instruction {
    spl_token_interface::instruction::transfer(
        &spl_token_interface::id(),
        from,
        to,
        authority,
        &[],
        amount,
    )
    .expect("transfer ix")
}

/// An SPL `CloseAccount` of `account` under `authority`, the lamports going to
/// `destination`.
fn spl_close_ix(account: &Pubkey, destination: &Pubkey, authority: &Pubkey) -> Instruction {
    spl_token_interface::instruction::close_account(
        &spl_token_interface::id(),
        account,
        destination,
        authority,
        &[],
    )
    .expect("close_account ix")
}

/// What every successful refund of deposit `index` leaves behind: the refund account
/// exists, is the wallet's USDC account, holds exactly `amount`, and the deposit is
/// closed. Returns the refund account's address.
fn assert_refunded(env: &TestEnv, wallet: &Pubkey, index: u64, amount: u64) -> Pubkey {
    let refund = refund_account_pda(wallet, index);
    assert!(env.account_exists(&refund), "the refund account exists");
    assert_eq!(
        env.token_account_mint_owner(&refund),
        (env.usdc_mint, *wallet),
        "the refund account is the wallet's USDC account"
    );
    assert_eq!(
        env.token_balance(&refund),
        amount,
        "the refund account holds exactly the deposit's amount"
    );
    assert!(
        !env.deposit_exists(wallet, index),
        "the deposit is closed (a replay guard)"
    );
    refund
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
    let refund = refund_account_pda(&user.pubkey(), 0);

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
    assert!(
        !env.account_exists(&refund),
        "the refund account does not exist before the refund"
    );
    let rent = token_account_rent(&env);
    let deposit_rent = env.lamports_of(&deposit_pda(&user.pubkey(), 0));
    let wallet_before = env.lamports_of(&user.pubkey());
    let operator_before = env.lamports_of(&operator.pubkey());

    // The refund of deposit index 0 ($30).
    env.expire_blockhash();
    let r = send(
        &mut env,
        refund_ix(operator.pubkey(), user.pubkey(), 0, usdc),
        &operator,
    );
    assert!(r.is_ok(), "refund of index 0 passes: {r:?}");

    // Deposit 0 is closed and deposit 1 is untouched.
    assert_refunded(&env, &user.pubkey(), 0, a0);
    assert!(
        env.deposit_exists(&user.pubkey(), 1),
        "deposit 1 is untouched"
    );
    assert_eq!(
        env.deposit_entry(&user.pubkey(), 1).amount,
        a1,
        "deposit 1 kept its amount"
    );

    // The USDC left the vault for exactly the deposit's amount - into the refund
    // account, not the ATA (the recipient and the amount come from the deposit).
    assert_eq!(env.token_balance(&ata), 0, "the ATA is not involved");
    assert_eq!(
        env.token_balance(&vault),
        a1,
        "the vault decreased by the refunded amount"
    );

    // The lamports: the operator paid the refund account's rent, the deposit's rent
    // followed the USDC into it, and the wallet account itself was not touched.
    assert_eq!(
        env.lamports_of(&refund),
        rent + deposit_rent,
        "the refund account holds its own rent plus the closed deposit's"
    );
    assert_eq!(
        env.lamports_of(&user.pubkey()),
        wallet_before,
        "nothing was credited to the wallet account"
    );
    assert!(
        operator_before - env.lamports_of(&operator.pubkey()) >= rent,
        "the operator paid the refund account's rent"
    );

    // The event - the only on-chain pointer to (wallet, index), from which the refund
    // account's address is derived.
    let ev: Refunded = decode_event(&r).expect("the Refunded event");
    assert_eq!(
        (ev.wallet, ev.index, ev.amount, ev.net, ev.total),
        (user.pubkey(), 0, a0, a1, a1),
        "the event carries the wallet, the index, the amount and both counters"
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

    // The wallet alone disposes of the account: it moves the USDC to its ATA and
    // closes the account, sending both rents wherever it likes.
    let rent_collector = Keypair::new().pubkey();
    env.expire_blockhash();
    send(
        &mut env,
        spl_transfer_ix(&refund, &ata, &user.pubkey(), a0),
        &user,
    )
    .expect("the wallet moves the USDC out of the refund account");
    env.expire_blockhash();
    send(
        &mut env,
        spl_close_ix(&refund, &rent_collector, &user.pubkey()),
        &user,
    )
    .expect("the wallet closes the refund account");
    assert_eq!(
        env.token_balance(&ata),
        a0,
        "the USDC reached the ATA under the wallet's own signature"
    );
    assert!(!env.account_exists(&refund), "the refund account is closed");
    assert_eq!(
        env.lamports_of(&rent_collector),
        rent + deposit_rent,
        "both rents went where the wallet sent them"
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

    assert_refunded(&env, &user.pubkey(), 0, amount);
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
    let (_p, _ata) = sale_with_deposits(&mut env, &user, &[amount]);

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

    assert_refunded(&env, &user.pubkey(), 0, amount);
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
    let (p, _ata) = sale_with_deposits(&mut env, &user, &[amount]);

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

    assert_refunded(&env, &user.pubkey(), 0, amount);
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
    assert!(
        !env.account_exists(&refund_account_pda(&user.pubkey(), 0)),
        "no refund account was left behind (the transaction reverted)"
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
    assert!(
        !env.account_exists(&refund_account_pda(&user.pubkey(), 0)),
        "no refund account was left behind (the transaction reverted)"
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
    let refund = assert_refunded(&env, &user.pubkey(), 0, amount);

    // The wallet moves the USDC to its ATA and closes the refund account: the slot at
    // its address is free again, and still the closed deposit alone decides.
    env.expire_blockhash();
    send(
        &mut env,
        spl_transfer_ix(&refund, &ata, &user.pubkey(), amount),
        &user,
    )
    .expect("the wallet moves the USDC out");
    env.expire_blockhash();
    send(
        &mut env,
        spl_close_ix(&refund, &user.pubkey(), &user.pubkey()),
        &user,
    )
    .expect("the wallet closes the refund account");
    assert!(!env.account_exists(&refund), "the refund account is closed");

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
    assert!(
        !env.account_exists(&refund),
        "the repeat created no refund account"
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
    // Unauthorized. The admin is funded: the refund account's creation runs before
    // the role check and is rolled back with the transaction.
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
    assert!(
        !env.account_exists(&refund_account_pda(&user.pubkey(), 0)),
        "no refund account was left behind (the transaction reverted)"
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
// refund - the mint (accounts-untrusted): the refund account is created for the
// mint passed in, and `address = sale_state.usdc_mint` pins that mint to USDC
// ===================================================================

#[test]
fn refund_with_foreign_usdc_mint_fails_constraint_address() {
    let mut env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();
    let user = env.actors.users[0].insecure_clone();
    let amount = P3_NET * 3;
    let (_p, _ata) = sale_with_deposits(&mut env, &user, &[amount]);
    let foreign_mint = env.create_mint_for_test(6);

    env.expire_blockhash();
    let r = send(
        &mut env,
        refund_ix(operator.pubkey(), user.pubkey(), 0, foreign_mint),
        &operator,
    );
    assert_eq!(
        custom(&r),
        Some(anchor_err(AnchorError::ConstraintAddress)),
        "a foreign mint in the usdc_mint slot → ConstraintAddress (2012): {r:?}"
    );
    assert!(
        env.deposit_exists(&user.pubkey(), 0),
        "the deposit was not closed"
    );
    assert!(
        !env.account_exists(&refund_account_pda(&user.pubkey(), 0)),
        "no refund account of the foreign mint was left behind (the transaction reverted)"
    );
}

// ===================================================================
// refund - the destination cannot be sabotaged (§9.8): the refund account is born
// inside the refund, so no state of the wallet's existing accounts - nor of the
// wallet account itself - is read or written
// ===================================================================

#[test]
fn refund_succeeds_after_wallet_rotates_ata_authority() {
    // The ATA's SPL owner is a field the wallet rewrites with its own signature: the
    // account keeps its canonical address while no longer belonging to the wallet. A
    // refund bound to the ATA would be rejected on that field; this one does not look
    // at the ATA at all.
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let operator = env.actors.operator.insecure_clone();
    let user = env.actors.users[0].insecure_clone();
    let amount = P3_NET * 3;
    let (_p, ata) = sale_with_deposits(&mut env, &user, &[amount]);

    let alt = Keypair::new();
    rotate_owner(&mut env, &ata, &user, &alt.pubkey());
    assert_eq!(
        env.token_account_mint_owner(&ata).1,
        alt.pubkey(),
        "precondition: the ATA's SPL owner is no longer the wallet"
    );

    env.expire_blockhash();
    send(
        &mut env,
        refund_ix(operator.pubkey(), user.pubkey(), 0, usdc),
        &operator,
    )
    .expect("refund passes with the ATA's authority rotated");

    assert_refunded(&env, &user.pubkey(), 0, amount);
    assert_eq!(
        env.token_account_mint_owner(&ata).1,
        alt.pubkey(),
        "the ATA is untouched"
    );
    assert_eq!(env.token_balance(&ata), 0, "the ATA received nothing");
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

#[test]
fn refund_succeeds_without_any_usdc_account() {
    // The user closed their ATA (its balance was 0 after the purchase): the refund
    // needs no account of the wallet's to exist and recreates nothing.
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let operator = env.actors.operator.insecure_clone();
    let user = env.actors.users[0].insecure_clone();
    let amount = P3_NET * 3;
    let (_p, ata) = sale_with_deposits(&mut env, &user, &[amount]);

    assert_eq!(
        env.token_balance(&ata),
        0,
        "the ATA emptied out after the purchase"
    );
    close_ata(&mut env, &ata, &user);

    env.expire_blockhash();
    send(
        &mut env,
        refund_ix(operator.pubkey(), user.pubkey(), 0, usdc),
        &operator,
    )
    .expect("refund passes with no USDC account of the wallet's");

    assert_refunded(&env, &user.pubkey(), 0, amount);
    assert!(
        !env.account_exists(&ata),
        "the ATA stays closed - the refund does not recreate it"
    );
}

#[test]
fn refund_succeeds_when_wallet_ata_is_frozen() {
    // An administered stablecoin's issuer can freeze any existing token account of
    // the mint; a transfer into a frozen ATA is refused. The refund account does not
    // exist until the refund itself, so there is nothing to freeze.
    let mut env = TestEnv::with_usdc_freeze_authority();
    let usdc = env.usdc_mint;
    let operator = env.actors.operator.insecure_clone();
    let user = env.actors.users[0].insecure_clone();
    let amount = P3_NET * 3;
    let (_p, ata) = sale_with_deposits(&mut env, &user, &[amount]);

    env.freeze_account(&ata, &usdc);
    assert!(
        env.token_account_frozen(&ata),
        "precondition: the wallet's ATA is frozen"
    );

    env.expire_blockhash();
    send(
        &mut env,
        refund_ix(operator.pubkey(), user.pubkey(), 0, usdc),
        &operator,
    )
    .expect("refund passes with the wallet's ATA frozen");

    let refund = assert_refunded(&env, &user.pubkey(), 0, amount);
    assert!(
        !env.token_account_frozen(&refund),
        "the refund account is not frozen"
    );
    assert!(
        env.token_account_frozen(&ata),
        "the ATA stays frozen and untouched"
    );
    assert_eq!(env.token_balance(&ata), 0, "the ATA received nothing");
}

#[test]
fn refund_succeeds_when_wallet_is_executable() {
    // A wallet that deployed a program at its own address is an executable account,
    // and the runtime refuses to change its lamports. The refund never credits the
    // wallet: the deposit's rent goes into the refund account.
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let operator = env.actors.operator.insecure_clone();
    let user = env.actors.users[0].insecure_clone();
    let amount = P3_NET * 3;
    let (_p, ata) = sale_with_deposits(&mut env, &user, &[amount]);
    let rent = token_account_rent(&env);
    let deposit_rent = env.lamports_of(&deposit_pda(&user.pubkey(), 0));

    env.make_executable(&user.pubkey());
    let wallet_before = env.lamports_of(&user.pubkey());

    env.expire_blockhash();
    send(
        &mut env,
        refund_ix(operator.pubkey(), user.pubkey(), 0, usdc),
        &operator,
    )
    .expect("refund passes for an executable wallet");

    let refund = assert_refunded(&env, &user.pubkey(), 0, amount);
    assert_eq!(
        env.lamports_of(&refund),
        rent + deposit_rent,
        "the deposit's rent went into the refund account"
    );
    assert_eq!(
        env.lamports_of(&user.pubkey()),
        wallet_before,
        "the wallet account was not credited"
    );
    assert_eq!(env.token_balance(&ata), 0, "the ATA received nothing");
}

#[test]
fn refund_succeeds_with_prefunded_refund_slot() {
    // The only thing anyone can do to the refund account's address before the refund
    // is send lamports to it (the address is computable). Both creation branches are
    // exercised: a balance below the token account's rent is topped up by the
    // operator; a balance at or above it costs the operator nothing beyond the fee.
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let operator = env.actors.operator.insecure_clone();
    let amount = P3_NET * 3;
    let rent = token_account_rent(&env);
    // The rent-exempt balance of a data-less account: the least a transfer may leave.
    let rent_of_empty = env.svm.minimum_balance_for_rent_exemption(0);
    let p = env.init_default_sale();
    env.warp_to(p.sale_start_date);

    let cases = [
        (env.actors.users[0].insecure_clone(), rent_of_empty),
        (env.actors.users[1].insecure_clone(), rent + 1),
    ];
    for (user, prefund) in cases {
        add_user_deposit(&mut env, &user, amount);
        let refund = refund_account_pda(&user.pubkey(), 0);
        let deposit_rent = env.lamports_of(&deposit_pda(&user.pubkey(), 0));

        env.expire_blockhash();
        send(
            &mut env,
            solana_system_interface::instruction::transfer(&user.pubkey(), &refund, prefund),
            &user,
        )
        .expect("the wallet pre-funds the refund account's address");
        assert_eq!(
            env.lamports_of(&refund),
            prefund,
            "precondition: the address holds lamports and nothing else"
        );

        let operator_before = env.lamports_of(&operator.pubkey());
        env.expire_blockhash();
        send(
            &mut env,
            refund_ix(operator.pubkey(), user.pubkey(), 0, usdc),
            &operator,
        )
        .unwrap_or_else(|e| panic!("refund with {prefund} lamports pre-funded passes: {e:?}"));

        assert_refunded(&env, &user.pubkey(), 0, amount);
        assert_eq!(
            env.lamports_of(&refund),
            prefund.max(rent) + deposit_rent,
            "the pre-funded lamports stay in the account"
        );
        let paid = operator_before - env.lamports_of(&operator.pubkey());
        if prefund < rent {
            assert!(
                paid >= rent - prefund && paid < rent,
                "the operator topped the balance up to the rent: paid {paid}"
            );
        } else {
            assert!(
                paid < rent_of_empty,
                "the operator paid the fee only: paid {paid}"
            );
        }
    }
}

#[test]
fn refund_rejects_any_account_but_the_refund_pda() {
    // The operator does not choose the destination: the slot must be the deposit's
    // own refund PDA - not the wallet's ATA, not another index's, not another
    // wallet's. The canonical slot passes.
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let operator = env.actors.operator.insecure_clone();
    let user = env.actors.users[0].insecure_clone();
    let other = env.actors.users[1].insecure_clone();
    let amount = P3_NET * 3;
    let (_p, ata) = sale_with_deposits(&mut env, &user, &[amount]);
    add_user_deposit(&mut env, &other, amount);
    let vault_before = env.token_balance(&deposit_vault_pda());

    let candidates = [
        ("the wallet's own ATA", ata),
        (
            "another index's refund PDA",
            refund_account_pda(&user.pubkey(), 1),
        ),
        (
            "another wallet's refund PDA",
            refund_account_pda(&other.pubkey(), 0),
        ),
    ];
    for (what, slot) in candidates {
        env.expire_blockhash();
        let r = send(
            &mut env,
            refund_ix_with_slots(
                operator.pubkey(),
                user.pubkey(),
                0,
                usdc,
                deposit_vault_pda(),
                slot,
            ),
            &operator,
        );
        assert_eq!(
            custom(&r),
            Some(anchor_err(AnchorError::ConstraintSeeds)),
            "{what} in the refund slot → ConstraintSeeds (2006): {r:?}"
        );
        assert!(
            env.deposit_exists(&user.pubkey(), 0),
            "the deposit was not closed ({what})"
        );
    }
    assert_eq!(
        env.token_balance(&deposit_vault_pda()),
        vault_before,
        "the vault is untouched"
    );
    assert_eq!(env.token_balance(&ata), 0, "the ATA received nothing");
    assert!(
        !env.account_exists(&refund_account_pda(&user.pubkey(), 1)),
        "no refund account was created at another index"
    );
    assert!(
        !env.account_exists(&refund_account_pda(&other.pubkey(), 0)),
        "no refund account was created for another wallet"
    );

    env.expire_blockhash();
    send(
        &mut env,
        refund_ix(operator.pubkey(), user.pubkey(), 0, usdc),
        &operator,
    )
    .expect("the canonical refund account passes");
    assert_refunded(&env, &user.pubkey(), 0, amount);
}

#[test]
fn refund_account_is_not_the_operators() {
    // The operator pays the refund account's rent but holds no right over it: only
    // the wallet's signature moves the USDC or closes the account.
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let operator = env.actors.operator.insecure_clone();
    let user = env.actors.users[0].insecure_clone();
    let stranger = env.actors.users[1].insecure_clone();
    let amount = P3_NET * 3;
    let (_p, ata) = sale_with_deposits(&mut env, &user, &[amount]);

    env.expire_blockhash();
    send(
        &mut env,
        refund_ix(operator.pubkey(), user.pubkey(), 0, usdc),
        &operator,
    )
    .expect("refund passes");
    let refund = assert_refunded(&env, &user.pubkey(), 0, amount);

    let intruders = [("the operator", operator), ("a stranger", stranger)];
    for (who, key) in &intruders {
        env.expire_blockhash();
        let r = send(
            &mut env,
            spl_transfer_ix(&refund, &ata, &key.pubkey(), amount),
            key,
        );
        assert_eq!(
            custom(&r),
            Some(SPL_OWNER_MISMATCH),
            "{who} cannot move the USDC out of the refund account: {r:?}"
        );
        assert_eq!(
            env.token_balance(&refund),
            amount,
            "the refund account is intact ({who})"
        );
    }

    // The wallet empties the account; an empty one is closable - by the wallet only.
    env.expire_blockhash();
    send(
        &mut env,
        spl_transfer_ix(&refund, &ata, &user.pubkey(), amount),
        &user,
    )
    .expect("the wallet moves the USDC out of the refund account");
    assert_eq!(
        env.token_balance(&ata),
        amount,
        "the USDC reached the ATA under the wallet's own signature"
    );
    for (who, key) in &intruders {
        env.expire_blockhash();
        let r = send(
            &mut env,
            spl_close_ix(&refund, &key.pubkey(), &key.pubkey()),
            key,
        );
        assert_eq!(
            custom(&r),
            Some(SPL_OWNER_MISMATCH),
            "{who} cannot close the refund account: {r:?}"
        );
        assert!(
            env.account_exists(&refund),
            "the refund account is intact ({who})"
        );
    }
    env.expire_blockhash();
    send(
        &mut env,
        spl_close_ix(&refund, &user.pubkey(), &user.pubkey()),
        &user,
    )
    .expect("the wallet closes the refund account");
    assert!(!env.account_exists(&refund), "the refund account is closed");
}
