//! The remaining account substitutions: the `vault_authority` slot in all four
//! instructions that accept it, direct SPL calls on a vault bypassing the program
//! (invariant I10), a recipient ATA under a DIFFERENT mint, a decoy event authority,
//! substitutions of the `vault` slot in `claim` / `cancel_vesting` / `close_vesting`,
//! and accounts at a NON-canonical address of the same seeds (the config and an
//! activity record).
//!
//! Both claim cases of group 5 (another activity's vault and a foreign token account)
//! also live in `claim.rs` - they are duplicated here deliberately, so that the
//! `vault`-slot substitution group reads as exhaustive across all three instructions
//! instead of sending the reader to another file for the third one.
//!
//! What is covered in other files and NOT duplicated here (listed by name: the suite
//! is one, and repeating a case would add no coverage):
//! - another activity's RECORD in the `vesting` slot - `claim.rs`
//!   (`claim_rejects_record_of_another_activity`), `cancel_close.rs`
//!   (`cancel_rejects_record_of_another_activity`,
//!   `close_rejects_record_of_another_activity`), `funding.rs`
//!   (`assert_funded_rejects_vesting_slot_of_another_activity`);
//! - another activity's vault in the `vault` slot of `cancel_vesting` /
//!   `close_vesting` / `assert_funded` - `cancel_close.rs`
//!   (`cancel_rejects_vault_of_another_activity`,
//!   `close_rejects_vault_of_another_activity`), `funding.rs`
//!   (`assert_funded_rejects_vault_slot_of_another_activity`);
//! - spoofing `vault_authority` in `create_vesting` and `set_recipient` -
//!   `create_vesting.rs`, `set_recipient.rs`; a decoy config and a foreign
//!   `token_mint` - in each instruction's own file.
//!
//! Why the tests are laid out the way they are.
//!
//! 1. The `vault_authority` slot is a data-less `UncheckedAccount`: substitute it and
//!    the CPI's signature would go under seeds the program never derived. It is
//!    checked on ALL four instructions rather than on one: their contexts differ and
//!    all they share is the constraint's text - the codes coinciding here is a
//!    consequence of the layout rather than a guarantee.
//! 2. Direct SPL calls (I10) are the only way to verify that the vault is controlled
//!    ONLY by `vault_authority`: there is no such path inside the program, so the call
//!    goes straight to SPL Token, bypassing Vesting. The error comes from spl-token
//!    (`OwnerMismatch`, `Custom(4)`) rather than from Anchor - and that is the
//!    observable proof that the account's owner is recorded as the program's PDA.
//! 3. Every group ends with a POSITIVE in the same environment: the same
//!    movement of funds, performed through the program's instruction, passes.
//!    Otherwise a "rejection" would prove only that the scenario does not work at all.
//! 4. The signer sweep in the I10 group covers all four roles (owner, operator,
//!    recipient, stranger): owner and operator hold privileges in the program, and it
//!    is on them that "the privilege does NOT extend to disposing of the token account
//!    directly" is verified.
//! 5. The Anchor and SPL error codes were pinned by the FIRST EXECUTION and are named
//!    by their variant (`ErrorCode::…`) or by a constant with the number in a comment -
//!    there are no `2xxx` / `6xxx` literals in the assertions.
//! 6. Two of the codes turned out not to be the ones the constraints' text suggests,
//!    and that is pinned by assertions: substituting `vault_authority` yields
//!    `ConstraintTokenOwner` (2015) rather than `ConstraintSeeds` (2006) - the `vault`
//!    slot is declared earlier and fails first; a decoy event authority yields
//!    `ConstraintAddress` (2012) rather than `ConstraintSeeds`. The invariant holds in
//!    both cases, but the on-chain diagnosis will be exactly that - and any refactor
//!    that changes the slot order turns these tests red.

use anchor_lang::error::ErrorCode;
use litesvm::types::TransactionResult;
use solana_instruction::Instruction;
use solana_instruction_error::InstructionError;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction_error::TransactionError;
use spl_token_interface::instruction::AuthorityType;
use vesting_test_common::vesting_expected::vesting::{
    ECOSYSTEM_FULL_RELEASE_DAY, V2_REFERENCE, V2_VESTED_DAY_151,
};
use vesting_test_common::{
    assert_funded_ix, assert_funded_ix_with_slots, cancel_vesting_ix, cancel_vesting_ix_with_slots,
    claim_ix, claim_ix_with_slots, close_vesting_ix, close_vesting_ix_with_slots, config_pda,
    custom_error_code, event_authority_pda, non_canonical_pda, recipient_ata, vault_authority_pda,
    vault_pda, vesting_pda, without_signer, ActivitySpec, TestEnv, DEFAULT_TGE_OFFSET,
};

/// The activity's reference amount: the V2 bucket of §10.5 (100,000,000 $1WIN in token
/// units).
const REFERENCE: u64 = V2_REFERENCE;

/// The day of the first claim: by then [`V2_VESTED_DAY_151`] is unlocked - strictly
/// above zero, so a substitution's rejection cannot be masked by `NothingToClaim`.
const DAY_151: i64 = 151;

/// The SPL Token program's `TokenError::OwnerMismatch` code: the fifth variant of its
/// enum (`NotRentExempt` = 0 … `OwnerMismatch` = 4), arriving as `Custom(4)`. A
/// literal in the test: this is a foreign (not our) interface, and taking it from the
/// crate would mean checking the encoding against itself. Anchor codes start at 6000
/// and cannot be confused with it.
const SPL_ERROR_OWNER_MISMATCH: u32 = 4;

/// `TokenError::NonNativeHasBalance` from the same enum (the twelfth variant, 11): "a
/// non-empty non-native token account cannot be closed". It arrives instead of
/// [`SPL_ERROR_OWNER_MISMATCH`] when `CloseAccount` is addressed to a vault with a
/// non-zero balance - spl-token's balance check comes before its owner check.
const SPL_ERROR_NON_NATIVE_HAS_BALANCE: u32 = 11;

/// The observable state around the activity. Comparing the snapshots before and after
/// is the "the rejection left no trace" assertion in a single line.
///
/// `vault_balance` / `vault_owner` are read with a panic on a missing vault: in every
/// negative of this file it must stay alive, and "the account is gone" must never be
/// read silently as "empty" (fail-closed).
#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    vesting_exists: bool,
    vault_exists: bool,
    vault_balance: u64,
    vault_owner: Pubkey,
    claimed: u64,
    recipient_ata_balance: u64,
    activity_count: u64,
}

