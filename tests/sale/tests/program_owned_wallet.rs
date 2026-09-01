//! A wallet moved under a foreign program stays manageable: it can still be banned,
//! given limits, and refunded.
//!
//! What is verified. The owner of any empty account is changed by the system
//! instruction `assign`: the account's own signature is enough. Previously
//! `blacklist_*`, `graylist_*` and `refund` required the wallet to belong to the
//! System Program, so a participant could buy with an ordinary wallet, `assign` it to
//! their own program and become unreachable for bans, limits and refunds while
//! continuing to buy and claim. Now the wallet in the lists is an instruction
//! argument, and in `refund` it is an account with no owner check.
//!
//! Here the wallet is moved under a program by a genuine `assign`, the same way a
//! participant could: the wallet itself signs and a separate address pays the fee.
//! The new owner is an arbitrary address: the System Program does not check that the
//! receiving program exists.

use anchor_lang::error::{ErrorCode as AnchorError, ERROR_CODE_OFFSET};
use sale::errors::SaleError;
use sale_test_common::{
    blacklist_add_ix, blacklist_add_ix_with_slot, blacklist_pda, blacklist_remove_ix,
    custom_error_code, deposit_vault_pda, graylist_create_ix, graylist_remove_ix,
    graylist_update_ix, init_aggregate_ix, purchase_ix, refund_ix, wallet_usdc_ata, TestEnv,
};
use solana_instruction_error::InstructionError;
use solana_keypair::Keypair;
use solana_program_pack::Pack;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction_error::TransactionError;

fn err(variant: SaleError) -> u32 {
    ERROR_CODE_OFFSET + variant as u32
}

fn anchor_err(code: AnchorError) -> u32 {
    code as u32
}

#[allow(clippy::result_large_err)]
fn send(
    env: &mut TestEnv,
    ix: solana_instruction::Instruction,
    signer: &Keypair,
) -> litesvm::types::TransactionResult {
    env.send_ixs(&[ix], &signer.pubkey(), &[signer])
}

/// A send in which the environment's `payer` pays the fee while `signer` only signs.
/// It is needed for a wallet under a foreign program: only a System-owned address can
/// pay a fee, so such a wallet cannot be the fee payer itself.
#[allow(clippy::result_large_err)]
fn send_paid_by_env(
    env: &mut TestEnv,
    ix: solana_instruction::Instruction,
    signer: &Keypair,
) -> litesvm::types::TransactionResult {
    let payer = env.payer.insecure_clone();
    env.send_ixs(&[ix], &payer.pubkey(), &[&payer, signer])
}

/// The instruction error of a failed transaction - for runtime rejections that carry
/// no `Custom` code (which `custom_error_code` does not see).
fn instruction_error(result: &litesvm::types::TransactionResult) -> Option<InstructionError> {
    match result {
        Err(failed) => match &failed.err {
            TransactionError::InstructionError(_, e) => Some(e.clone()),
            _ => None,
        },
        Ok(_) => None,
    }
}

/// The account's owning program. Panics if the account does not exist.
fn owner_of(env: &TestEnv, address: &Pubkey) -> Pubkey {
    env.svm
        .get_account(address)
        .unwrap_or_else(|| panic!("account {address} does not exist"))
        .owner
}

/// Moves the wallet under a foreign program with a genuine system `assign`: the
/// wallet itself signs and the environment's `payer` pays the fee (after the owner
/// change the wallet can no longer pay fees). Returns the new owner.
fn assign_to_foreign_program(env: &mut TestEnv, wallet: &Keypair) -> Pubkey {
    let new_owner = Pubkey::new_unique();
    let payer = env.payer.insecure_clone();
    let ix = solana_system_interface::instruction::assign(&wallet.pubkey(), &new_owner);

    env.expire_blockhash();
    env.send_ixs(&[ix], &payer.pubkey(), &[&payer, wallet])
        .expect("assigning the wallet to a foreign program");

    assert_eq!(
        owner_of(env, &wallet.pubkey()),
        new_owner,
        "after the assign the wallet belongs to the foreign program"
    );
    new_owner
}