fn snapshot(env: &TestEnv, activity_id: u64, recipient: &Pubkey) -> Snapshot {
    let (_, vault_owner) = env.token_account_mint_owner(&vault_pda(activity_id));
    Snapshot {
        vesting_exists: env.vesting_exists(activity_id),
        vault_exists: env.vault_exists(activity_id),
        vault_balance: env.vault_balance(activity_id),
        vault_owner,
        claimed: env.vesting(activity_id).claimed,
        recipient_ata_balance: env.recipient_ata_balance(recipient),
        activity_count: env.vesting_config().activity_count,
    }
}

/// An environment with a config (TGE in a month), an activity on the "Ecosystem"
/// schedule for [`REFERENCE`], and a vault funded to exactly the reference amount.
/// Returns `(env, activity_id, tge_ts)`.
fn setup() -> (TestEnv, u64, i64) {
    let mut env = TestEnv::new();
    let params = env.init_default(DEFAULT_TGE_OFFSET);
    let activity_id = env.create_activity(&ActivitySpec::valid(REFERENCE));
    env.fund_vault(activity_id, REFERENCE);
    (env, activity_id, params.tge_ts)
}

/// [`setup`] plus the canonical `beneficiary_token` - the ATA of the single withdrawal
/// recipient W (`config.withdrawal_authority` = the `rent_collector` actor) under
/// $1WIN. Needed by the `cancel_vesting` / `close_vesting` instructions.
fn setup_with_beneficiary() -> (TestEnv, u64, Pubkey, i64) {
    let (mut env, activity_id, tge_ts) = setup();
    let withdrawal = env.actors.rent_collector.pubkey();
    let win_mint = env.win_mint;
    let beneficiary_token = env.create_ata(&withdrawal, &win_mint);
    (env, activity_id, beneficiary_token, tge_ts)
}

/// A claim by the activity's recipient (`actors.recipients[0]`).
#[allow(clippy::result_large_err)]
fn claim_by_recipient(env: &mut TestEnv, activity_id: u64) -> TransactionResult {
    let recipient = env.actors.recipients[0].insecure_clone();
    env.claim_as(&recipient, activity_id, recipient.pubkey())
}

/// Sends an instruction signed by `actor`; the fee payer is the environment's `payer`,
/// so that the fee does not move the actors' lamports (they are part of the rent
/// assertions).
#[allow(clippy::result_large_err)]
fn send_as(env: &mut TestEnv, ix: Instruction, actor: &Keypair) -> TransactionResult {
    let payer = env.payer.insecure_clone();
    env.send_ixs(&[ix], &payer.pubkey(), &[&payer, actor])
}

/// The keys used to attempt disposing of the vault directly: the program's two
/// privileged roles, the payout recipient and an outsider.
fn outside_signers(env: &TestEnv) -> Vec<(&'static str, Keypair)> {
    vec![
        ("owner", env.actors.owner.insecure_clone()),
        ("operator", env.actors.operator.insecure_clone()),
        ("recipient", env.actors.recipients[0].insecure_clone()),
        ("stranger", env.actors.stranger.insecure_clone()),
    ]
}

/// Replaces the key `from` with `to` across every meta of an already assembled
/// instruction.
///
/// A separate combinator rather than a builder parameter: the `event_authority` and
/// `program` slots are added by Anchor itself (`#[event_cpi]`), they appear in no
/// builder's argument list, and the substitution is needed in exactly this file.
///
/// It panics if the key is absent from the instruction: the test would be checking
/// something other than what it intended (fail-closed).
fn substitute_account(mut ix: Instruction, from: &Pubkey, to: Pubkey) -> Instruction {
    let mut replaced = 0usize;
    for meta in &mut ix.accounts {
        if meta.pubkey == *from {
            meta.pubkey = to;
            replaced += 1;
        }
    }
    assert!(
        replaced > 0,
        "account {from} is absent from the instruction - there is nothing to substitute"
    );
    ix
}

// ===================================================================
// Group 1 - substituting the `vault_authority` slot in four instructions
// ===================================================================

#[test]
fn claim_rejects_non_canonical_vault_authority_slot() {
    let (mut env, id, tge_ts) = setup();
    let recipient = env.actors.recipients[0].insecure_clone();
    let win_mint = env.win_mint;
    env.warp_to_day(tge_ts, DAY_151);
    let before = snapshot(&env, id, &recipient.pubkey());

    // A decoy authority: were it to pass, `token::authority = vault_authority` would be
    // checked against it too, and the CPI's signature would go under seeds the program
    // never derived.
    let ix = claim_ix_with_slots(
        recipient.pubkey(),
        config_pda(),
        vesting_pda(id),
        vault_pda(id),
        recipient.pubkey(),
        recipient_ata(&recipient.pubkey(), &win_mint),
        Pubkey::new_unique(),
        win_mint,
        spl_token_interface::id(),
        id,
    );
    let r = send_as(&mut env, ix, &recipient);
    // The code is pinned by the first execution: `ConstraintTokenOwner` (2015) and NOT
    // `ConstraintSeeds` (2006). Anchor resolves the slots in declaration order and
    // `vault` is declared before `vault_authority`: what fails first is its
    // `token::authority = vault_authority` binding (the vault's owner is the canonical
    // PDA while a foreign key was submitted into the slot). The `vault_authority` slot's
    // own seeds remain the second layer - it is what would hold the invariant should
    // anyone remove `token::authority` from the vault.
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintTokenOwner)),
        "a decoy vault_authority in claim → a rejection on the vault's binding: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &recipient.pubkey()),
        before,
        "the rejection touched neither the accounting, nor the vault, nor the recipient's account"
    );
    assert!(
        !env.recipient_ata_exists(&recipient.pubkey()),
        "the recipient's ATA was not created: the revert rolled back init_if_needed too"
    );

    // A POSITIVE: the canonical slot passes with the same call.
    env.expire_blockhash();
    claim_by_recipient(&mut env, id).expect("the canonical vault_authority is accepted");
    assert_eq!(env.vesting(id).claimed, V2_VESTED_DAY_151);
}

#[test]
fn cancel_rejects_non_canonical_vault_authority_slot() {
    let (mut env, id, beneficiary_token, _tge) = setup_with_beneficiary();
    let owner = env.actors.owner.insecure_clone();
    let rent_collector = env.actors.rent_collector.pubkey();
    let win_mint = env.win_mint;
    let recipient = env.actors.recipients[0].pubkey();
    let before = snapshot(&env, id, &recipient);

    let ix = cancel_vesting_ix_with_slots(
        owner.pubkey(),
        config_pda(),
        vesting_pda(id),
        vault_pda(id),
        beneficiary_token,
        rent_collector,
        Pubkey::new_unique(),
        win_mint,
        id,
    );
    let r = send_as(&mut env, ix, &owner);
    // The same resolution order as in claim: `vault` is declared before
    // `vault_authority`, so its `token::authority` fails first.
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintTokenOwner)),
        "a decoy vault_authority in cancel_vesting → a rejection on the vault's binding: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &recipient),
        before,
        "the rejection touched neither the record nor the vault"
    );
    assert_eq!(
        env.token_balance(&beneficiary_token),
        0,
        "not a single unit reached the beneficiary's account"
    );

    // A POSITIVE: the canonical slot withdraws the remainder and closes both
    // accounts with the same call.
    env.expire_blockhash();
    let ix = cancel_vesting_ix(
        owner.pubkey(),
        id,
        beneficiary_token,
        rent_collector,
        win_mint,
    );
    send_as(&mut env, ix, &owner).expect("the canonical vault_authority is accepted");
    assert_eq!(
        env.token_balance(&beneficiary_token),
        REFERENCE,
        "the vault's remainder went to the beneficiary"
    );
    assert!(!env.vault_exists(id), "the vault is closed");
    assert!(!env.vesting_exists(id), "the record is closed");
}

#[test]
fn close_rejects_non_canonical_vault_authority_slot() {
    let (mut env, id, beneficiary_token, tge_ts) = setup_with_beneficiary();
    let owner = env.actors.owner.insecure_clone();
    let rent_collector = env.actors.rent_collector.pubkey();
    let win_mint = env.win_mint;
    let recipient = env.actors.recipients[0].pubkey();

    // The activity is fully drawn down: the `claimed >= reference_amount` gate is
    // satisfied and the only possible cause of a rejection is the substitution.
    env.warp_to_day(tge_ts, ECOSYSTEM_FULL_RELEASE_DAY);
    claim_by_recipient(&mut env, id)
        .expect("the terminal claim releases the whole reference amount");
    assert_eq!(
        env.vesting(id).claimed,
        REFERENCE,
        "the activity is closable"
    );
    let before = snapshot(&env, id, &recipient);

    let ix = close_vesting_ix_with_slots(
        owner.pubkey(),
        config_pda(),
        vesting_pda(id),
        vault_pda(id),
        beneficiary_token,
        rent_collector,
        Pubkey::new_unique(),
        win_mint,
        id,
    );
    let r = send_as(&mut env, ix, &owner);
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintTokenOwner)),
        "a decoy vault_authority in close_vesting → a rejection on the vault's binding: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &recipient),
        before,
        "the rejection touched neither the record nor the vault"
    );

    // A POSITIVE: the canonical slot closes both accounts.
    env.expire_blockhash();
    let ix = close_vesting_ix(
        owner.pubkey(),
        id,
        beneficiary_token,
        rent_collector,
        win_mint,
    );
    send_as(&mut env, ix, &owner).expect("the canonical vault_authority is accepted");
    assert!(!env.vault_exists(id), "the vault is closed");
    assert!(!env.vesting_exists(id), "the record is closed");
}

#[test]
fn assert_funded_rejects_non_canonical_vault_authority_slot() {
    let (mut env, id, _tge) = setup();
    let stranger = env.actors.stranger.insecure_clone();
    let win_mint = env.win_mint;
    let recipient = env.actors.recipients[0].pubkey();
    let before = snapshot(&env, id, &recipient);

    let ix = assert_funded_ix_with_slots(
        config_pda(),
        vesting_pda(id),
        vault_pda(id),
        Pubkey::new_unique(),
        win_mint,
        id,
    );
    // The instruction has no signer: the transaction is signed only by the fee payer.
    let r = env.send_ixs(&[ix], &stranger.pubkey(), &[&stranger]);
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintTokenOwner)),
        "a decoy vault_authority in assert_funded → a rejection on the vault's binding: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &recipient),
        before,
        "the coverage check changes no state, on a rejection or on success"
    );

    // A POSITIVE: the canonical slot passes with the same call.
    env.expire_blockhash();
    let ix = assert_funded_ix(id, win_mint);
    env.send_ixs(&[ix], &stranger.pubkey(), &[&stranger])
        .expect("the canonical vault_authority is accepted");
}

// ===================================================================
// Group 2 [I10] - direct SPL calls on the vault, bypassing the program
// ===================================================================

#[test]
fn direct_spl_transfer_from_vault_is_rejected_for_every_outside_signer() {
    let (mut env, id, tge_ts) = setup();
    let win_mint = env.win_mint;
    let recipient = env.actors.recipients[0].pubkey();
    // The receiving account is created by an honest ATA instruction: the ATA program does
    // not ask for the owner's signature, so anyone can create such an account in advance.
    let stranger_ata = env.create_ata(&env.actors.stranger.pubkey(), &win_mint);
    let before = snapshot(&env, id, &recipient);

    for (label, actor) in outside_signers(&env) {
        env.expire_blockhash();
        let ix = spl_token_interface::instruction::transfer(
            &spl_token_interface::id(),
            &vault_pda(id),
            &stranger_ata,
            &actor.pubkey(),
            &[],
            1,
        )
        .expect("the SPL Transfer instruction");
        let r = send_as(&mut env, ix, &actor);
        assert_eq!(
            custom_error_code(&r),
            Some(SPL_ERROR_OWNER_MISMATCH),
            "{label} signed a direct Transfer out of the vault → OwnerMismatch from spl-token: the account's owner is the program's PDA rather than a role (I10): {r:?}"
        );
        assert_eq!(
            snapshot(&env, id, &recipient),
            before,
            "{label}: the vault lost not a single unit"
        );
        assert_eq!(
            env.token_balance(&stranger_ata),
            0,
            "{label}: not a single unit reached the foreign account"
        );
    }

    // A POSITIVE: the same movement of funds through the program's instruction
    // passes - the vault works and the rejections were indeed about the account's owner.
    env.warp_to_day(tge_ts, DAY_151);
    env.expire_blockhash();
    claim_by_recipient(&mut env, id)
        .expect("the claim moves the vault with vault_authority's signature");
    assert_eq!(
        env.vault_balance(id),
        REFERENCE - V2_VESTED_DAY_151,
        "the vault decreased by exactly what was released"
    );
}