/// Returns the wallet back under the System Program.
///
/// This is a simulation, not an honest path. `assign` changes the owner only of a
/// System-owned account, so the reverse move cannot be made by the wallet's own
/// signature - only the program the wallet sits under may hand ownership back, and
/// only while the account's data is empty. Here the new owner is an arbitrary address
/// with no program behind it, so for this particular wallet the move is irreversible;
/// the helper reproduces exactly the transition a participant-controlled owning
/// program would make in one instruction. The account's data is empty and its
/// lamports are preserved.
fn restore_system_ownership(env: &mut TestEnv, wallet: &Pubkey) {
    let mut account = env
        .svm
        .get_account(wallet)
        .unwrap_or_else(|| panic!("account {wallet} does not exist"));
    assert!(
        account.data.is_empty(),
        "a wallet with no data - returning ownership carries no content over"
    );
    account.owner = solana_sdk_ids::system_program::id();
    env.svm
        .set_account(*wallet, account)
        .expect("returning the wallet under the System Program");
}

// ===================================================================
// blacklist: a wallet under a foreign program can be banned and unbanned
// ===================================================================

#[test]
fn blacklist_add_remove_for_program_owned_wallet() {
    let mut env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();
    env.init_default_sale();

    let wallet = env.new_user();
    let foreign_program = assign_to_foreign_program(&mut env, &wallet);
    assert!(
        !env.is_blacklisted(&wallet.pubkey()),
        "before the add there must be no marker"
    );

    env.expire_blockhash();
    send(
        &mut env,
        blacklist_add_ix(operator.pubkey(), wallet.pubkey()),
        &operator,
    )
    .expect("blacklist_add for a wallet under a foreign program passes");

    assert!(
        env.is_blacklisted(&wallet.pubkey()),
        "the marker was created for a wallet under a foreign program"
    );
    assert_eq!(
        env.blacklist_entry(&wallet.pubkey()).wallet,
        wallet.pubkey(),
        "the marker records exactly this wallet"
    );
    assert_eq!(
        owner_of(&env, &wallet.pubkey()),
        foreign_program,
        "the wallet was under a foreign program throughout"
    );

    env.expire_blockhash();
    send(
        &mut env,
        blacklist_remove_ix(operator.pubkey(), wallet.pubkey()),
        &operator,
    )
    .expect("blacklist_remove for a wallet under a foreign program passes");
    assert!(
        !env.is_blacklisted(&wallet.pubkey()),
        "the marker is closed (the account is gone)"
    );
}

// ===================================================================
// blacklist: a mismatch between the argument and the PDA passed still reverts
// ===================================================================

#[test]
fn blacklist_add_seed_mismatch_still_fails_for_program_owned_wallet() {
    let mut env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();
    env.init_default_sale();

    let wallet = env.new_user();
    assign_to_foreign_program(&mut env, &wallet);

    // The argument is one wallet while the slot is derived from another: the addresses
    // will not agree.
    let other = Pubkey::new_unique();

    env.expire_blockhash();
    let r = send(
        &mut env,
        blacklist_add_ix_with_slot(operator.pubkey(), wallet.pubkey(), blacklist_pda(&other)),
        &operator,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(anchor_err(AnchorError::ConstraintSeeds)),
        "the slot is not derived from the wallet argument → ConstraintSeeds: {r:?}"
    );
    assert!(
        !env.is_blacklisted(&wallet.pubkey()),
        "no marker was created for wallet"
    );
    assert!(
        !env.is_blacklisted(&other),
        "no marker was created for other"
    );
}

// ===================================================================
// graylist: a wallet under a foreign program can be given, updated and stripped of
// its limits
// ===================================================================