#[test]
fn direct_spl_close_account_on_vault_is_rejected_for_every_outside_signer() {
    let (mut env, funded_id, beneficiary_token, _tge) = setup_with_beneficiary();
    let owner = env.actors.owner.insecure_clone();
    let rent_collector = env.actors.rent_collector.pubkey();
    let win_mint = env.win_mint;
    let recipient = env.actors.recipients[0].pubkey();

    // A second activity with NO funding: in SPL Token the "the balance is zero" check
    // comes BEFORE the owner check, so on a non-empty vault the rejection comes from the
    // balance (`NonNativeHasBalance`, 11) and the owner is never checked at all. The sharp
    // case of I10 is precisely an empty vault: there the owner is the only obstacle left,
    // and the prize is its rent.
    let second_recipient = env.actors.recipients[1].pubkey();
    let empty_id = env.create_activity_for(&second_recipient, &ActivitySpec::valid(REFERENCE));
    assert_eq!(
        env.vault_balance(empty_id),
        0,
        "precondition: the second activity's vault is empty"
    );
    let before_funded = snapshot(&env, funded_id, &recipient);
    let before_empty = snapshot(&env, empty_id, &second_recipient);

    for (label, actor) in outside_signers(&env) {
        // The closed account's rent would go to the signer itself - which is what makes it
        // dangerous.
        for (id, expected, why) in [
            (
                funded_id,
                SPL_ERROR_NON_NATIVE_HAS_BALANCE,
                "a non-empty vault: the rejection comes from the balance and never reaches the owner",
            ),
            (
                empty_id,
                SPL_ERROR_OWNER_MISMATCH,
                "an empty vault: the rejection comes from the owner - the rent stays with the program",
            ),
        ] {
            env.expire_blockhash();
            let ix = spl_token_interface::instruction::close_account(
                &spl_token_interface::id(),
                &vault_pda(id),
                &actor.pubkey(),
                &actor.pubkey(),
                &[],
            )
            .expect("the SPL CloseAccount instruction");
            let r = send_as(&mut env, ix, &actor);
            assert_eq!(
                custom_error_code(&r),
                Some(expected),
                "{label} signed a direct CloseAccount of the vault ({why}): {r:?}"
            );
            assert!(
                env.vault_exists(id),
                "{label}: activity {id}'s vault was not closed"
            );
        }
        assert_eq!(
            snapshot(&env, funded_id, &recipient),
            before_funded,
            "{label}: the funded vault is alive and intact"
        );
        assert_eq!(
            snapshot(&env, empty_id, &second_recipient),
            before_empty,
            "{label}: the empty vault is alive and its rent is in place"
        );
    }

    // A POSITIVE: the program closes the vault - a cancellation of the activity
    // before TGE. Empty and non-empty vaults close identically, the only difference being
    // the transfer.
    env.expire_blockhash();
    let rent_collector_lamports_before = env.lamports_of(&rent_collector);
    let vault_rent = env.lamports_of(&vault_pda(empty_id));
    let record_rent = env.lamports_of(&vesting_pda(empty_id));
    let ix = cancel_vesting_ix(
        owner.pubkey(),
        empty_id,
        beneficiary_token,
        rent_collector,
        win_mint,
    );
    send_as(&mut env, ix, &owner)
        .expect("cancel_vesting closes an empty vault with the PDA's signature");
    assert!(
        !env.vault_exists(empty_id),
        "the vault was closed by the program"
    );
    assert_eq!(
        env.lamports_of(&rent_collector) - rent_collector_lamports_before,
        vault_rent + record_rent,
        "the rent of both accounts went to the withdrawal recipient rather than to the signer of the direct call"
    );

    env.expire_blockhash();
    let ix = cancel_vesting_ix(
        owner.pubkey(),
        funded_id,
        beneficiary_token,
        rent_collector,
        win_mint,
    );
    send_as(&mut env, ix, &owner).expect("cancel_vesting closes a non-empty vault");
    assert!(
        !env.vault_exists(funded_id),
        "the vault was closed by the program"
    );
    assert_eq!(
        env.token_balance(&beneficiary_token),
        REFERENCE,
        "the remainder went to an account under withdrawal_authority's custody rather than to the signer"
    );
}

#[test]
fn direct_spl_set_authority_on_vault_is_rejected_for_every_outside_signer() {
    let (mut env, id, tge_ts) = setup();
    let recipient = env.actors.recipients[0].pubkey();
    let before = snapshot(&env, id, &recipient);

    // Both authority types of a token account: seizing ownership and seizing the right to
    // close. The class is closed in full rather than on a single variant.
    for authority_type in [AuthorityType::AccountOwner, AuthorityType::CloseAccount] {
        for (label, actor) in outside_signers(&env) {
            env.expire_blockhash();
            let ix = spl_token_interface::instruction::set_authority(
                &spl_token_interface::id(),
                &vault_pda(id),
                Some(&actor.pubkey()),
                authority_type.clone(),
                &actor.pubkey(),
                &[],
            )
            .expect("the SPL SetAuthority instruction");
            let r = send_as(&mut env, ix, &actor);
            assert_eq!(
                custom_error_code(&r),
                Some(SPL_ERROR_OWNER_MISMATCH),
                "{label} signed SetAuthority({authority_type:?}) on the vault → OwnerMismatch (I10): {r:?}"
            );
            assert_eq!(
                snapshot(&env, id, &recipient),
                before,
                "{label}/{authority_type:?}: the vault's owner did not change"
            );
        }
    }
    assert_eq!(
        env.token_account_mint_owner(&vault_pda(id)).1,
        vault_authority_pda(),
        "the vault's owner remained the program's canonical PDA"
    );

    // A POSITIVE: ownership is intact - the claim passes and moves the vault.
    env.warp_to_day(tge_ts, DAY_151);
    env.expire_blockhash();
    claim_by_recipient(&mut env, id).expect("a claim after the failed authority seizures");
    assert_eq!(env.vesting(id).claimed, V2_VESTED_DAY_151);
}

#[test]
fn direct_spl_transfer_under_vault_authority_slot_fails_without_program_signature() {
    let (mut env, id, tge_ts) = setup();
    let win_mint = env.win_mint;
    let recipient = env.actors.recipients[0].pubkey();
    let stranger_ata = env.create_ata(&env.actors.stranger.pubkey(), &win_mint);
    let before = snapshot(&env, id, &recipient);

    // The authority key is given CORRECTLY - the canonical `vault_authority` - but there
    // is nothing to sign it with from outside: a PDA has no private key. The signature
    // flag is cleared explicitly, otherwise the transaction would not assemble at all.
    let ix = spl_token_interface::instruction::transfer(
        &spl_token_interface::id(),
        &vault_pda(id),
        &stranger_ata,
        &vault_authority_pda(),
        &[],
        1,
    )
    .expect("the SPL Transfer instruction");
    // The transaction has exactly one signer - its fee payer: the message requires no
    // more signatures, and a superfluous key would be filtered out as `TooManySigners`
    // before execution.
    let payer = env.payer.insecure_clone();
    let ix = without_signer(ix, &vault_authority_pda());
    let r = env.send_ixs(&[ix], &payer.pubkey(), &[&payer]);
    // Not a `Custom`: it never gets as far as the owner check - spl-token demands the
    // authority's signature. The only way to obtain it is a CPI from the program that
    // owns the seeds `[b"vault_authority"]`.
    assert!(
        matches!(
            &r,
            Err(failed)
                if failed.err
                    == TransactionError::InstructionError(
                        0,
                        InstructionError::MissingRequiredSignature
                    )
        ),
        "vault_authority with no signature → MissingRequiredSignature rather than Custom: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &recipient),
        before,
        "the vault lost not a single unit"
    );
    assert_eq!(env.token_balance(&stranger_ata), 0);

    // A POSITIVE: the program issues that same signature - and the transfer
    // passes.
    env.warp_to_day(tge_ts, DAY_151);
    env.expire_blockhash();
    claim_by_recipient(&mut env, id).expect("the PDA's signature comes only from the program");
    assert_eq!(
        env.recipient_ata_balance(&recipient),
        V2_VESTED_DAY_151,
        "the payout went to the recipient"
    );
}

// ===================================================================
// Group 3 - the recipient's ATA under a DIFFERENT mint
// ===================================================================

#[test]
fn claim_rejects_recipient_ata_of_another_mint() {
    let (mut env, id, tge_ts) = setup();
    let recipient = env.actors.recipients[0].insecure_clone();
    let win_mint = env.win_mint;
    let foreign_mint = env.foreign_mint;
    env.warp_to_day(tge_ts, DAY_151);

    // The recipient's account under a FOREIGN mint (the same 8 decimals) was created
    // honestly and exists, so `init_if_needed` takes the "already created" branch and
    // checks the bindings. The `token_mint` slot is canonical meanwhile - exactly the
    // payout addressee was substituted.
    let foreign_ata = env.create_ata(&recipient.pubkey(), &foreign_mint);
    let before = snapshot(&env, id, &recipient.pubkey());

    let ix = claim_ix_with_slots(
        recipient.pubkey(),
        config_pda(),
        vesting_pda(id),
        vault_pda(id),
        recipient.pubkey(),
        foreign_ata,
        vault_authority_pda(),
        win_mint,
        spl_token_interface::id(),
        id,
    );
    let r = send_as(&mut env, ix, &recipient);
    // The code is pinned by the first execution: `ConstraintTokenMint` (2014) -
    // `associated_token::mint = token_mint` checks an existing account's mint before the
    // derivation of its address (`ConstraintAssociated`, 2009).
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintTokenMint)),
        "an ATA under a different mint in the addressee slot → a rejection BEFORE the transfer: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &recipient.pubkey()),
        before,
        "the rejection touched neither the accounting nor the vault"
    );
    assert_eq!(
        env.token_balance(&foreign_ata),
        0,
        "not a single unit reached the account under the foreign mint"
    );
    assert!(
        !env.recipient_ata_exists(&recipient.pubkey()),
        "the recipient's canonical ATA did not appear either"
    );

    // A POSITIVE: the canonical addressee passes with the same call.
    env.expire_blockhash();
    claim_by_recipient(&mut env, id).expect("an ATA under the config's mint is accepted");
    assert_eq!(
        env.recipient_ata_balance(&recipient.pubkey()),
        V2_VESTED_DAY_151,
        "the payout went to the ATA under $1WIN"
    );
    assert_eq!(
        env.token_balance(&foreign_ata),
        0,
        "the account under the foreign mint stayed empty"
    );
}

// ===================================================================
// Group 4 - a decoy event authority in an instruction with `#[event_cpi]`
// ===================================================================

#[test]
fn claim_rejects_forged_event_authority() {
    let (mut env, id, tge_ts) = setup();
    let recipient = env.actors.recipients[0].insecure_clone();
    let win_mint = env.win_mint;
    env.warp_to_day(tge_ts, DAY_151);
    let before = snapshot(&env, id, &recipient.pubkey());

    // The `event_authority` slot is added by Anchor itself and bound to the seeds
    // `[b"__event_authority"]`. Were the substitution to pass, the event's self-CPI would
    // go out under a foreign address and an indexer reading events by the program's
    // authority would lose the payout from sight.
    let ix = substitute_account(
        claim_ix(recipient.pubkey(), id, recipient.pubkey(), win_mint),
        &event_authority_pda(),
        Pubkey::new_unique(),
    );
    let r = send_as(&mut env, ix, &recipient);
    // The code is pinned by the first execution: `ConstraintAddress` (2012) rather than
    // `ConstraintSeeds` (2006): in `#[event_cpi]` Anchor 1.0.2 derives the event
    // authority's address itself and checks the submitted slot against the derived one
    // through an `address` binding rather than by checking the submitted account's seeds.
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintAddress)),
        "a decoy event_authority → ConstraintAddress: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &recipient.pubkey()),
        before,
        "the rejection touched neither the accounting, nor the vault, nor the recipient's account"
    );
    assert!(
        !env.recipient_ata_exists(&recipient.pubkey()),
        "the recipient's ATA was not created: the revert rolled back init_if_needed too"
    );

    // A POSITIVE: the canonical event authority passes with the same call.
    env.expire_blockhash();
    claim_by_recipient(&mut env, id).expect("the canonical event_authority is accepted");
    assert_eq!(env.vesting(id).claimed, V2_VESTED_DAY_151);
}

// ===================================================================
// Group 5 - substituting the `vault` slot in claim / cancel / close
//
// The `vault` slot is bound in THREE ways: seeds+bump, `token::mint` and
// `token::authority`. Hence two substitutions per instruction: ANOTHER activity's
// vault (the mint and authority are correct while the seeds derive from a foreign
// `activity_id` - isolation I2) and a foreign token account with the correct mint and
// authority outside the canonical PDA (two of the three bindings are satisfied and
// exactly the seeds reject it).
//
// The price of a substitution differs across the instructions, so all three are
// checked: `claim` moves PART of the remainder to the recipient's account, while
// `cancel_vesting` and `close_vesting` DRAIN the remainder in full and close the
// account - a decoy account would be emptied and closed, and its rent would go to the
// `rent_collector`.
// ===================================================================