#[test]
fn graylist_lifecycle_for_program_owned_wallet() {
    // Self-consistent limits: 0 < min ≤ max ≤ max_total.
    const MIN: u64 = 10;
    const MAX: u64 = 1_000;
    const TOTAL: u64 = 10_000;
    const NEW_MIN: u64 = 50;
    const NEW_MAX: u64 = 2_000;
    const NEW_TOTAL: u64 = 50_000;

    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    env.init_default_sale();

    let wallet = env.new_user();
    let foreign_program = assign_to_foreign_program(&mut env, &wallet);

    env.expire_blockhash();
    send(
        &mut env,
        graylist_create_ix(admin.pubkey(), wallet.pubkey(), MIN, MAX, TOTAL),
        &admin,
    )
    .expect("graylist_create for a wallet under a foreign program passes");

    let entry = env.graylist_entry(&wallet.pubkey());
    assert_eq!(
        entry.wallet,
        wallet.pubkey(),
        "the record holds exactly this wallet"
    );
    assert_eq!(entry.min_per_tx, MIN, "min_per_tx was stored");
    assert_eq!(entry.max_per_tx, MAX, "max_per_tx was stored");
    assert_eq!(entry.max_total, TOTAL, "max_total was stored");

    env.expire_blockhash();
    send(
        &mut env,
        graylist_update_ix(admin.pubkey(), wallet.pubkey(), NEW_MIN, NEW_MAX, NEW_TOTAL),
        &admin,
    )
    .expect("graylist_update for a wallet under a foreign program passes");

    let entry = env.graylist_entry(&wallet.pubkey());
    assert_eq!(entry.min_per_tx, NEW_MIN, "min_per_tx was overwritten");
    assert_eq!(entry.max_per_tx, NEW_MAX, "max_per_tx was overwritten");
    assert_eq!(entry.max_total, NEW_TOTAL, "max_total was overwritten");

    env.expire_blockhash();
    send(
        &mut env,
        graylist_remove_ix(admin.pubkey(), wallet.pubkey()),
        &admin,
    )
    .expect("graylist_remove for a wallet under a foreign program passes");
    assert!(
        !env.is_graylisted(&wallet.pubkey()),
        "the record is closed (the account is gone)"
    );
    assert_eq!(
        owner_of(&env, &wallet.pubkey()),
        foreign_program,
        "the wallet was under a foreign program throughout"
    );
}

// ===================================================================
// refund: the deposit of a wallet that moved under a foreign program is refunded
// ===================================================================

#[test]
fn refund_returns_usdc_to_program_owned_wallet() {
    // Two contributions: $25 (index 0) and $15 (index 1) within the $10…$10,000 bounds.
    // We refund the first; every expectation below is a literal computed by hand.
    const DEPOSIT_0: u64 = 25_000_000;
    const DEPOSIT_1: u64 = 15_000_000;
    const FUNDED: u64 = 40_000_000;

    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let operator = env.actors.operator.insecure_clone();
    let wallet = env.new_user();

    let p = env.init_default_sale();
    env.warp_to(p.sale_start_date);

    // An ordinary purchase with an ordinary wallet: the ATA is created and both
    // contributions are accepted.
    let ata = env.create_ata(&wallet.pubkey(), &usdc);
    env.mint_to(&usdc, &ata, FUNDED);
    send(&mut env, init_aggregate_ix(wallet.pubkey()), &wallet).expect("init_aggregate");
    env.expire_blockhash();
    send(
        &mut env,
        purchase_ix(wallet.pubkey(), ata, 0, DEPOSIT_0),
        &wallet,
    )
    .expect("the first purchase");
    env.expire_blockhash();
    send(
        &mut env,
        purchase_ix(wallet.pubkey(), ata, 1, DEPOSIT_1),
        &wallet,
    )
    .expect("the second purchase");

    let vault = deposit_vault_pda();
    assert_eq!(
        env.token_balance(&ata),
        0,
        "the ATA emptied out after the two purchases"
    );
    assert_eq!(env.token_balance(&vault), 40_000_000, "the vault = $40");
    assert_eq!(
        env.sale_state().deposits_usdc_contributed,
        40_000_000,
        "the accounted pool = $40"
    );

    // The wallet moves under a foreign program - which used to cut it off from refunds.
    let foreign_program = assign_to_foreign_program(&mut env, &wallet);
    let lamports_before = env.lamports_of(&wallet.pubkey());

    env.expire_blockhash();
    send(
        &mut env,
        refund_ix(operator.pubkey(), wallet.pubkey(), 0, usdc),
        &operator,
    )
    .expect("a refund to a wallet under a foreign program passes");

    assert_eq!(
        env.token_balance(&ata),
        25_000_000,
        "exactly $25 came back to the wallet's ATA"
    );
    assert_eq!(
        wallet_usdc_ata(&wallet.pubkey(), &usdc),
        ata,
        "the refund landed in the wallet's canonical ATA"
    );
    assert_eq!(
        env.token_balance(&vault),
        15_000_000,
        "$15 remained in the vault"
    );
    assert!(
        !env.deposit_exists(&wallet.pubkey(), 0),
        "deposit 0 is closed"
    );
    assert!(
        env.deposit_exists(&wallet.pubkey(), 1),
        "deposit 1 is untouched"
    );
    assert_eq!(
        env.wallet_aggregate(&wallet.pubkey()).net_usdc_contributed,
        15_000_000,
        "the wallet's net decreased by $25"
    );
    assert_eq!(
        env.sale_state().deposits_usdc_contributed,
        15_000_000,
        "the overall pool decreased by $25"
    );
    assert!(
        env.lamports_of(&wallet.pubkey()) > lamports_before,
        "the closed deposit's rent went to the wallet, foreign program and all"
    );
    assert_eq!(
        owner_of(&env, &wallet.pubkey()),
        foreign_program,
        "the refund did not require moving the wallet back under the System Program"
    );
}