/// A token account with the CORRECT mint and authority but not at the vault's
/// canonical PDA: `vault_authority`'s own ATA, funded with [`REFERENCE`]. It satisfies
/// the `token::mint` / `token::authority` bindings - the seeds are what reject it.
fn decoy_vault(env: &mut TestEnv) -> Pubkey {
    let win_mint = env.win_mint;
    let decoy = env.create_ata(&vault_authority_pda(), &win_mint);
    env.mint_to(&win_mint, &decoy, REFERENCE);
    assert_eq!(
        env.token_account_mint_owner(&decoy),
        (win_mint, vault_authority_pda()),
        "precondition: the decoy account's mint and authority match the vault's"
    );
    decoy
}

/// Two funded activities with DIFFERENT recipients, with time moved to day
/// [`DAY_151`]: before it there is nothing to claim and a substitution's rejection
/// could not be told apart from `NothingToClaim`. Two activities are needed because on
/// a single one substituting the vault is unobservable. Returns `(env, id_a, id_b)`.
fn setup_two_funded() -> (TestEnv, u64, u64) {
    let mut env = TestEnv::new();
    let params = env.init_default(DEFAULT_TGE_OFFSET);
    let first = env.actors.recipients[0].pubkey();
    let second = env.actors.recipients[1].pubkey();
    let id_a = env.create_activity_for(&first, &ActivitySpec::valid(REFERENCE));
    let id_b = env.create_activity_for(&second, &ActivitySpec::valid(REFERENCE));
    env.fund_vault(id_a, REFERENCE);
    env.fund_vault(id_b, REFERENCE);
    env.warp_to_day(params.tge_ts, DAY_151);
    (env, id_a, id_b)
}

#[test]
fn claim_rejects_vault_of_another_activity() {
    let (mut env, id_a, id_b) = setup_two_funded();
    let recipient = env.actors.recipients[0].insecure_clone();
    let recipient_b = env.actors.recipients[1].pubkey();
    let win_mint = env.win_mint;
    let before_a = snapshot(&env, id_a, &recipient.pubkey());
    let before_b = snapshot(&env, id_b, &recipient_b);

    // The record belongs to activity A while the vault belongs to activity B: were the
    // substitution to pass, A's recipient would be paying itself tokens out of the
    // neighbour's vault until it ran dry.
    let ix = claim_ix_with_slots(
        recipient.pubkey(),
        config_pda(),
        vesting_pda(id_a),
        vault_pda(id_b),
        recipient.pubkey(),
        recipient_ata(&recipient.pubkey(), &win_mint),
        vault_authority_pda(),
        win_mint,
        spl_token_interface::id(),
        id_a,
    );
    let r = send_as(&mut env, ix, &recipient);
    // The code is pinned by execution: `ConstraintSeeds` (2006) and NOT
    // `ConstraintTokenOwner` (2015) as in group 1: the foreign vault's mint and authority
    // are correct, so exactly the seed check fails - a vault's address is derived from the
    // same `activity_id` as the record (I2).
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintSeeds)),
        "another activity's vault in claim's vault slot → a rejection on the seeds: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id_a, &recipient.pubkey()),
        before_a,
        "activity A is intact: the rejection touched neither its accounting nor its vault"
    );
    assert_eq!(
        snapshot(&env, id_b, &recipient_b),
        before_b,
        "activity B is intact: the neighbour's vault lost not a single unit"
    );
    assert!(
        !env.recipient_ata_exists(&recipient.pubkey()),
        "the recipient's ATA was not created: the revert rolled back init_if_needed too"
    );

    // A POSITIVE: the canonical vault releases what is unlocked with the same call.
    env.expire_blockhash();
    claim_by_recipient(&mut env, id_a).expect("the canonical vault in the slot is accepted");
    assert_eq!(env.vesting(id_a).claimed, V2_VESTED_DAY_151);
    assert_eq!(
        env.vault_balance(id_a),
        REFERENCE - V2_VESTED_DAY_151,
        "the payout was debited from ITS OWN activity's vault"
    );
    assert_eq!(
        env.vault_balance(id_b),
        REFERENCE,
        "the positive did not touch the neighbour's vault either"
    );
}

#[test]
fn claim_rejects_non_canonical_token_account_in_vault_slot() {
    let (mut env, id_a, id_b) = setup_two_funded();
    let recipient = env.actors.recipients[0].insecure_clone();
    let recipient_b = env.actors.recipients[1].pubkey();
    let win_mint = env.win_mint;
    let decoy = decoy_vault(&mut env);
    // The snapshots AFTER the decoy account was prepared.
    let before_a = snapshot(&env, id_a, &recipient.pubkey());
    let before_b = snapshot(&env, id_b, &recipient_b);

    let ix = claim_ix_with_slots(
        recipient.pubkey(),
        config_pda(),
        vesting_pda(id_a),
        decoy,
        recipient.pubkey(),
        recipient_ata(&recipient.pubkey(), &win_mint),
        vault_authority_pda(),
        win_mint,
        spl_token_interface::id(),
        id_a,
    );
    let r = send_as(&mut env, ix, &recipient);
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintSeeds)),
        "matching the mint and the authority is not enough: the payout's vault is also bound by seeds: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id_a, &recipient.pubkey()),
        before_a,
        "activity A is intact: the rejection touched neither its accounting nor its vault"
    );
    assert_eq!(
        snapshot(&env, id_b, &recipient_b),
        before_b,
        "activity B is intact"
    );
    assert_eq!(
        env.token_balance(&decoy),
        REFERENCE,
        "the decoy account lost not a single unit"
    );
    assert!(
        !env.recipient_ata_exists(&recipient.pubkey()),
        "the recipient's ATA was not created: the revert rolled back init_if_needed too"
    );

    // A POSITIVE: the canonical vault releases what is unlocked with the same call.
    env.expire_blockhash();
    claim_by_recipient(&mut env, id_a).expect("the canonical vault in the slot is accepted");
    assert_eq!(env.vesting(id_a).claimed, V2_VESTED_DAY_151);
    assert_eq!(
        env.vault_balance(id_a),
        REFERENCE - V2_VESTED_DAY_151,
        "the payout was debited from the canonical vault"
    );
    assert_eq!(
        env.token_balance(&decoy),
        REFERENCE,
        "the positive did not touch the decoy account either"
    );
}