// ===================================================================
// refund: the wallet has no USDC account - the refund creates it itself
// ===================================================================

#[test]
fn refund_creates_missing_ata_for_program_owned_wallet() {
    // The same contribution layout as above, but the wallet closed its own USDC account
    // before moving under the program. The refund then recreates the account - this is
    // the only place where the runtime still touches the wallet through a CPI, and there
    // it acts as the authority of the account being created.
    const DEPOSIT_0: u64 = 25_000_000;
    const DEPOSIT_1: u64 = 15_000_000;
    const FUNDED: u64 = 40_000_000;

    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let operator = env.actors.operator.insecure_clone();
    let wallet = env.new_user();

    let p = env.init_default_sale();
    env.warp_to(p.sale_start_date);

    let ata = env.create_ata(&wallet.pubkey(), &usdc);
    env.mint_to(&usdc, &ata, FUNDED);
    send(&mut env, init_aggregate_ix(wallet.pubkey()), &wallet).expect("init_aggregate");
    env.expire_blockhash();
    send(
        &mut env,
        purchase_ix(wallet.pubkey(), ata, 0, DEPOSIT_0),
        &wallet,
    )
    .expect("the first purchase");
    env.expire_blockhash();
    send(
        &mut env,
        purchase_ix(wallet.pubkey(), ata, 1, DEPOSIT_1),
        &wallet,
    )
    .expect("the second purchase");

    // The wallet closes its emptied USDC account while it can still sign transactions as
    // a fee payer.
    assert_eq!(
        env.token_balance(&ata),
        0,
        "the ATA emptied out after the two purchases"
    );
    let close_ix = spl_token_interface::instruction::close_account(
        &spl_token_interface::id(),
        &ata,
        &wallet.pubkey(),
        &wallet.pubkey(),
        &[],
    )
    .expect("the close_account instruction");
    env.expire_blockhash();
    send(&mut env, close_ix, &wallet).expect("the wallet closed its USDC account");
    assert!(!env.account_exists(&ata), "the wallet has no USDC account");

    let foreign_program = assign_to_foreign_program(&mut env, &wallet);

    let ata_rent = env
        .svm
        .minimum_balance_for_rent_exemption(spl_token_interface::state::Account::LEN);
    let operator_lamports_before = env.lamports_of(&operator.pubkey());

    env.expire_blockhash();
    send(
        &mut env,
        refund_ix(operator.pubkey(), wallet.pubkey(), 0, usdc),
        &operator,
    )
    .expect("the refund creates a USDC account for a wallet under a foreign program");

    assert!(
        env.account_exists(&ata),
        "the wallet's USDC account was created by the refund"
    );
    assert_eq!(
        wallet_usdc_ata(&wallet.pubkey(), &usdc),
        ata,
        "it was created at the canonical address"
    );
    assert_eq!(
        env.token_balance(&ata),
        25_000_000,
        "the created account holds exactly $25"
    );
    assert_eq!(
        env.token_account_mint_owner(&ata),
        (usdc, wallet.pubkey()),
        "the USDC mint, with the wallet itself in charge of the account"
    );
    assert!(
        operator_lamports_before.saturating_sub(env.lamports_of(&operator.pubkey())) >= ata_rent,
        "the operator paid the new account's rent"
    );
    assert_eq!(
        env.token_balance(&deposit_vault_pda()),
        15_000_000,
        "$15 remained in the vault"
    );
    assert!(
        !env.deposit_exists(&wallet.pubkey(), 0),
        "deposit 0 is closed"
    );
    assert_eq!(
        env.wallet_aggregate(&wallet.pubkey()).net_usdc_contributed,
        15_000_000,
        "the wallet's net decreased by $25"
    );
    assert_eq!(
        env.sale_state().deposits_usdc_contributed,
        15_000_000,
        "the overall pool decreased by $25"
    );
    assert_eq!(
        owner_of(&env, &wallet.pubkey()),
        foreign_program,
        "the wallet was under a foreign program throughout"
    );
}