#[test]
fn cancel_rejects_non_canonical_token_account_in_vault_slot() {
    let (mut env, id, beneficiary_token, _tge) = setup_with_beneficiary();
    let owner = env.actors.owner.insecure_clone();
    let rent_collector = env.actors.rent_collector.pubkey();
    let win_mint = env.win_mint;
    let recipient = env.actors.recipients[0].pubkey();
    let decoy = decoy_vault(&mut env);
    // The snapshot AFTER the decoy account was prepared.
    let before = snapshot(&env, id, &recipient);

    let ix = cancel_vesting_ix_with_slots(
        owner.pubkey(),
        config_pda(),
        vesting_pda(id),
        decoy,
        beneficiary_token,
        rent_collector,
        vault_authority_pda(),
        win_mint,
        id,
    );
    let r = send_as(&mut env, ix, &owner);
    // The code is pinned by the first execution: `ConstraintSeeds` (2006). Here the slot
    // resolution order does not affect the diagnosis - the account's mint and authority
    // are correct, so exactly the seed check fails rather than `token::mint` /
    // `token::authority` (unlike substituting `vault_authority` itself in group 1, where
    // `ConstraintTokenOwner` fails first).
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintSeeds)),
        "matching the mint and the authority is not enough: the cancellation's vault is also bound by seeds: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &recipient),
        before,
        "the activity is intact: the rejection touched neither the record nor its vault"
    );
    assert!(
        env.account_exists(&decoy),
        "the decoy account was not closed - otherwise its rent would have gone to the rent_collector"
    );
    assert_eq!(
        env.token_balance(&decoy),
        REFERENCE,
        "the decoy account lost not a single unit"
    );
    assert_eq!(
        env.token_balance(&beneficiary_token),
        0,
        "not a single unit reached the withdrawal account"
    );

    // A POSITIVE: the canonical vault is drained and closed with the same call.
    env.expire_blockhash();
    let ix = cancel_vesting_ix(
        owner.pubkey(),
        id,
        beneficiary_token,
        rent_collector,
        win_mint,
    );
    send_as(&mut env, ix, &owner).expect("the canonical vault in the slot is accepted");
    assert!(!env.vault_exists(id), "the vault is closed");
    assert!(!env.vesting_exists(id), "the record is closed");
    assert_eq!(
        env.token_balance(&beneficiary_token),
        REFERENCE,
        "exactly the activity's vault was drained"
    );
    assert_eq!(
        env.token_balance(&decoy),
        REFERENCE,
        "the positive did not touch the decoy account either"
    );
}

#[test]
fn close_rejects_non_canonical_token_account_in_vault_slot() {
    let (mut env, id, beneficiary_token, _tge) = setup_with_beneficiary();
    let owner = env.actors.owner.insecure_clone();
    let rent_collector = env.actors.rent_collector.pubkey();
    let win_mint = env.win_mint;
    let recipient = env.actors.recipients[0].pubkey();
    let decoy = decoy_vault(&mut env);
    // The activity was brought to a full draw-down by a forge: `close`'s gate is
    // `claimed >= reference_amount`, but the vault must stay funded, or draining the decoy
    // account would be unobservable (a terminal claim would empty the vault too).
    env.forge_claimed(id, REFERENCE);
    let before = snapshot(&env, id, &recipient);

    let ix = close_vesting_ix_with_slots(
        owner.pubkey(),
        config_pda(),
        vesting_pda(id),
        decoy,
        beneficiary_token,
        rent_collector,
        vault_authority_pda(),
        win_mint,
        id,
    );
    let r = send_as(&mut env, ix, &owner);
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintSeeds)),
        "matching the mint and the authority is not enough: the close's vault is also bound by seeds: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &recipient),
        before,
        "the activity is intact: the rejection touched neither the record nor its vault"
    );
    assert!(
        env.account_exists(&decoy),
        "the decoy account was not closed - otherwise its rent would have gone to the rent_collector"
    );
    assert_eq!(
        env.token_balance(&decoy),
        REFERENCE,
        "the decoy account lost not a single unit"
    );
    assert_eq!(
        env.token_balance(&beneficiary_token),
        0,
        "not a single unit reached the withdrawal account"
    );

    // A POSITIVE: the canonical vault is drained and closed with the same call.
    env.expire_blockhash();
    let ix = close_vesting_ix(
        owner.pubkey(),
        id,
        beneficiary_token,
        rent_collector,
        win_mint,
    );
    send_as(&mut env, ix, &owner).expect("the canonical vault in the slot is accepted");
    assert!(!env.vault_exists(id), "the vault is closed");
    assert!(!env.vesting_exists(id), "the record is closed");
    assert_eq!(
        env.token_balance(&beneficiary_token),
        REFERENCE,
        "exactly the activity's vault was drained"
    );
    assert_eq!(
        env.token_balance(&decoy),
        REFERENCE,
        "the positive did not touch the decoy account either"
    );
}

// ===================================================================
// Group 6 - an account at a NON-canonical address of the same seeds
//
// A single set of seeds has many valid addresses: `find_program_address` returns the
// highest suitable bump, but every lower bump yields its own, equally valid address. A
// PDA check must accept exactly the canonical one - otherwise two accounts would live
// under one set of seeds: a second config with its own roles and a second activity
// record with its own `claimed` against a single vault.
//
// The decoy account is a copy of a live one at a non-canonical address: owned by the
// Vesting program, with a genuine discriminator and the same fields.
//
// The two tests on an activity RECORD put the decoy address's bump into the copy's
// last byte ([`TestEnv::forge_vesting_copy`]): the copy is then self-consistent
// (address ↔ stored bump), and only recomputing the canonical bump can reject it.
// This was verified by mutation: with a cached bump in the record and a check against
// it, both tests turn red - and the instruction does not merely fail to reject, it
// executes on the decoy record: claim releases tokens and cancel drains the vault and
// closes the activity.
//
// The two tests on the CONFIG substitute an unedited copy: the config's last byte is
// `vault_authority_bump`, which the separate `vault_authority` slot is checked
// against, and overwriting it would break that check instead of checking the config's
// address. So those two verify the property "a config outside the canonical address is
// rejected" but do NOT catch a returned bump cache: the copy carries the canonical
// bump, a check against it derives the canonical address, that address differs from
// the decoy one, and the rejection comes anyway - verified by a run with
// `config.bump` restored, where both stayed green. A returned cache is caught by the
// two tests on the activity record; both accounts' address check uses the very same
// attribute.
// ===================================================================