// ===================================================================
// An end-to-end scenario: bought with an ordinary wallet → moved under a program →
// the ban still works → the next purchase is rejected by the ban
// ===================================================================

#[test]
fn program_owned_wallet_can_be_banned_and_then_cannot_purchase() {
    const DEPOSIT: u64 = 25_000_000; // $25 within the $10…$10,000 bounds
    const FUNDED: u64 = 50_000_000; // enough for two such purchases

    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let operator = env.actors.operator.insecure_clone();
    let wallet = env.new_user();

    let p = env.init_default_sale();
    env.warp_to(p.sale_start_date);

    let ata = env.create_ata(&wallet.pubkey(), &usdc);
    env.mint_to(&usdc, &ata, FUNDED);
    send(&mut env, init_aggregate_ix(wallet.pubkey()), &wallet).expect("init_aggregate");
    env.expire_blockhash();
    send(
        &mut env,
        purchase_ix(wallet.pubkey(), ata, 0, DEPOSIT),
        &wallet,
    )
    .expect("a purchase with an ordinary wallet");
    assert_eq!(
        env.token_balance(&ata),
        25_000_000,
        "after the first purchase $25 remained in the ATA"
    );

    // The move under a foreign program.
    assign_to_foreign_program(&mut env, &wallet);

    // The ban does reach such a wallet.
    env.expire_blockhash();
    send(
        &mut env,
        blacklist_add_ix(operator.pubkey(), wallet.pubkey()),
        &operator,
    )
    .expect("banning a wallet under a foreign program passes");
    assert!(env.is_blacklisted(&wallet.pubkey()), "the wallet is banned");

    // While the wallet is under a foreign program it cannot buy at all: a purchase
    // creates a deposit at the wallet's expense, and only the owning program may debit
    // lamports from an account. The rejection comes from the runtime while the deposit
    // is being created, before the instruction's body, so there is no WalletBlacklisted
    // code here.
    env.expire_blockhash();
    let r = send_paid_by_env(
        &mut env,
        purchase_ix(wallet.pubkey(), ata, 1, DEPOSIT),
        &wallet,
    );
    assert_eq!(
        instruction_error(&r),
        Some(InstructionError::ExternalAccountLamportSpend),
        "a purchase by a wallet under a foreign program → rejected while paying the rent: {r:?}"
    );
    assert!(
        !env.deposit_exists(&wallet.pubkey(), 1),
        "no deposit was created in this attempt either"
    );

    // Only the owning program can bring the wallet back under the System Program (see
    // restore_system_ownership); after that we check that the ban still holds.
    restore_system_ownership(&mut env, &wallet.pubkey());

    env.expire_blockhash();
    let r = send(
        &mut env,
        purchase_ix(wallet.pubkey(), ata, 1, DEPOSIT),
        &wallet,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::WalletBlacklisted)),
        "a purchase by the banned wallet → WalletBlacklisted: {r:?}"
    );
    assert!(
        !env.deposit_exists(&wallet.pubkey(), 1),
        "the second deposit was not created"
    );
    assert_eq!(
        env.token_balance(&ata),
        25_000_000,
        "no USDC was debited from the ATA"
    );
    assert_eq!(
        env.sale_state().deposits_usdc_contributed,
        25_000_000,
        "the pool did not grow"
    );
    assert_eq!(
        env.wallet_aggregate(&wallet.pubkey()).net_usdc_contributed,
        25_000_000,
        "the wallet's net did not grow"
    );
}