#[test]
fn claim_rejects_vesting_record_at_non_canonical_address() {
    let (mut env, id, tge_ts) = setup();
    let recipient = env.actors.recipients[0].insecure_clone();
    let win_mint = env.win_mint;
    env.warp_to_day(tge_ts, DAY_151);

    let (forged, bump) = non_canonical_pda(&[b"vesting", &id.to_le_bytes()]);
    env.forge_vesting_copy(id, &forged, bump);
    let forged_before = env
        .svm
        .get_account(&forged)
        .expect("the decoy record was written")
        .data;
    let before = snapshot(&env, id, &recipient.pubkey());

    let ix = claim_ix_with_slots(
        recipient.pubkey(),
        config_pda(),
        forged,
        vault_pda(id),
        recipient.pubkey(),
        recipient_ata(&recipient.pubkey(), &win_mint),
        vault_authority_pda(),
        win_mint,
        spl_token_interface::id(),
        id,
    );
    let r = send_as(&mut env, ix, &recipient);
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintSeeds)),
        "an activity record outside the canonical PDA → ConstraintSeeds: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &recipient.pubkey()),
        before,
        "the rejection touched neither the canonical record nor the vault"
    );
    assert_eq!(
        env.svm
            .get_account(&forged)
            .expect("the decoy record is in place")
            .data,
        forged_before,
        "the rejection did not accrue `claimed` on the decoy record"
    );
    assert!(
        !env.recipient_ata_exists(&recipient.pubkey()),
        "the recipient's ATA was not created: the revert rolled back init_if_needed too"
    );

    // A POSITIVE: the canonical record is accepted by the same call.
    env.expire_blockhash();
    claim_by_recipient(&mut env, id).expect("the canonical activity record is accepted");
    assert_eq!(env.vesting(id).claimed, V2_VESTED_DAY_151);
}

#[test]
fn cancel_rejects_vesting_record_at_non_canonical_address() {
    let (mut env, id, beneficiary_token, _tge) = setup_with_beneficiary();
    let owner = env.actors.owner.insecure_clone();
    let rent_collector = env.actors.rent_collector.pubkey();
    let win_mint = env.win_mint;
    let recipient = env.actors.recipients[0].pubkey();

    let (forged, bump) = non_canonical_pda(&[b"vesting", &id.to_le_bytes()]);
    env.forge_vesting_copy(id, &forged, bump);
    let before = snapshot(&env, id, &recipient);

    let ix = cancel_vesting_ix_with_slots(
        owner.pubkey(),
        config_pda(),
        forged,
        vault_pda(id),
        beneficiary_token,
        rent_collector,
        vault_authority_pda(),
        win_mint,
        id,
    );
    let r = send_as(&mut env, ix, &owner);
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintSeeds)),
        "an activity record outside the canonical PDA → ConstraintSeeds: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &recipient),
        before,
        "the rejection touched neither the canonical record nor the vault"
    );
    assert!(
        env.account_exists(&forged),
        "the decoy record was not closed - otherwise its rent would have gone to the rent_collector"
    );
    assert_eq!(
        env.token_balance(&beneficiary_token),
        0,
        "not a single unit reached the beneficiary's account"
    );

    // A POSITIVE: the canonical record is drained and closed by the same call.
    env.expire_blockhash();
    let ix = cancel_vesting_ix(
        owner.pubkey(),
        id,
        beneficiary_token,
        rent_collector,
        win_mint,
    );
    send_as(&mut env, ix, &owner).expect("the canonical activity record is accepted");
    assert!(!env.vault_exists(id), "the vault is closed");
    assert!(!env.vesting_exists(id), "the record is closed");
    assert_eq!(
        env.token_balance(&beneficiary_token),
        REFERENCE,
        "the vault's remainder went to the beneficiary"
    );
}

#[test]
fn claim_rejects_config_at_non_canonical_address() {
    let (mut env, id, tge_ts) = setup();
    let recipient = env.actors.recipients[0].insecure_clone();
    let win_mint = env.win_mint;
    env.warp_to_day(tge_ts, DAY_151);

    // The config's copy is unedited: the same roles, the same withdrawal account, the same
    // `vault_authority_bump`. So the `vault_authority` slot (whose bump comes from the
    // config) passes, every check in the body would pass too, and exactly one check can
    // fail - the config's own address.
    let (forged, _) = non_canonical_pda(&[b"config"]);
    let live = env.vesting_config();
    env.forge_config(&forged, &live);
    let before = snapshot(&env, id, &recipient.pubkey());

    let ix = claim_ix_with_slots(
        recipient.pubkey(),
        forged,
        vesting_pda(id),
        vault_pda(id),
        recipient.pubkey(),
        recipient_ata(&recipient.pubkey(), &win_mint),
        vault_authority_pda(),
        win_mint,
        spl_token_interface::id(),
        id,
    );
    let r = send_as(&mut env, ix, &recipient);
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintSeeds)),
        "a config outside the canonical PDA → ConstraintSeeds: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &recipient.pubkey()),
        before,
        "the rejection touched neither the accounting nor the vault"
    );

    // A POSITIVE: the canonical config is accepted by the same call.
    env.expire_blockhash();
    claim_by_recipient(&mut env, id).expect("the canonical config is accepted");
    assert_eq!(env.vesting(id).claimed, V2_VESTED_DAY_151);
}

#[test]
fn cancel_rejects_config_at_non_canonical_address() {
    let (mut env, id, beneficiary_token, _tge) = setup_with_beneficiary();
    let owner = env.actors.owner.insecure_clone();
    let rent_collector = env.actors.rent_collector.pubkey();
    let win_mint = env.win_mint;
    let recipient = env.actors.recipients[0].pubkey();

    // The copy is unedited - for the same reason as in the claim: the only check that can
    // fail is the config's address.
    let (forged, _) = non_canonical_pda(&[b"config"]);
    let live = env.vesting_config();
    env.forge_config(&forged, &live);
    let before = snapshot(&env, id, &recipient);

    let ix = cancel_vesting_ix_with_slots(
        owner.pubkey(),
        forged,
        vesting_pda(id),
        vault_pda(id),
        beneficiary_token,
        rent_collector,
        vault_authority_pda(),
        win_mint,
        id,
    );
    let r = send_as(&mut env, ix, &owner);
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintSeeds)),
        "a config outside the canonical PDA → ConstraintSeeds: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &recipient),
        before,
        "the rejection touched neither the record nor the vault"
    );
    assert_eq!(
        env.token_balance(&beneficiary_token),
        0,
        "not a single unit reached the beneficiary's account"
    );

    // A POSITIVE: the canonical config drains and closes with the same call.
    env.expire_blockhash();
    let ix = cancel_vesting_ix(
        owner.pubkey(),
        id,
        beneficiary_token,
        rent_collector,
        win_mint,
    );
    send_as(&mut env, ix, &owner).expect("the canonical config is accepted");
    assert!(!env.vault_exists(id), "the vault is closed");
    assert!(!env.vesting_exists(id), "the record is closed");
    assert_eq!(
        env.token_balance(&beneficiary_token),
        REFERENCE,
        "the vault's remainder went to the beneficiary"
    );
}
