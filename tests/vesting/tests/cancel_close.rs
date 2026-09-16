//! Cancelling and closing an activity (Arch §9.7, cards I7/I8): `cancel_vesting` and
//! `close_vesting`. The two instructions share one
//! context and one `drain_and_close` mechanism and differ only in their gate, which is
//! why the tests live side by side: the addressee substitutions, the rent and the
//! replay guard are common to both, while each is checked on what is its own.
//!
//! Why the tests are laid out the way they are.
//!
//! 1. The drain→close order is proven by the EXISTENCE of a positive with a non-empty
//!    vault: SPL Token refuses to close a non-native token account with a
//!    non-zero balance, so the reverse order (close, then transfer) would fail exactly
//!    that test. There is - and can be - no separate "order test": the order is
//!    observable only through the fact of success.
//! 2. An empty vault is the paired positive of the same mechanism: the
//!    `if amount > 0` branch must be a SKIP rather than a revert. The beneficiary's
//!    balance in that case is non-zero in advance - otherwise "did not change" and
//!    "was zeroed" would be indistinguishable. The skip itself is unobservable by
//!    balances (SPL Token accepts a `Transfer` of zero units and moves no balance), so
//!    both positives check the LIST of CPIs to SPL Token: with an empty vault it holds
//!    only `CloseAccount`, with a non-empty one `Transfer` and `CloseAccount`
//!    ([`spl_token_cpi_tags`]).
//! 3. The rent is computed as the lamports of both accounts TAKEN BEFORE the call
//!    rather than as a literal: `rent_collector` receives it by two routes (the
//!    vault's `CloseAccount` CPI and Anchor's `close` of the record), and asserting on
//!    the sum proves that both routes reached one and the same addressee.
//! 4. Every negative takes a FULL snapshot of the observable state ([`Snapshot`])
//!    before and after: a rejection must touch neither the vault, nor the record, nor
//!    the beneficiary's account, nor `rent_collector`'s lamports, nor the activity
//!    counter. A partial effect here is more dangerous than the rejection itself - the
//!    vault already emptied while the record is alive.
//! 5. In the addressee negatives the vault is NON-EMPTY: on an empty one "the vault
//!    is intact" would be checked degenerately (there is nothing to lose).
//! 6. The "`beneficiary_token` == the vault" case asserts NOT the domain
//!    `InvalidBeneficiary` but `ConstraintDuplicateMutableAccount` (2040): Anchor 1.0.2
//!    rejects duplicate mutable slots ahead of the user constraints. Invariant I12
//!    still holds - structurally, by the framework's means. The details are in the
//!    test's own comment.
//! 7. `activity_count` is checked for immutability in every cancellation case (I8): the
//!    counter is an identifier allocator, and rolling it back would make a closed
//!    activity's seeds reusable. A test proves that by behaviour - the id after
//!    cancelling activity zero is 1, not 0.
//! 8. The two withdrawal addressees answer with DIFFERENT codes, and the negatives are
//!    split accordingly. `InvalidBeneficiary` serves the `beneficiary_token` slot (the
//!    `owner == config.withdrawal_authority` condition) - it is reached by ANOTHER
//!    activity's vault, by `vault_authority`'s own ATA and by an outside wallet's ATA;
//!    the first two addressees would take the remainder into the program's custody,
//!    from which nothing can withdraw it - the program has no instruction that takes
//!    funds off an arbitrary `vault_authority` account. `InvalidRentCollector` serves
//!    the `rent_collector` slot (the `address == config.withdrawal_authority`
//!    condition). The distinct codes are needed operationally: the rejection shows
//!    which of the two addressees was submitted wrong (every error
//!    variant has a test that reaches it).
//! 9. The cancellation's role negative puts a foreign key BOTH into the `owner` slot
//!    AND into the signers (otherwise the rejection would come from the missing
//!    signature rather than from the role), while the environment's `payer` acts as the
//!    fee payer: otherwise the fee would move the actor's own lamports and, on the
//!    `rent_collector` iteration, the "the state is intact" snapshot would jitter. It
//!    checks `has_one` but not the slot's type: `has_one` compares addresses and
//!    requires no signature. So both instructions have a separate case "the Owner key
//!    is correct, only the signature flag was cleared" - without it, changing the
//!    slot's type to `UncheckedAccount` would let an outsider destroy an activity
//!    before TGE and force-close a drawn-down one.
//!
//! 10. `close_vesting`'s gate is checked BEFORE TGE as well: there it must
//!     answer `NotFullyClaimed` rather than `ScheduleStarted` - the instruction has no
//!     time gate and none must appear, or a drawn-down activity could not be closed
//!     after TGE.
//! 11. All three conditions of the `beneficiary_token` slot are checked on both
//!     instructions rather than only on the cancellation. Their context is shared
//!     today, so the codes coinciding is a consequence of the layout; the test holds
//!     the boundary by behaviour in case the contexts are ever split apart: a close
//!     takes the vault's remainder away exactly as a cancellation does.
//!
//! ===== DELIBERATELY DEFERRED =====
//! - Substituting `vault_authority` and a foreign `token_mint` in the cancellation and
//!   close contexts - the substitution matrix.

use anchor_lang::error::ErrorCode;
use litesvm::types::TransactionResult;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use vesting::errors::VestingError;
use vesting::events::{VestingCancelled, VestingClosed, VestingCreated};
use vesting_test_common::vesting_expected::vesting::{
    ECOSYSTEM_FULL_RELEASE_DAY, V2_REFERENCE, V5_CLAIMABLE_DAY_541, V5_CLAIMED_AFTER_DAY_151,
    V5_CLAIMED_AFTER_DAY_541,
};
use vesting_test_common::{
    assert_funded_ix, cancel_vesting_ix, cancel_vesting_ix_with_slots, claim_ix, close_vesting_ix,
    close_vesting_ix_with_slots, config_pda, create_vesting_ix, custom_error_code, decode_event,
    err, inner_instructions, recipient_ata, set_recipient_ix, vault_authority_pda, vault_pda,
    vesting_pda, without_signer, ActivitySpec, TestEnv, DEFAULT_TGE_OFFSET,
};

/// The activity's reference amount: the V2 bucket of §10.5 (10⁸ $1WIN (100,000,000)
/// in token units).
const REFERENCE: u64 = V2_REFERENCE;

/// How much sits in the vault by the time of the cancellation. Deliberately NOT equal
/// to `REFERENCE`: the mechanism must withdraw the vault's actual balance rather than
/// the activity's reference amount.
const VAULT_FUNDED: u64 = 7_777_000_000_000;

/// How much sits in the SECOND activity's vault in the addressee-binding negatives.
/// It differs from [`VAULT_FUNDED`] so that swapped vaults differ by balance rather
/// than by address alone.
const OTHER_VAULT_FUNDED: u64 = 1_234_500_000_000;

/// The balance sitting on the beneficiary's account BEFORE the cancellation.
/// Non-zero, so that in the empty-vault case "the balance did not change"
/// differs from "the balance was zeroed".
const BENEFICIARY_PREFUNDED: u64 = 4_242;

/// The tag of the `Transfer` instruction in SPL Token's binary interface (the data's
/// first byte). A literal in the test: taking it from the builder would mean checking
/// the encoding against itself.
const SPL_TAG_TRANSFER: u8 = 3;

/// The tag of the `CloseAccount` instruction in the same interface.
const SPL_TAG_CLOSE_ACCOUNT: u8 = 9;

// We return the native litesvm TransactionResult as is - just like `send_ixs`.
#[allow(clippy::result_large_err)]
fn send(env: &mut TestEnv, ix: Instruction, signer: &Keypair) -> TransactionResult {
    env.send_ixs(&[ix], &signer.pubkey(), &[signer])
}

/// Sends the cancellation on behalf of `actor`: its key goes BOTH into the `owner`
/// slot AND into the signers. The fee payer is the environment's `payer`, so that the
/// fee does not move the actors' lamports (among them `rent_collector`, whose lamports
/// are part of [`Snapshot`]).
#[allow(clippy::result_large_err)]
fn cancel_as(
    env: &mut TestEnv,
    actor: &Keypair,
    activity_id: u64,
    beneficiary_token: Pubkey,
) -> TransactionResult {
    let ix = cancel_vesting_ix(
        actor.pubkey(),
        activity_id,
        beneficiary_token,
        env.actors.rent_collector.pubkey(),
        env.win_mint,
    );
    let payer = env.payer.insecure_clone();
    env.send_ixs(&[ix], &payer.pubkey(), &[&payer, actor])
}

/// The keys an Owner instruction must reject: every actor of the environment except
/// Owner itself. Admin stands in the list on equal terms: moving tokens and rent
/// remains Owner's.
fn wrong_owners(env: &TestEnv) -> Vec<(&'static str, Keypair)> {
    vec![
        ("admin", env.actors.admin.insecure_clone()),
        ("operator", env.actors.operator.insecure_clone()),
        ("recipient", env.actors.recipients[0].insecure_clone()),
        ("beneficiary", env.actors.beneficiary.insecure_clone()),
        ("rent_collector", env.actors.rent_collector.insecure_clone()),
        ("stranger", env.actors.stranger.insecure_clone()),
    ]
}

/// The tags of the CPI calls to SPL Token a successful transaction made, in execution
/// order. Anchor's event self-CPI (`emit_cpi!`) goes to the Vesting program itself and
/// does not appear in the list.
fn spl_token_cpi_tags(result: &TransactionResult, ix: &Instruction, payer: &Pubkey) -> Vec<u8> {
    inner_instructions(result, std::slice::from_ref(ix), payer)
        .into_iter()
        .filter(|(program_id, _)| *program_id == spl_token_interface::id())
        .map(|(_, data)| {
            *data
                .first()
                .expect("SPL Token instruction data is never empty")
        })
        .collect()
}

/// An environment with a config (TGE in a month), a created activity, its vault
/// holding `vault_amount`, and the canonical `beneficiary_token` - the ATA of the
/// single withdrawal recipient W (= the `rent_collector` actor,
/// `config.withdrawal_authority`) under $1WIN with [`BENEFICIARY_PREFUNDED`] on it. The
/// binding of §3: `beneficiary_token` must belong to W and `rent_collector` must be W -
/// both are `rent_collector`. Returns `(env, activity_id, beneficiary_token, tge_ts)`.
fn setup(vault_amount: u64) -> (TestEnv, u64, Pubkey, i64) {
    let mut env = TestEnv::new();
    let params = env.init_default(DEFAULT_TGE_OFFSET);
    let activity_id = env.create_activity(&ActivitySpec::valid(REFERENCE));
    if vault_amount > 0 {
        env.fund_vault(activity_id, vault_amount);
    }

    let withdrawal = env.actors.rent_collector.pubkey();
    let win_mint = env.win_mint;
    let beneficiary_token = env.create_ata(&withdrawal, &win_mint);
    env.mint_to(&win_mint, &beneficiary_token, BENEFICIARY_PREFUNDED);

    (env, activity_id, beneficiary_token, params.tge_ts)
}

/// The canonical cancellation call: Owner signs, the remainder goes to
/// `beneficiary_token` and the rent to the `rent_collector` actor.
fn cancel_ix(env: &TestEnv, activity_id: u64, beneficiary_token: Pubkey) -> Instruction {
    cancel_vesting_ix(
        env.actors.owner.pubkey(),
        activity_id,
        beneficiary_token,
        env.actors.rent_collector.pubkey(),
        env.win_mint,
    )
}

/// A full snapshot of the observable state around the activity. Comparing the
/// snapshots before and after is the "the rejection left no partial effect" assertion
/// in a single line.
#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    vesting_exists: bool,
    vault_exists: bool,
    vault_balance: u64,
    /// Every field of the `Vesting` record as a tuple: the type does not derive `PartialEq`.
    vesting_fields: (Pubkey, u64, u64, u16, u16, u16),
    beneficiary_balance: u64,
    rent_collector_lamports: u64,
    activity_count: u64,
}

/// It panics if the record or the vault is absent: the snapshot is taken where the
/// activity must be alive, and "the account is gone" must never be read silently as
/// "empty".
fn snapshot(env: &TestEnv, activity_id: u64, beneficiary_token: &Pubkey) -> Snapshot {
    let v = env.vesting(activity_id);
    Snapshot {
        vesting_exists: env.vesting_exists(activity_id),
        vault_exists: env.vault_exists(activity_id),
        vault_balance: env.vault_balance(activity_id),
        vesting_fields: (
            v.recipient,
            v.reference_amount,
            v.claimed,
            v.tge_bps,
            v.monthly_bps,
            v.cliff_months,
        ),
        beneficiary_balance: env.token_balance(beneficiary_token),
        rent_collector_lamports: env.lamports_of(&env.actors.rent_collector.pubkey()),
        activity_count: env.vesting_config().activity_count,
    }
}

// ===================================================================
// the positive, a non-empty vault
// ===================================================================

#[test]
fn cancel_drains_vault_closes_both_accounts_and_returns_rent() {
    let (mut env, id, beneficiary_token, _tge) = setup(VAULT_FUNDED);
    let owner = env.actors.owner.insecure_clone();
    let rent_collector = env.actors.rent_collector.pubkey();

    // The rent of both accounts is their actual lamports BEFORE the call: the expectation
    // is not a literal but what was really debited from Owner at creation time.
    let vesting_rent = env.lamports_of(&vesting_pda(id));
    let vault_rent = env.lamports_of(&vault_pda(id));
    let rent_collector_before = env.lamports_of(&rent_collector);
    assert_eq!(
        env.vesting_config().activity_count,
        1,
        "precondition: exactly one activity was created"
    );

    let ix = cancel_ix(&env, id, beneficiary_token);
    let r = send(&mut env, ix.clone(), &owner);
    // This call's success IS the proof of the drain→close order: SPL Token does not close
    // a non-native token account with a non-zero balance, so the reverse order (close
    // before the transfer) would fail the transaction here.
    r.as_ref()
        .expect("cancelling a non-empty activity before TGE passes");

    // The paired half of the skip assertion: with a non-empty vault the transfer
    // MUST be in the CPI list, and precisely before the close.
    assert_eq!(
        spl_token_cpi_tags(&r, &ix, &owner.pubkey()),
        vec![SPL_TAG_TRANSFER, SPL_TAG_CLOSE_ACCOUNT],
        "a non-empty vault: Transfer, then CloseAccount"
    );
    assert_eq!(
        env.token_balance(&beneficiary_token),
        BENEFICIARY_PREFUNDED + VAULT_FUNDED,
        "the vault's whole balance went to beneficiary_token"
    );
    assert!(!env.vault_exists(id), "the vault is closed");
    assert!(!env.vesting_exists(id), "the Vesting record is closed");
    assert_eq!(
        env.lamports_of(&rent_collector) - rent_collector_before,
        vesting_rent + vault_rent,
        "the rent of BOTH accounts arrived at rent_collector (the CloseAccount CPI + Anchor's close)"
    );
    assert_eq!(
        env.vesting_config().activity_count,
        1,
        "activity_count is not decremented: it is an id allocator rather than a counter of live records (I8)"
    );

    // [#5] The event carries the drained remainder and both addressees (the payload is an
    // observable effect, tests-bind-to-real-code): a non-empty vault → `amount` == its
    // balance BEFORE the call.
    let ev: VestingCancelled = decode_event(&r).expect("cancel_vesting emits VestingCancelled");
    assert_eq!(
        ev.activity_id, id,
        "the event carries the cancelled activity's id"
    );
    assert_eq!(
        ev.amount, VAULT_FUNDED,
        "the event: amount == the drained remainder (the vault's balance before the call)"
    );
    assert_eq!(
        ev.beneficiary_token, beneficiary_token,
        "the event: the remainder's addressee"
    );
    assert_eq!(
        ev.rent_collector, rent_collector,
        "the event: the rent recipient"
    );
}

// ===================================================================
// the positive, an empty vault
// ===================================================================

#[test]
fn cancel_with_empty_vault_skips_transfer_and_still_closes() {
    let (mut env, id, beneficiary_token, _tge) = setup(0);
    let owner = env.actors.owner.insecure_clone();
    let rent_collector = env.actors.rent_collector.pubkey();

    assert_eq!(env.vault_balance(id), 0, "precondition: the vault is empty");
    let vesting_rent = env.lamports_of(&vesting_pda(id));
    let vault_rent = env.lamports_of(&vault_pda(id));
    let rent_collector_before = env.lamports_of(&rent_collector);

    let ix = cancel_ix(&env, id, beneficiary_token);
    let r = send(&mut env, ix.clone(), &owner);
    // `if amount > 0` is a SKIP of the step rather than a revert (Arch §9.7).
    r.as_ref()
        .expect("a cancellation with an empty vault passes without an error");

    // The skip itself is observable ONLY here: SPL Token accepts a `Transfer` of zero
    // units and moves no balance, so removing `if amount > 0` from `drain_and_close`
    // would not change a single balance - only the CPI list would change. Asserting on
    // the list is what makes the branch verifiable.
    assert_eq!(
        spl_token_cpi_tags(&r, &ix, &owner.pubkey()),
        vec![SPL_TAG_CLOSE_ACCOUNT],
        "an empty vault: there is no Transfer instruction at all, only CloseAccount"
    );
    assert_eq!(
        env.token_balance(&beneficiary_token),
        BENEFICIARY_PREFUNDED,
        "the transfer was skipped: beneficiary_token's balance did not change (and was not zeroed)"
    );
    assert!(
        !env.vault_exists(id),
        "an empty vault is closed all the same"
    );
    assert!(!env.vesting_exists(id), "the Vesting record is closed");
    assert_eq!(
        env.lamports_of(&rent_collector) - rent_collector_before,
        vesting_rent + vault_rent,
        "the rent of both accounts arrived at rent_collector"
    );

    // [#5] An empty vault → `amount == 0` (there was nothing to transfer) - the paired
    // half of the non-empty case above, where `amount == VAULT_FUNDED`.
    let ev: VestingCancelled = decode_event(&r).expect("cancel_vesting emits VestingCancelled");
    assert_eq!(ev.activity_id, id);
    assert_eq!(ev.amount, 0, "the event: an empty vault drains zero");
    assert_eq!(
        ev.beneficiary_token, beneficiary_token,
        "the event: the remainder's addressee"
    );
    assert_eq!(
        ev.rent_collector, rent_collector,
        "the event: the rent recipient"
    );
}

// ===================================================================
// beneficiary_token == the vault
// ===================================================================

#[test]
fn cancel_rejects_beneficiary_token_equal_to_vault() {
    let (mut env, id, beneficiary_token, _tge) = setup(VAULT_FUNDED);
    let owner = env.actors.owner.insecure_clone();
    let before = snapshot(&env, id, &beneficiary_token);

    // Invariant I12 "the remainder does not go back into the vault" holds, but the
    // rejection's code is NOT the domain `InvalidBeneficiary`.
    //
    // Anchor 1.0.2 generates its own duplicate check across mutable slots whose types
    // serialize on the way out (`Account`, `InterfaceAccount`, `LazyAccount`): it stands
    // in `try_accounts` BEFORE the user constraints and, when `vault` and
    // `beneficiary_token` coincide, answers `ConstraintDuplicateMutableAccount` (2040).
    // So BOTH of close.rs's conditions that would have rejected this input
    // (`beneficiary_token.key() != vault.key()` and
    // `beneficiary_token.owner != vault_authority` - the vault belongs to precisely that
    // authority) are unreachable on it through the public interface. The first of them
    // remains a safeguard against a change of the slot's type or the removal of `mut` -
    // like `recipient != Pubkey::default()` in `create_vesting`; the domain
    // `InvalidBeneficiary` is reached by other inputs: ANOTHER activity's vault and
    // `vault_authority`'s own ATA - the tests below. What must be asserted here is the
    // genuinely observable code (the divergence from the spec was handed over in
    // Arch §9.7 / I12).
    let ix = cancel_vesting_ix(
        owner.pubkey(),
        id,
        vault_pda(id),
        env.actors.rent_collector.pubkey(),
        env.win_mint,
    );
    let r = send(&mut env, ix, &owner);
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintDuplicateMutableAccount)),
        "beneficiary_token == the vault → ConstraintDuplicateMutableAccount (2040), the domain InvalidBeneficiary is unreachable: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &beneficiary_token),
        before,
        "the rejection touched neither the vault, nor the record, nor the accounts"
    );

    // A POSITIVE in the same config: with a correct addressee the cancellation passes.
    env.expire_blockhash();
    let ix = cancel_ix(&env, id, beneficiary_token);
    send(&mut env, ix, &owner).expect("a correct beneficiary_token is accepted");
    assert_eq!(
        env.token_balance(&beneficiary_token),
        BENEFICIARY_PREFUNDED + VAULT_FUNDED
    );
}

// ===================================================================
// [I12] - a beneficiary_token under vault_authority's custody
// ===================================================================

#[test]
fn cancel_rejects_vault_of_another_activity_as_beneficiary_token() {
    let (mut env, id, beneficiary_token, _tge) = setup(VAULT_FUNDED);
    let owner = env.actors.owner.insecure_clone();

    // A second activity with its OWN funded vault. Such a submission is not a duplicate
    // of mutable slots (the addresses differ), so Anchor passes it through to the user
    // conditions - and it is `beneficiary_token.owner != vault_authority` that rejects
    // it.
    let other_recipient = env.actors.recipients[1].pubkey();
    let other_id = env.create_activity_for(&other_recipient, &ActivitySpec::valid(REFERENCE));
    env.fund_vault(other_id, OTHER_VAULT_FUNDED);
    assert_ne!(
        vault_pda(other_id),
        vault_pda(id),
        "the vaults are different"
    );

    let before = snapshot(&env, id, &beneficiary_token);
    let before_other = snapshot(&env, other_id, &beneficiary_token);

    let ix = cancel_vesting_ix(
        owner.pubkey(),
        id,
        vault_pda(other_id),
        env.actors.rent_collector.pubkey(),
        env.win_mint,
    );
    let r = send(&mut env, ix, &owner);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InvalidBeneficiary)),
        "another activity's vault in the beneficiary_token slot → InvalidBeneficiary: the remainder would go under the same vault_authority custody, and the program has no instruction that takes funds off a foreign vault - the money would be locked forever: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &beneficiary_token),
        before,
        "the activity being cancelled is intact"
    );
    assert_eq!(
        snapshot(&env, other_id, &beneficiary_token),
        before_other,
        "the second activity is intact and its vault received not a single unit"
    );
    assert_eq!(
        env.vault_balance(other_id),
        OTHER_VAULT_FUNDED,
        "the second vault's balance did not change"
    );

    // A POSITIVE in the same environment: an account outside the program's custody is accepted.
    env.expire_blockhash();
    let ix = cancel_ix(&env, id, beneficiary_token);
    send(&mut env, ix, &owner).expect("the beneficiary's ATA is accepted");
    assert_eq!(
        env.token_balance(&beneficiary_token),
        BENEFICIARY_PREFUNDED + VAULT_FUNDED,
        "the remainder went to an account outside vault_authority's custody"
    );
    assert_eq!(
        env.vault_balance(other_id),
        OTHER_VAULT_FUNDED,
        "the positive did not touch the foreign vault either"
    );
}

#[test]
fn cancel_rejects_vault_authority_ata_as_beneficiary_token() {
    let (mut env, id, beneficiary_token, _tge) = setup(VAULT_FUNDED);
    let owner = env.actors.owner.insecure_clone();
    let win_mint = env.win_mint;

    // The ATA is created the honest way: the ATA program does not ask for the owner's
    // signature, so anyone can create such an account under `vault_authority` - an
    // attacker included, in advance and without Vesting's involvement.
    let authority_ata = env.create_ata(&vault_authority_pda(), &win_mint);
    assert_eq!(
        env.token_account_mint_owner(&authority_ata),
        (win_mint, vault_authority_pda()),
        "precondition: an account of the correct mint under vault_authority's custody"
    );
    let before = snapshot(&env, id, &beneficiary_token);

    let ix = cancel_vesting_ix(
        owner.pubkey(),
        id,
        authority_ata,
        env.actors.rent_collector.pubkey(),
        win_mint,
    );
    let r = send(&mut env, ix, &owner);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InvalidBeneficiary)),
        "vault_authority's own ATA in the beneficiary_token slot → InvalidBeneficiary: the account is not an activity vault, but the custody is the same and nothing can withdraw from it: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &beneficiary_token),
        before,
        "the rejection did not touch the state"
    );
    assert_eq!(
        env.token_balance(&authority_ata),
        0,
        "nothing went to the account under the program's custody"
    );

    // A POSITIVE in the same environment.
    env.expire_blockhash();
    let ix = cancel_ix(&env, id, beneficiary_token);
    send(&mut env, ix, &owner).expect("an account outside vault_authority's custody is accepted");
    assert_eq!(
        env.token_balance(&beneficiary_token),
        BENEFICIARY_PREFUNDED + VAULT_FUNDED
    );
    assert_eq!(env.token_balance(&authority_ata), 0);
}

// ===================================================================
// five cancellation negatives
// ===================================================================

#[test]
fn cancel_rejects_beneficiary_token_of_foreign_mint() {
    let (mut env, id, beneficiary_token, _tge) = setup(VAULT_FUNDED);
    let owner = env.actors.owner.insecure_clone();
    let beneficiary = env.actors.beneficiary.pubkey();
    let foreign_mint = env.foreign_mint;
    // The mint is "foreign" but has the same 8 decimals: the rejection must come from the
    // mint's binding rather than from a decimals mismatch.
    let foreign_token = env.create_ata(&beneficiary, &foreign_mint);
    let before = snapshot(&env, id, &beneficiary_token);

    let ix = cancel_vesting_ix(
        owner.pubkey(),
        id,
        foreign_token,
        env.actors.rent_collector.pubkey(),
        env.win_mint,
    );
    let r = send(&mut env, ix, &owner);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InvalidMint)),
        "a beneficiary_token with a foreign mint → the domain InvalidMint (I12) rather than a generic Anchor error: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &beneficiary_token),
        before,
        "the rejection did not touch the state"
    );
    assert_eq!(
        env.token_balance(&foreign_token),
        0,
        "nothing went to the foreign account"
    );

    // A POSITIVE in the same config.
    env.expire_blockhash();
    let ix = cancel_ix(&env, id, beneficiary_token);
    send(&mut env, ix, &owner).expect("an ATA of the correct mint is accepted");
    assert_eq!(
        env.token_balance(&beneficiary_token),
        BENEFICIARY_PREFUNDED + VAULT_FUNDED
    );
}

#[test]
fn cancel_rejects_token_account_as_rent_collector() {
    let (mut env, id, beneficiary_token, _tge) = setup(VAULT_FUNDED);
    let owner = env.actors.owner.insecure_clone();
    let before = snapshot(&env, id, &beneficiary_token);

    // `rent_collector: SystemAccount` structurally excludes a token account (I12): the
    // rejection happens during account RESOLUTION, before the instruction's body.
    let ix = cancel_vesting_ix(
        owner.pubkey(),
        id,
        beneficiary_token,
        beneficiary_token,
        env.win_mint,
    );
    let r = send(&mut env, ix, &owner);
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::AccountNotSystemOwned)),
        "a token account in the rent_collector slot → AccountNotSystemOwned (3011): {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &beneficiary_token),
        before,
        "the rejection did not touch the state"
    );

    // A POSITIVE in the same config: a system account in the same slot passes.
    env.expire_blockhash();
    let ix = cancel_ix(&env, id, beneficiary_token);
    send(&mut env, ix, &owner).expect("a SystemAccount in the rent_collector slot is accepted");
    assert!(!env.vault_exists(id));
}

#[test]
fn cancel_rejects_vault_address_as_rent_collector() {
    let (mut env, id, beneficiary_token, _tge) = setup(VAULT_FUNDED);
    let owner = env.actors.owner.insecure_clone();
    let before = snapshot(&env, id, &beneficiary_token);

    // Redirecting the rent into the vault itself: the same structural rejection as above.
    // The case is separate because the address is submitted TWICE - into the `vault` slot
    // and into the `rent_collector` slot - and Anchor does not forbid that on its own.
    let ix = cancel_vesting_ix(
        owner.pubkey(),
        id,
        beneficiary_token,
        vault_pda(id),
        env.win_mint,
    );
    let r = send(&mut env, ix, &owner);
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::AccountNotSystemOwned)),
        "the vault's address in the rent_collector slot → AccountNotSystemOwned (3011): {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &beneficiary_token),
        before,
        "the rejection did not touch the state"
    );

    // A POSITIVE in the same config.
    env.expire_blockhash();
    let ix = cancel_ix(&env, id, beneficiary_token);
    send(&mut env, ix, &owner).expect("a correct rent_collector is accepted");
    assert!(!env.vault_exists(id));
}

#[test]
fn cancel_rejects_after_tge() {
    let (mut env, id, beneficiary_token, tge_ts) = setup(VAULT_FUNDED);
    let owner = env.actors.owner.insecure_clone();

    // The gate is inclusive: at the TGE second itself the activity is already
    // uncancellable (`now < config.tge_ts`, Arch §9.7).
    env.warp_to(tge_ts);
    let before = snapshot(&env, id, &beneficiary_token);

    let ix = cancel_ix(&env, id, beneficiary_token);
    let r = send(&mut env, ix, &owner);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::ScheduleStarted)),
        "now == tge → ScheduleStarted: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &beneficiary_token),
        before,
        "after TGE the activity and its vault are inviolable"
    );
}

#[test]
fn cancel_rejects_activity_with_nonzero_claimed() {
    let (mut env, id, beneficiary_token, _tge) = setup(VAULT_FUNDED);
    let owner = env.actors.owner.insecure_clone();

    // A forge: `claimed > 0` before TGE cannot be built by a sequence of
    // instructions - the only writer of `claimed` is `claim`, and it requires
    // `now >= tge_ts`. `cancel_vesting`'s gate leans on ANOTHER instruction's gate, so
    // without this test it would stay uncovered: a cancellation would carry away the
    // remainder of an activity whose payout had already started.
    env.forge_claimed(id, 1);
    let before = snapshot(&env, id, &beneficiary_token);

    let ix = cancel_ix(&env, id, beneficiary_token);
    let r = send(&mut env, ix, &owner);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::ScheduleStarted)),
        "claimed > 0 → ScheduleStarted even before TGE: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &beneficiary_token),
        before,
        "the rejection touched neither the vault, nor the record, nor the accounts"
    );

    // A POSITIVE in the same environment: only the `claimed` field differs.
    env.forge_claimed(id, 0);
    env.expire_blockhash();
    let ix = cancel_ix(&env, id, beneficiary_token);
    send(&mut env, ix, &owner).expect("claimed == 0 - the cancellation passes");
    assert!(!env.vesting_exists(id) && !env.vault_exists(id));
}

#[test]
fn cancel_twice_fails_on_closed_activity() {
    let (mut env, id, beneficiary_token, _tge) = setup(VAULT_FUNDED);
    let owner = env.actors.owner.insecure_clone();

    let ix = cancel_ix(&env, id, beneficiary_token);
    send(&mut env, ix, &owner).expect("the first cancellation passes");
    let after_first = env.token_balance(&beneficiary_token);
    let rent_collector_after_first = env.lamports_of(&env.actors.rent_collector.pubkey());

    // A fresh blockhash is mandatory: without it a byte-for-byte repeat would be filtered
    // out by the history as AlreadyProcessed BEFORE execution and the guard would not run
    //.
    env.expire_blockhash();
    let ix = cancel_ix(&env, id, beneficiary_token);
    let r = send(&mut env, ix, &owner);
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::AccountNotInitialized)),
        "a repeated cancellation does not find the closed record → AccountNotInitialized (3012): {r:?}"
    );
    assert_eq!(
        env.token_balance(&beneficiary_token),
        after_first,
        "the repeat transferred nothing a second time"
    );
    assert_eq!(
        env.lamports_of(&env.actors.rent_collector.pubkey()),
        rent_collector_after_first,
        "the repeat did not hand out the rent a second time"
    );
    assert_eq!(
        env.vesting_config().activity_count,
        1,
        "the activity counter is untouched by the first cancellation and by the repeat"
    );
}

// ===================================================================
// the cancellation's authorization: a foreign signer and a decoy config
//
// The card was extended after the V2 review: `cancel_vesting` is the only V2
// instruction that moves funds, and deferring to the V4 role matrix left its
// authorization without a single negative until the end of the iteration.
// ===================================================================

#[test]
fn cancel_rejects_wrong_signers() {
    let (mut env, id, beneficiary_token, _tge) = setup(VAULT_FUNDED);
    let owner = env.actors.owner.insecure_clone();
    let before = snapshot(&env, id, &beneficiary_token);

    // The foreign key goes BOTH into the `owner` slot AND into the signers: pass it only
    // into the slot and the transaction would fail on the missing signature
    // (`AccountNotSigner`), leaving the role guard unchecked. Everything else in
    // the call is valid, so the rejection is a role one.
    for (label, actor) in wrong_owners(&env) {
        env.expire_blockhash();
        let r = cancel_as(&mut env, &actor, id, beneficiary_token);
        assert_eq!(
            custom_error_code(&r),
            Some(err(VestingError::NotOwner)),
            "{label} signed cancel_vesting → NotOwner was expected: a cancellation carries the vault's remainder away to an account it names itself: {r:?}"
        );
        assert_eq!(
            snapshot(&env, id, &beneficiary_token),
            before,
            "{label}: the rejection touched neither the vault, nor the record, nor the accounts"
        );
    }

    // A POSITIVE with the same call: only the key in the `owner` slot differs.
    env.expire_blockhash();
    cancel_as(&mut env, &owner, id, beneficiary_token).expect("the correct Owner passes");
    assert_eq!(
        env.token_balance(&beneficiary_token),
        BENEFICIARY_PREFUNDED + VAULT_FUNDED,
        "only Owner withdrew the remainder"
    );
    assert!(
        !env.vault_exists(id) && !env.vesting_exists(id),
        "both of the activity's accounts are closed"
    );
}

#[test]
fn cancel_rejects_unsigned_owner_slot() {
    let (mut env, id, beneficiary_token, _tge) = setup(VAULT_FUNDED);
    let owner = env.actors.owner.insecure_clone();
    let stranger = env.actors.stranger.insecure_clone();
    let before = snapshot(&env, id, &beneficiary_token);

    let expected = u32::from(ErrorCode::AccountNotSigner);
    assert_eq!(
        expected, 3010,
        "the nominal anchor code AccountNotSigner (Anchor 1.0.2)"
    );

    // The key in the `owner` slot is CORRECT and only the signature flag was cleared:
    // `has_one` compares addresses and would let such a call through - it is the
    // `Signer<'info>` type that rejects it. The fee is paid by an outsider, otherwise
    // owner would sign the transaction as the payer and the slot would get its signature
    // back.
    let ix = without_signer(cancel_ix(&env, id, beneficiary_token), &owner.pubkey());
    let r = env.send_ixs(&[ix], &stranger.pubkey(), &[&stranger]);
    assert_eq!(
        custom_error_code(&r),
        Some(expected),
        "owner's signature was cleared → AccountNotSigner during account resolution, before the body: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &beneficiary_token),
        before,
        "the rejection touched neither the vault, nor the record, nor the accounts"
    );

    // A POSITIVE in the same environment: only the signature flag differs.
    env.expire_blockhash();
    cancel_as(&mut env, &owner, id, beneficiary_token)
        .expect("the same call with owner's signature passes");
    assert!(
        !env.vault_exists(id) && !env.vesting_exists(id),
        "the signed cancellation closed both of the activity's accounts"
    );
}

#[test]
fn cancel_rejects_forged_config() {
    let (mut env, id, beneficiary_token, _tge) = setup(VAULT_FUNDED);
    let owner = env.actors.owner.insecure_clone();

    // A forge: a program-owned account with a genuine discriminator and a
    // copy of a live config's fields at a random address. Unreachable honestly -
    // `initialize` only ever creates the config at the PDA of the seeds `[b"config"]`.
    // Were the substitution to pass, the decoy config's Owner (which could be anyone)
    // would be withdrawing the remainders of other people's vaults: `vault_authority` and
    // its bump come precisely from the submitted config.
    let forged = Pubkey::new_unique();
    let real = env.vesting_config();
    env.forge_config(&forged, &real);
    assert_ne!(
        forged,
        config_pda(),
        "the forge sits outside the canonical address"
    );
    let before = snapshot(&env, id, &beneficiary_token);

    let ix = cancel_vesting_ix_with_slots(
        owner.pubkey(),
        forged,
        vesting_pda(id),
        vault_pda(id),
        beneficiary_token,
        env.actors.rent_collector.pubkey(),
        vault_authority_pda(),
        env.win_mint,
        id,
    );
    let r = send(&mut env, ix, &owner);
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintSeeds)),
        "a config outside the canonical PDA → ConstraintSeeds (2006): Anchor derives the address from the seeds `[b\\\"config\\\"]` with the canonical bump: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &beneficiary_token),
        before,
        "the rejection did not touch the state"
    );

    // A POSITIVE in the same environment: the same call on the canonical config.
    env.expire_blockhash();
    let ix = cancel_ix(&env, id, beneficiary_token);
    send(&mut env, ix, &owner).expect("the canonical config accepts cancel_vesting");
    assert_eq!(
        env.token_balance(&beneficiary_token),
        BENEFICIARY_PREFUNDED + VAULT_FUNDED
    );
}

// ===================================================================
// [I8] - the monotonicity of activity_id and the absence of an activity_id
// argument
// ===================================================================

#[test]
fn activity_id_never_reused_after_cancel() {
    let (mut env, first_id, beneficiary_token, _tge) = setup(0);
    let owner = env.actors.owner.insecure_clone();
    assert_eq!(first_id, 0, "the first activity gets id 0");

    let ix = cancel_ix(&env, first_id, beneficiary_token);
    send(&mut env, ix, &owner).expect("cancelling activity zero passes");
    assert_eq!(
        env.vesting_config().activity_count,
        1,
        "the cancellation did not roll the counter back"
    );

    let spec = ActivitySpec::valid(REFERENCE);
    let recipient = env.actors.recipients[1].pubkey();
    let next_id = env.next_activity_id();
    let token_mint = env.win_mint;
    let r = send(
        &mut env,
        create_vesting_ix(owner.pubkey(), recipient, next_id, token_mint, &spec),
        &owner,
    );
    r.as_ref()
        .expect("creating after the cancellation passes on the NEXT id");

    let ev: VestingCreated = decode_event(&r).expect("create_vesting emits VestingCreated");
    assert_eq!(
        ev.activity_id, 1,
        "the closed activity's id is not reused: the next one is 1, not 0"
    );
    assert_eq!(
        env.vesting_config().activity_count,
        2,
        "activity_count == 2 after two creations and one cancellation"
    );
    assert!(
        env.vesting_exists(1) && env.vault_exists(1),
        "a new pair of PDAs was created"
    );
    assert!(
        !env.vesting_exists(0) && !env.vault_exists(0),
        "activity zero's PDAs never come back into existence - its seeds are spent forever"
    );
}

#[test]
fn create_vesting_takes_no_activity_id_argument() {
    // The instruction's IDL surface is checked (Arch §6.2): the activity's
    // identifier is assigned by the program from `config.activity_count` and a client
    // cannot specify it - otherwise it would occupy someone else's or a future slot.
    //
    // The source of truth is the Anchor types `vesting::instruction::*`: the IDL file is
    // generated from them by the separate `make idl` target, is not part of `make check`
    // and is absent on a cold checkout, so a test reading target/idl/*.json would turn red
    // for no good reason.
    let spec = ActivitySpec::valid(REFERENCE);

    // Part one, compile time: an exhaustive destructuring. A sixth argument
    // (`activity_id` in particular) would break this pattern's compilation.
    let vesting::instruction::CreateVesting {
        reference_amount,
        tge_bps,
        monthly_bps,
        cliff_months,
        label,
    } = vesting::instruction::CreateVesting {
        reference_amount: spec.reference_amount,
        tge_bps: spec.tge_bps,
        monthly_bps: spec.monthly_bps,
        cliff_months: spec.cliff_months,
        label: spec.label.clone(),
    };
    assert_eq!(
        (reference_amount, tge_bps, monthly_bps, cliff_months),
        (
            spec.reference_amount,
            spec.tge_bps,
            spec.monthly_bps,
            spec.cliff_months
        ),
        "the destructuring did not lose the arguments' values"
    );

    // Part two, runtime: the REAL instruction's data - the discriminator plus the Borsh
    // encoding of exactly those five arguments. A superfluous `activity_id: u64` would add
    // 8 bytes.
    let ix = create_vesting_ix(
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        0,
        Pubkey::new_unique(),
        &spec,
    );
    let expected_len = 8  // the instruction discriminator
        + 8               // reference_amount: u64
        + 2 + 2 + 2       // tge_bps / monthly_bps / cliff_months: u16
        + 4 + label.len(); // label: String = a u32 length prefix plus the bytes
    assert_eq!(
        ix.data.len(),
        expected_len,
        "create_vesting's arguments: five only; a superfluous u64 (activity_id) would add 8 bytes"
    );
}

// ===================================================================
// [I8/I9] - closing a fully drawn activity
// ===================================================================

/// A stray transfer into the vault beyond the reference amount (VE-17): the recipient
/// cannot claim it (the I1 ceiling), so on a close it goes to the project. The value is
/// arbitrary and not a multiple of the reference amount - so that the "remainder"
/// cannot be confused with a tranche or with the full balance.
const VAULT_EXCESS: u64 = 999_777;

/// The canonical close call: Owner signs, the remainder goes to `beneficiary_token`
/// and the rent to the `rent_collector` actor.
fn close_ix(env: &TestEnv, activity_id: u64, beneficiary_token: Pubkey) -> Instruction {
    close_vesting_ix(
        env.actors.owner.pubkey(),
        activity_id,
        beneficiary_token,
        env.actors.rent_collector.pubkey(),
        env.win_mint,
    )
}

/// Sends the close on behalf of `actor`: its key goes BOTH into the `owner` slot AND
/// into the signers; the fee payer is the environment's `payer` (see [`cancel_as`]).
#[allow(clippy::result_large_err)]
fn close_as(
    env: &mut TestEnv,
    actor: &Keypair,
    activity_id: u64,
    beneficiary_token: Pubkey,
) -> TransactionResult {
    let ix = close_vesting_ix(
        actor.pubkey(),
        activity_id,
        beneficiary_token,
        env.actors.rent_collector.pubkey(),
        env.win_mint,
    );
    let payer = env.payer.insecure_clone();
    env.send_ixs(&[ix], &payer.pubkey(), &[&payer, actor])
}

/// Warps time to the terminal period of the activity's schedule (day 541, "Ecosystem")
/// and releases the whole reference amount to the recipient in a single claim -
/// the only honest path to the state "the activity is closable".
fn claim_everything(env: &mut TestEnv, activity_id: u64, tge_ts: i64) {
    let recipient = env.actors.recipients[0].insecure_clone();
    env.claim_at_day(tge_ts, ECOSYSTEM_FULL_RELEASE_DAY, activity_id, &recipient)
        .expect("the terminal claim releases the whole reference amount");
    assert_eq!(
        env.vesting(activity_id).claimed,
        REFERENCE,
        "precondition: the activity is fully drawn down"
    );
}

// ---------- [I9]: the full-draw-down gate ----------

#[test]
fn close_rejects_activity_with_nothing_claimed() {
    let (mut env, id, beneficiary_token, tge_ts) = setup(VAULT_FUNDED);
    let owner = env.actors.owner.insecure_clone();
    let before = snapshot(&env, id, &beneficiary_token);
    assert_eq!(
        env.vesting(id).claimed,
        0,
        "precondition: nothing has been claimed"
    );

    // Before TGE the rejection must come from the DRAW-DOWN rather than from time:
    // `close_vesting` has no time gate and none must appear, or a drawn-down activity
    // could not be closed after TGE.
    let ix = close_ix(&env, id, beneficiary_token);
    let r = send(&mut env, ix, &owner);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::NotFullyClaimed)),
        "before TGE close_vesting answers NotFullyClaimed rather than ScheduleStarted: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &beneficiary_token),
        before,
        "the rejection touched neither the vault, nor the record, nor the accounts"
    );

    // After TGE the two instructions' gates diverge for good: the cancellation is closed
    // by time and the close by the incomplete draw-down. Neither releases any funds.
    env.warp_to(tge_ts);
    env.expire_blockhash();
    let ix = cancel_ix(&env, id, beneficiary_token);
    let r = send(&mut env, ix, &owner);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::ScheduleStarted)),
        "after TGE the cancellation is closed by time: {r:?}"
    );
    env.expire_blockhash();
    let ix = close_ix(&env, id, beneficiary_token);
    let r = send(&mut env, ix, &owner);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::NotFullyClaimed)),
        "after TGE the close is still closed by the incomplete draw-down: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &beneficiary_token),
        before,
        "the funded activity survived both rejections in full"
    );
}

#[test]
fn close_rejects_activity_short_by_one_unit() {
    let (mut env, id, beneficiary_token, _tge) = setup(VAULT_FUNDED);
    let owner = env.actors.owner.insecure_clone();

    // A forge of `claimed`: the state "everything drawn down except one token
    // unit" would honestly cost a run through the whole schedule, while what is checked
    // here is exactly the gate's boundary - the last unit must keep the activity open.
    env.forge_claimed(id, REFERENCE - 1);
    let before = snapshot(&env, id, &beneficiary_token);

    let ix = close_ix(&env, id, beneficiary_token);
    let r = send(&mut env, ix, &owner);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::NotFullyClaimed)),
        "claimed == reference_amount − 1 → NotFullyClaimed: closing with an open obligation would lose the unreleased remainder: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &beneficiary_token),
        before,
        "the rejection did not touch the state"
    );

    // A POSITIVE in the same environment: exactly on the boundary the gate opens.
    env.forge_claimed(id, REFERENCE);
    env.expire_blockhash();
    let ix = close_ix(&env, id, beneficiary_token);
    send(&mut env, ix, &owner).expect("claimed == reference_amount closes the activity");
    assert!(
        !env.vesting_exists(id) && !env.vault_exists(id),
        "both of the activity's accounts are closed"
    );
}

// ---------- [I9]: the positive after the terminal claim ----------

#[test]
fn close_after_terminal_claim_closes_both_accounts_and_returns_rent() {
    let (mut env, id, beneficiary_token, tge_ts) = setup(REFERENCE);
    let owner = env.actors.owner.insecure_clone();
    let rent_collector = env.actors.rent_collector.pubkey();
    claim_everything(&mut env, id, tge_ts);
    assert_eq!(
        env.vault_balance(id),
        0,
        "precondition: funded to exactly the reference amount and all of it released"
    );

    let vesting_rent = env.lamports_of(&vesting_pda(id));
    let vault_rent = env.lamports_of(&vault_pda(id));
    let rent_collector_before = env.lamports_of(&rent_collector);
    let beneficiary_before = env.token_balance(&beneficiary_token);

    env.expire_blockhash();
    let ix = close_ix(&env, id, beneficiary_token);
    let r = send(&mut env, ix.clone(), &owner);
    r.as_ref().expect("a fully drawn activity closes");

    // The vault is empty, so there must be no transfer in the CPI list - the paired half
    // of the skip assertion above.
    assert_eq!(
        spl_token_cpi_tags(&r, &ix, &owner.pubkey()),
        vec![SPL_TAG_CLOSE_ACCOUNT],
        "an empty vault: CloseAccount only, the Transfer is skipped"
    );
    assert!(!env.vault_exists(id), "the vault is closed");
    assert!(!env.vesting_exists(id), "the Vesting record is closed");
    assert_eq!(
        env.token_balance(&beneficiary_token),
        beneficiary_before,
        "there was nothing to transfer - the beneficiary's account did not change"
    );
    assert_eq!(
        env.lamports_of(&rent_collector) - rent_collector_before,
        vesting_rent + vault_rent,
        "the rent of BOTH accounts arrived at rent_collector (the CloseAccount CPI + Anchor's close)"
    );
    assert_eq!(
        env.vesting_config().activity_count,
        1,
        "activity_count is not decremented on a close either (I8)"
    );

    // [#5] Funded to exactly the reference amount and all of it released → the vault is
    // empty → `amount == 0`.
    let ev: VestingClosed = decode_event(&r).expect("close_vesting emits VestingClosed");
    assert_eq!(
        ev.activity_id, id,
        "the event carries the closed activity's id"
    );
    assert_eq!(ev.amount, 0, "the event: an empty vault drains zero");
    assert_eq!(
        ev.beneficiary_token, beneficiary_token,
        "the event: the remainder's addressee"
    );
    assert_eq!(
        ev.rent_collector, rent_collector,
        "the event: the rent recipient"
    );
}

// ---------- [VE-17]: a surplus beyond the reference amount goes to the project ----------

#[test]
fn close_sends_the_excess_above_reference_to_the_beneficiary() {
    // The vault is funded beyond the reference amount - simulating a stray transfer to its
    // address (Arch §7): it does not move the unlock and cannot be claimed.
    let (mut env, id, beneficiary_token, tge_ts) = setup(REFERENCE + VAULT_EXCESS);
    let owner = env.actors.owner.insecure_clone();
    let recipient = env.actors.recipients[0].pubkey();

    claim_everything(&mut env, id, tge_ts);
    assert_eq!(
        env.token_balance(&recipient_ata(&recipient, &env.win_mint)),
        REFERENCE,
        "exactly the reference amount was released to the recipient: the surplus did not increase the unlock (I11)"
    );
    assert_eq!(
        env.vault_balance(id),
        VAULT_EXCESS,
        "exactly the surplus remained in the vault"
    );
    let beneficiary_before = env.token_balance(&beneficiary_token);

    env.expire_blockhash();
    let ix = close_ix(&env, id, beneficiary_token);
    let r = send(&mut env, ix.clone(), &owner);
    // This call's success IS the proof of the drain→close order: SPL Token does not close
    // a non-native token account with a non-zero balance, so the reverse order would fail
    // the transaction precisely here.
    r.as_ref()
        .expect("a close with a non-empty vault passes: drain first, then close");
    assert_eq!(
        spl_token_cpi_tags(&r, &ix, &owner.pubkey()),
        vec![SPL_TAG_TRANSFER, SPL_TAG_CLOSE_ACCOUNT],
        "a non-empty vault: Transfer, then CloseAccount"
    );
    assert_eq!(
        env.token_balance(&beneficiary_token),
        beneficiary_before + VAULT_EXCESS,
        "the surplus went to beneficiary_token rather than staying locked"
    );
    assert!(
        !env.vault_exists(id) && !env.vesting_exists(id),
        "both accounts are closed"
    );

    // [#5] A non-empty vault → `amount` == the drained surplus.
    let ev: VestingClosed = decode_event(&r).expect("close_vesting emits VestingClosed");
    assert_eq!(ev.activity_id, id, "the event: the closed activity's id");
    assert_eq!(
        ev.amount, VAULT_EXCESS,
        "the event: amount == the drained surplus (the vault's balance before the call)"
    );
    assert_eq!(
        ev.beneficiary_token, beneficiary_token,
        "the event: the remainder's addressee"
    );
    assert_eq!(
        ev.rent_collector,
        env.actors.rent_collector.pubkey(),
        "the event: the rent recipient"
    );
}

// ---------- The end-to-end chain: create → fund → TGE → claim×2 → close ----------

/// The day of the chain's first claim: the architecture's example V5 - the recipient
/// does not claim until day 151, and a single call releases what accrued (the TGE share
/// plus five tranches).
const DAY_151: i64 = 151;

#[test]
fn claim_chain_across_periods_then_close_vesting() {
    // An activity's full lifecycle the honest way, without a single forge: create
    // (V2/ECOSYSTEM) → fund(V2_REFERENCE) → TGE → the day-151 claim → the day-541 (terminal)
    // claim → close_vesting. Every expectation is a literal of example V5.
    let (mut env, id, beneficiary_token, tge_ts) = setup(REFERENCE);
    let owner = env.actors.owner.insecure_clone();
    let recipient = env.actors.recipients[0].insecure_clone();
    let rent_collector = env.actors.rent_collector.pubkey();
    let ata = recipient_ata(&recipient.pubkey(), &env.win_mint);

    // The first claim: day 151.
    env.claim_at_day(tge_ts, DAY_151, id, &recipient)
        .expect("the day-151 claim passes");
    assert_eq!(
        env.token_balance(&ata),
        V5_CLAIMED_AFTER_DAY_151,
        "after the first claim the ATA holds what accrued by day 151 (example V5)"
    );
    assert_eq!(
        env.vesting(id).claimed,
        V5_CLAIMED_AFTER_DAY_151,
        "claimed == vested(day 151) (I1)"
    );
    let after_first = env.token_balance(&ata);

    // The second claim: the terminal day 541 takes the remainder.
    env.claim_at_day(tge_ts, ECOSYSTEM_FULL_RELEASE_DAY, id, &recipient)
        .expect("the terminal day-541 claim passes");
    assert_eq!(
        env.token_balance(&ata),
        V5_CLAIMED_AFTER_DAY_541,
        "after the second claim the ATA holds the whole reference amount (example V5)"
    );
    assert_eq!(
        env.token_balance(&ata) - after_first,
        V5_CLAIMABLE_DAY_541,
        "the second claim's increment is exactly the obligation's remainder"
    );
    assert_eq!(
        env.vesting(id).claimed,
        REFERENCE,
        "claimed == reference_amount - the activity is closable (I9)"
    );

    // The close: both accounts are closed and the rent of both goes to rent_collector.
    let vesting_rent = env.lamports_of(&vesting_pda(id));
    let vault_rent = env.lamports_of(&vault_pda(id));
    let rent_collector_before = env.lamports_of(&rent_collector);
    env.expire_blockhash();
    let ix = close_ix(&env, id, beneficiary_token);
    let r = send(&mut env, ix, &owner);
    r.as_ref()
        .expect("close_vesting after a full draw-down through a chain of claims passes");
    assert!(
        !env.vesting_exists(id) && !env.vault_exists(id),
        "both of the activity's accounts are closed"
    );
    assert_eq!(
        env.lamports_of(&rent_collector) - rent_collector_before,
        vesting_rent + vault_rent,
        "the rent of BOTH accounts arrived at rent_collector exactly once"
    );

    let ev: VestingClosed = decode_event(&r).expect("close_vesting emits VestingClosed");
    assert_eq!(ev.activity_id, id, "the event: the closed activity's id");
    assert_eq!(
        ev.amount, 0,
        "the event: funded to exactly the reference amount and drawn down in full - there was nothing to drain"
    );
    assert_eq!(
        ev.beneficiary_token, beneficiary_token,
        "the event: the remainder's addressee"
    );
    assert_eq!(
        ev.rent_collector, rent_collector,
        "the event: the rent recipient"
    );
}

// ---------- [I12]: the same three beneficiary_token conditions on the close ----------

/// Prepares the input for substitutions into the `beneficiary_token` slot: a second
/// activity with a funded vault and `vault_authority`'s own ATA. Both before TGE:
/// `create_vesting` is closed after it (I6), and the activity being closed must first be
/// drawn down in full. Returns `(other_id, authority_ata)`.
fn setup_custody_slots(env: &mut TestEnv) -> (u64, Pubkey) {
    let other_recipient = env.actors.recipients[1].pubkey();
    let other_id = env.create_activity_for(&other_recipient, &ActivitySpec::valid(REFERENCE));
    env.fund_vault(other_id, OTHER_VAULT_FUNDED);
    let win_mint = env.win_mint;
    // The ATA program does not ask for the owner's signature, so anyone can create an
    // account under `vault_authority` in advance - the path is honest.
    let authority_ata = env.create_ata(&vault_authority_pda(), &win_mint);
    (other_id, authority_ata)
}

#[test]
fn close_rejects_beneficiary_token_under_program_custody() {
    // The `CloseActivity` context is shared by the cancellation and the close, so the
    // `beneficiary_token` slot's conditions apply to both instructions structurally. The
    // test holds that boundary by behaviour: split the contexts apart and the loss of the
    // conditions on the close would pass silently, while a close does take the vault's
    // remainder away.
    let (mut env, id, beneficiary_token, tge_ts) = setup(REFERENCE + VAULT_EXCESS);
    let owner = env.actors.owner.insecure_clone();
    let (other_id, authority_ata) = setup_custody_slots(&mut env);

    claim_everything(&mut env, id, tge_ts);
    // The vault stays NON-EMPTY: on an empty one "the remainder went nowhere" would be
    // checked degenerately (there is nothing to lose).
    assert_eq!(
        env.vault_balance(id),
        VAULT_EXCESS,
        "precondition: the activity is drawn down and a surplus remained in the vault"
    );
    let before = snapshot(&env, id, &beneficiary_token);

    for (label, slot) in [
        ("another activity's vault", vault_pda(other_id)),
        ("vault_authority's own ATA", authority_ata),
    ] {
        env.expire_blockhash();
        let ix = close_ix(&env, id, slot);
        let r = send(&mut env, ix, &owner);
        assert_eq!(
            custom_error_code(&r),
            Some(err(VestingError::InvalidBeneficiary)),
            "{label} in close_vesting's beneficiary_token slot → InvalidBeneficiary: the remainder would go under vault_authority's custody, and the program has no instruction that takes funds off such an account: {r:?}"
        );
        assert_eq!(
            snapshot(&env, id, &beneficiary_token),
            before,
            "{label}: the rejection touched neither the vault, nor the record, nor the accounts"
        );
    }
    assert_eq!(
        env.vault_balance(other_id),
        OTHER_VAULT_FUNDED,
        "the foreign vault received not a single unit"
    );
    assert_eq!(
        env.token_balance(&authority_ata),
        0,
        "nothing went to the account under the program's custody"
    );

    // A POSITIVE in the same environment: only the addressee differs.
    env.expire_blockhash();
    let ix = close_ix(&env, id, beneficiary_token);
    send(&mut env, ix, &owner).expect("an account outside vault_authority's custody is accepted");
    assert_eq!(
        env.token_balance(&beneficiary_token),
        BENEFICIARY_PREFUNDED + VAULT_EXCESS,
        "the surplus went to an account outside the program's custody"
    );
    assert!(
        !env.vesting_exists(id) && !env.vault_exists(id),
        "both accounts are closed"
    );
}

#[test]
fn close_rejects_beneficiary_token_of_foreign_mint_and_the_vault_address() {
    // The slot's third condition (`mint == config.token_mint`) and the safeguard
    // inequality against the vault - also on the close, with the same codes as on the
    // cancellation: the domain `InvalidMint` and the structural
    // `ConstraintDuplicateMutableAccount` (2040) respectively.
    let (mut env, id, beneficiary_token, tge_ts) = setup(REFERENCE + VAULT_EXCESS);
    let owner = env.actors.owner.insecure_clone();
    let beneficiary = env.actors.beneficiary.pubkey();
    let foreign_mint = env.foreign_mint;
    let foreign_token = env.create_ata(&beneficiary, &foreign_mint);

    claim_everything(&mut env, id, tge_ts);
    let before = snapshot(&env, id, &beneficiary_token);

    for (label, slot, code) in [
        (
            "a foreign mint",
            foreign_token,
            err(VestingError::InvalidMint),
        ),
        (
            "the vault's own address",
            vault_pda(id),
            u32::from(ErrorCode::ConstraintDuplicateMutableAccount),
        ),
    ] {
        env.expire_blockhash();
        let ix = close_ix(&env, id, slot);
        let r = send(&mut env, ix, &owner);
        assert_eq!(
            custom_error_code(&r),
            Some(code),
            "{label} in close_vesting's beneficiary_token slot: {r:?}"
        );
        assert_eq!(
            snapshot(&env, id, &beneficiary_token),
            before,
            "{label}: the rejection did not touch the state"
        );
    }
    assert_eq!(
        env.token_balance(&foreign_token),
        0,
        "nothing went to the account under the foreign mint"
    );

    // A POSITIVE in the same environment.
    env.expire_blockhash();
    let ix = close_ix(&env, id, beneficiary_token);
    send(&mut env, ix, &owner)
        .expect("an ATA of the correct mint outside the program's custody is accepted");
    assert_eq!(
        env.token_balance(&beneficiary_token),
        BENEFICIARY_PREFUNDED + VAULT_EXCESS
    );
}

// ---------- [I8]: a closed activity is unreachable by anything ----------

#[test]
fn closed_activity_rejects_every_instruction_that_addresses_it() {
    let (mut env, id, beneficiary_token, tge_ts) = setup(REFERENCE);
    let owner = env.actors.owner.insecure_clone();
    let recipient = env.actors.recipients[0].insecure_clone();
    let new_recipient = env.actors.recipients[1].pubkey();
    let win_mint = env.win_mint;

    claim_everything(&mut env, id, tge_ts);
    env.expire_blockhash();
    let ix = close_ix(&env, id, beneficiary_token);
    send(&mut env, ix, &owner).expect("the activity closes after the full claim");
    let beneficiary_after_close = env.token_balance(&beneficiary_token);
    let rent_collector_after_close = env.lamports_of(&env.actors.rent_collector.pubkey());

    // Every instruction addressing the activity must fail during account RESOLUTION:
    // zeroing the data and reassigning to the System Program IS the one-shot guard against
    // a repeat (I8). A fresh blockhash before every send is mandatory, otherwise the
    // repeat would be filtered out by the history before execution.
    //
    // Each case has its own signer, which is also the fee payer of its transaction: an
    // extra key in the signers is an error at transaction assembly time and the test would
    // be checking the wrong thing. `assert_funded` has no signer slot at all
    // (permissionless, Arch §9.9) - its key (stranger, with no role) only pays the fee, and
    // even that access fails at the same resolution.
    let cases = |env: &TestEnv| -> Vec<(&'static str, Instruction, Keypair)> {
        vec![
            (
                "claim",
                claim_ix(recipient.pubkey(), id, recipient.pubkey(), win_mint),
                recipient.insecure_clone(),
            ),
            (
                "set_recipient",
                set_recipient_ix(owner.pubkey(), id, new_recipient),
                owner.insecure_clone(),
            ),
            (
                "cancel_vesting",
                cancel_ix(env, id, beneficiary_token),
                owner.insecure_clone(),
            ),
            (
                "close_vesting",
                close_ix(env, id, beneficiary_token),
                owner.insecure_clone(),
            ),
            (
                "assert_funded",
                assert_funded_ix(id, win_mint),
                env.actors.stranger.insecure_clone(),
            ),
        ]
    };

    // The property under test is "there is no program account at the address and the call
    // reverts" rather than one specific code. Anchor returns `AccountNotInitialized`
    // (3012) only while the address SIMULTANEOUSLY has the System Program as its owner and
    // zero lamports; anyone can send lamports to a closed activity's predictable address,
    // and then the same rejection arrives as `AccountOwnedByWrongProgram` (3007). So the
    // second pass runs over the topped-up addresses and the expectation is a pair of
    // codes: asserting on one of them would check a point instead of a class.
    let account_is_gone = [
        u32::from(ErrorCode::AccountNotInitialized),
        u32::from(ErrorCode::AccountOwnedByWrongProgram),
    ];
    let mut observed: Vec<u32> = Vec::new();
    for (stage, prefund) in [
        ("the closed addresses are empty", false),
        ("lamports were topped up onto the closed addresses", true),
    ] {
        if prefund {
            env.prefund_lamports(&vesting_pda(id), 1);
            env.prefund_lamports(&vault_pda(id), 1);
            assert!(
                env.account_exists(&vesting_pda(id)) && env.account_exists(&vault_pda(id)),
                "the second pass's precondition: both closed addresses exist again"
            );
        }
        for (label, ix, signer) in cases(&env) {
            env.expire_blockhash();
            let r = env.send_ixs(&[ix], &signer.pubkey(), &[&signer]);
            let code = custom_error_code(&r).unwrap_or_else(|| {
                panic!("{label} ({stage}) failed with a non-Anchor code: {r:?}")
            });
            assert!(
                account_is_gone.contains(&code),
                "{label} on a closed activity ({stage}) must be rejected for the account's absence - 3012 or 3007; got {code}: {r:?}"
            );
            observed.push(code);
        }
    }
    // The second pass must be substantive rather than a repeat of the first: if both
    // passes yield the same code, the class's second point is unchecked.
    assert!(
        account_is_gone.iter().all(|c| observed.contains(c)),
        "the passes must yield BOTH codes of the class, {observed:?} was observed"
    );

    assert_eq!(
        env.token_balance(&beneficiary_token),
        beneficiary_after_close,
        "not a single repeated attempt transferred a single unit"
    );
    assert_eq!(
        env.lamports_of(&env.actors.rent_collector.pubkey()),
        rent_collector_after_close,
        "the rent was not handed out a second time"
    );
    assert_eq!(
        env.vesting_config().activity_count,
        1,
        "the counter is untouched by the close and by the repeats: a closed activity's id is spent forever (I8)"
    );
}

// ---------- The close's authorization ----------

#[test]
fn close_rejects_wrong_signers() {
    let (mut env, id, beneficiary_token, tge_ts) = setup(REFERENCE + VAULT_EXCESS);
    let owner = env.actors.owner.insecure_clone();
    claim_everything(&mut env, id, tge_ts);
    let before = snapshot(&env, id, &beneficiary_token);

    // A close carries the vault's surplus and the rent of both accounts away to accounts
    // named by the caller, so its role negative is as load-bearing as the cancellation's.
    // The foreign key goes BOTH into the `owner` slot AND into the signers.
    for (label, actor) in wrong_owners(&env) {
        env.expire_blockhash();
        let r = close_as(&mut env, &actor, id, beneficiary_token);
        assert_eq!(
            custom_error_code(&r),
            Some(err(VestingError::NotOwner)),
            "{label} signed close_vesting → NotOwner was expected: {r:?}"
        );
        assert_eq!(
            snapshot(&env, id, &beneficiary_token),
            before,
            "{label}: the rejection touched neither the vault, nor the record, nor the accounts"
        );
    }

    // A POSITIVE with the same call: only the key in the `owner` slot differs.
    env.expire_blockhash();
    close_as(&mut env, &owner, id, beneficiary_token)
        .expect("the correct Owner closes the activity");
    assert!(!env.vesting_exists(id) && !env.vault_exists(id));
}

#[test]
fn close_rejects_unsigned_owner_slot() {
    let (mut env, id, beneficiary_token, tge_ts) = setup(REFERENCE + VAULT_EXCESS);
    let owner = env.actors.owner.insecure_clone();
    let stranger = env.actors.stranger.insecure_clone();
    claim_everything(&mut env, id, tge_ts);
    let before = snapshot(&env, id, &beneficiary_token);

    let expected = u32::from(ErrorCode::AccountNotSigner);
    assert_eq!(
        expected, 3010,
        "the nominal anchor code AccountNotSigner (Anchor 1.0.2)"
    );

    // The same case as on the cancellation, but on the close: the context's `owner` slot
    // is one, yet the slot's type is a property of the context as a whole and must be
    // checked on every instruction that shares it. Without Owner's signature an outsider
    // would carry off the vault's surplus and the rent of both accounts.
    let ix = without_signer(close_ix(&env, id, beneficiary_token), &owner.pubkey());
    let r = env.send_ixs(&[ix], &stranger.pubkey(), &[&stranger]);
    assert_eq!(
        custom_error_code(&r),
        Some(expected),
        "owner's signature was cleared → AccountNotSigner during account resolution, before the body: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &beneficiary_token),
        before,
        "the rejection touched neither the vault, nor the record, nor the accounts"
    );

    // A POSITIVE in the same environment: only the signature flag differs.
    env.expire_blockhash();
    close_as(&mut env, &owner, id, beneficiary_token)
        .expect("the same call with owner's signature passes");
    assert!(
        !env.vesting_exists(id) && !env.vault_exists(id),
        "the signed close closed both of the activity's accounts"
    );
}

// ===================================================================
// A [I2 / uncaught seeds+bump mutations on CloseActivity] - activity isolation
//
// A mutation run did not turn red on removing `seeds+bump` from the `vault` slot or
// from the `vesting` slot of the `CloseActivity` context. Binding `vault`/`vesting` to the `activity_id` argument is the only thing
// stopping one from submitting one activity's fully drawn record together with
// ANOTHER's funded vault, draining the foreign vault and closing both accounts.
// Following the pattern (claim.rs): two funded activities A and B
// with different recipients and balances, mismatched slots through `_with_slots`,
// `ConstraintSeeds` (2006), and on every negative a snapshot "the rejection moved
// nothing" plus "B's vault is intact". The coverage is held on BOTH instructions even
// though the context is shared: split them apart and the loss of the binding on one
// would pass silently.
// ===================================================================

/// Two funded activities A and B with DIFFERENT recipients and vault balances; the
/// config is BEFORE TGE. `cancel_vesting` requires pre-TGE while `close_vesting` has no
/// time gate, so a single environment serves both instructions. `beneficiary_token` is
/// the canonical ATA of the single withdrawal recipient W (empty). Returns
/// `(env, id_a, id_b, beneficiary_token)`.
fn setup_two_activities() -> (TestEnv, u64, u64, Pubkey) {
    let mut env = TestEnv::new();
    env.init_default(DEFAULT_TGE_OFFSET);
    let first = env.actors.recipients[0].pubkey();
    let second = env.actors.recipients[1].pubkey();
    let id_a = env.create_activity_for(&first, &ActivitySpec::valid(REFERENCE));
    let id_b = env.create_activity_for(&second, &ActivitySpec::valid(REFERENCE));
    env.fund_vault(id_a, VAULT_FUNDED);
    env.fund_vault(id_b, OTHER_VAULT_FUNDED);
    let withdrawal = env.actors.rent_collector.pubkey();
    let win_mint = env.win_mint;
    let beneficiary_token = env.create_ata(&withdrawal, &win_mint);
    (env, id_a, id_b, beneficiary_token)
}

#[test]
fn cancel_rejects_vault_of_another_activity() {
    let (mut env, id_a, id_b, beneficiary_token) = setup_two_activities();
    let owner = env.actors.owner.insecure_clone();
    let rent_collector = env.actors.rent_collector.pubkey();
    let win_mint = env.win_mint;
    let before_a = snapshot(&env, id_a, &beneficiary_token);
    let before_b = snapshot(&env, id_b, &beneficiary_token);

    // The argument and the record belong to activity A while the vault is supplied from B.
    // Without seeds+bump on the `vault` slot, cancelling A would drain B's FOREIGN vault
    // and close it.
    let ix = cancel_vesting_ix_with_slots(
        owner.pubkey(),
        config_pda(),
        vesting_pda(id_a),
        vault_pda(id_b),
        beneficiary_token,
        rent_collector,
        vault_authority_pda(),
        win_mint,
        id_a,
    );
    let r = send(&mut env, ix, &owner);
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintSeeds)),
        "activity B's vault with the argument A → ConstraintSeeds: a vault's address is derived from the same activity_id as the record (I2): {r:?}"
    );
    assert_eq!(
        snapshot(&env, id_a, &beneficiary_token),
        before_a,
        "activity A is intact"
    );
    assert_eq!(
        snapshot(&env, id_b, &beneficiary_token),
        before_b,
        "activity B is intact"
    );
    assert_eq!(
        env.vault_balance(id_b),
        OTHER_VAULT_FUNDED,
        "B's foreign vault lost not a single unit"
    );

    // A POSITIVE: the canonical cancellation of A drains ITS vault and leaves B alone.
    env.expire_blockhash();
    let ix = cancel_ix(&env, id_a, beneficiary_token);
    send(&mut env, ix, &owner).expect("the canonical cancellation of A passes");
    assert_eq!(
        env.token_balance(&beneficiary_token),
        VAULT_FUNDED,
        "exactly A's vault was drained"
    );
    assert_eq!(
        env.vault_balance(id_b),
        OTHER_VAULT_FUNDED,
        "the positive did not touch B either"
    );
}

#[test]
fn cancel_rejects_record_of_another_activity() {
    let (mut env, id_a, id_b, beneficiary_token) = setup_two_activities();
    let owner = env.actors.owner.insecure_clone();
    let rent_collector = env.actors.rent_collector.pubkey();
    let win_mint = env.win_mint;
    let before_a = snapshot(&env, id_a, &beneficiary_token);
    let before_b = snapshot(&env, id_b, &beneficiary_token);

    // The argument and the vault belong to activity B while the record is supplied from A.
    // Without seeds+bump on the `vesting` slot the cancellation would accept A's record,
    // drain B's vault and close A's record - a mismatch of "we drain one and close
    // another".
    let ix = cancel_vesting_ix_with_slots(
        owner.pubkey(),
        config_pda(),
        vesting_pda(id_a),
        vault_pda(id_b),
        beneficiary_token,
        rent_collector,
        vault_authority_pda(),
        win_mint,
        id_b,
    );
    let r = send(&mut env, ix, &owner);
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintSeeds)),
        "activity A's record with the argument B → ConstraintSeeds: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id_a, &beneficiary_token),
        before_a,
        "activity A is intact"
    );
    assert_eq!(
        snapshot(&env, id_b, &beneficiary_token),
        before_b,
        "activity B is intact"
    );
    assert_eq!(
        env.vault_balance(id_b),
        OTHER_VAULT_FUNDED,
        "B's vault is untouched"
    );

    // A POSITIVE: the canonical cancellation of B passes.
    env.expire_blockhash();
    let ix = cancel_ix(&env, id_b, beneficiary_token);
    send(&mut env, ix, &owner).expect("the canonical cancellation of B passes");
    assert_eq!(
        env.token_balance(&beneficiary_token),
        OTHER_VAULT_FUNDED,
        "B's vault was drained"
    );
}

#[test]
fn close_rejects_vault_of_another_activity() {
    let (mut env, id_a, id_b, beneficiary_token) = setup_two_activities();
    let owner = env.actors.owner.insecure_clone();
    let rent_collector = env.actors.rent_collector.pubkey();
    let win_mint = env.win_mint;
    // A was brought to a full draw-down by a forge: `close` requires
    // `claimed >= reference`, but A's vault must stay funded - otherwise draining B's
    // foreign one would be unobservable. Honestly `claimed == reference` comes from the
    // terminal claim, but that would empty A's vault too.
    env.forge_claimed(id_a, REFERENCE);
    let before_a = snapshot(&env, id_a, &beneficiary_token);
    let before_b = snapshot(&env, id_b, &beneficiary_token);

    // A's record (fully drawn - the `close` gate would pass) with the vault supplied from
    // B. Without seeds+bump on the `vault` slot this is exactly the threat:
    // `close` would drain B's funded foreign vault and close both accounts.
    let ix = close_vesting_ix_with_slots(
        owner.pubkey(),
        config_pda(),
        vesting_pda(id_a),
        vault_pda(id_b),
        beneficiary_token,
        rent_collector,
        vault_authority_pda(),
        win_mint,
        id_a,
    );
    let r = send(&mut env, ix, &owner);
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintSeeds)),
        "activity B's vault with the argument A on close → ConstraintSeeds: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id_a, &beneficiary_token),
        before_a,
        "activity A is intact"
    );
    assert_eq!(
        snapshot(&env, id_b, &beneficiary_token),
        before_b,
        "activity B is intact"
    );
    assert_eq!(
        env.vault_balance(id_b),
        OTHER_VAULT_FUNDED,
        "B's foreign vault was not drained"
    );

    // A POSITIVE: the canonical close of A drains ITS vault.
    env.expire_blockhash();
    let ix = close_ix(&env, id_a, beneficiary_token);
    send(&mut env, ix, &owner).expect("the canonical close of A passes");
    assert_eq!(
        env.token_balance(&beneficiary_token),
        VAULT_FUNDED,
        "A's vault was drained"
    );
    assert_eq!(
        env.vault_balance(id_b),
        OTHER_VAULT_FUNDED,
        "the positive did not touch B either"
    );
}

#[test]
fn close_rejects_record_of_another_activity() {
    let (mut env, id_a, id_b, beneficiary_token) = setup_two_activities();
    let owner = env.actors.owner.insecure_clone();
    let rent_collector = env.actors.rent_collector.pubkey();
    let win_mint = env.win_mint;
    // A is fully drawn by a forge: under the mutation (no seeds on `vesting`) `close` would
    // accept its record as "fully drawn" alongside B's vault.
    env.forge_claimed(id_a, REFERENCE);
    let before_a = snapshot(&env, id_a, &beneficiary_token);
    let before_b = snapshot(&env, id_b, &beneficiary_token);

    // The argument and the vault belong to activity B while the record is supplied from A
    // (fully drawn). Without seeds+bump on the `vesting` slot `close` would drain B's vault
    // against A's foreign drawn-down record and close A's record.
    let ix = close_vesting_ix_with_slots(
        owner.pubkey(),
        config_pda(),
        vesting_pda(id_a),
        vault_pda(id_b),
        beneficiary_token,
        rent_collector,
        vault_authority_pda(),
        win_mint,
        id_b,
    );
    let r = send(&mut env, ix, &owner);
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintSeeds)),
        "activity A's record with the argument B on close → ConstraintSeeds: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id_a, &beneficiary_token),
        before_a,
        "activity A is intact"
    );
    assert_eq!(
        snapshot(&env, id_b, &beneficiary_token),
        before_b,
        "activity B is intact"
    );
    assert_eq!(
        env.vault_balance(id_b),
        OTHER_VAULT_FUNDED,
        "B's vault was not drained"
    );

    // A POSITIVE: the canonical close of the fully drawn A passes.
    env.expire_blockhash();
    let ix = close_ix(&env, id_a, beneficiary_token);
    send(&mut env, ix, &owner).expect("the canonical close of the drawn-down A passes");
    assert_eq!(
        env.token_balance(&beneficiary_token),
        VAULT_FUNDED,
        "A's vault was drained"
    );
    assert_eq!(
        env.vault_balance(id_b),
        OTHER_VAULT_FUNDED,
        "the positive did not touch B either"
    );
}

// ===================================================================
// D [#3] - binding rent_collector and beneficiary_token to the single recipient W
//
// Fix #3 pins W (`config.withdrawal_authority`) at `initialize` and in
// `CloseActivity` checks both addressees against it: `rent_collector` through
// `address == config.withdrawal_authority` and `beneficiary_token` through
// `owner == config.withdrawal_authority`. The core of the finding: `rent_collector`
// used to be constrained only by the `SystemAccount` type (ANY system account passed,
// including a data-less program PDA - the rent would go irrecoverably), and
// `beneficiary_token` only by `owner != vault_authority` (any outside wallet's ATA
// passed). The regressions are placed on BOTH instructions (the context is shared).
// ===================================================================

#[test]
fn cancel_rejects_invalid_rent_collector() {
    let (mut env, id, beneficiary_token, _tge) = setup(VAULT_FUNDED);
    let owner = env.actors.owner.insecure_clone();
    let win_mint = env.win_mint;
    let before = snapshot(&env, id, &beneficiary_token);

    // Both decoys are system-owned, so `SystemAccount` accepts them; what rejects them is
    // precisely `address == W`. `stranger` is an ordinary wallet ≠ W (the core of the
    // finding: it would have passed before); `vault_authority` is a data-less program PDA
    // (the finding's example: the rent would go into the program's custody
    // irrecoverably).
    for (label, bad) in [
        ("an outside system wallet ≠ W", env.actors.stranger.pubkey()),
        ("the data-less vault_authority PDA", vault_authority_pda()),
    ] {
        env.expire_blockhash();
        let ix = cancel_vesting_ix(owner.pubkey(), id, beneficiary_token, bad, win_mint);
        let r = send(&mut env, ix, &owner);
        assert_eq!(
            custom_error_code(&r),
            Some(err(VestingError::InvalidRentCollector)),
            "{label} in the rent_collector slot → InvalidRentCollector (#3): {r:?}"
        );
        assert_eq!(
            snapshot(&env, id, &beneficiary_token),
            before,
            "{label}: the rejection did not touch the state"
        );
    }

    // A POSITIVE: rent_collector == W is accepted.
    env.expire_blockhash();
    let ix = cancel_ix(&env, id, beneficiary_token);
    send(&mut env, ix, &owner).expect("rent_collector == W is accepted");
    assert!(
        !env.vault_exists(id),
        "the vault is closed and the rent went to W"
    );
}

#[test]
fn cancel_rejects_foreign_owner_and_non_token_beneficiary_token() {
    let (mut env, id, beneficiary_token, _tge) = setup(VAULT_FUNDED);
    let owner = env.actors.owner.insecure_clone();
    let rent_collector = env.actors.rent_collector.pubkey();
    let win_mint = env.win_mint;
    // An outside wallet's ATA: the mint is correct and the account is NOT under the
    // program's custody, but owner ≠ W - the core of finding #3 (the
    // `owner != vault_authority` condition used to let it through). `stranger` is not a
    // token account at all: the slot's type rejects it.
    let foreign = env.actors.beneficiary.pubkey();
    let foreign_ata = env.create_ata(&foreign, &win_mint);
    let stranger_wallet = env.actors.stranger.pubkey();
    let before = snapshot(&env, id, &beneficiary_token);

    for (label, slot, code) in [
        (
            "an outside wallet's ATA (owner ≠ W)",
            foreign_ata,
            err(VestingError::InvalidBeneficiary),
        ),
        (
            "an ordinary system wallet (not a token account)",
            stranger_wallet,
            u32::from(ErrorCode::AccountOwnedByWrongProgram),
        ),
    ] {
        env.expire_blockhash();
        let ix = cancel_vesting_ix(owner.pubkey(), id, slot, rent_collector, win_mint);
        let r = send(&mut env, ix, &owner);
        assert_eq!(
            custom_error_code(&r),
            Some(code),
            "{label} in the beneficiary_token slot: {r:?}"
        );
        assert_eq!(
            snapshot(&env, id, &beneficiary_token),
            before,
            "{label}: the rejection did not touch the state"
        );
    }
    assert_eq!(
        env.token_balance(&foreign_ata),
        0,
        "nothing went to the outside ATA"
    );

    // A POSITIVE: the canonical beneficiary_token (W's ATA) is accepted.
    env.expire_blockhash();
    let ix = cancel_ix(&env, id, beneficiary_token);
    send(&mut env, ix, &owner).expect("the single recipient W's ATA is accepted");
    assert_eq!(
        env.token_balance(&beneficiary_token),
        BENEFICIARY_PREFUNDED + VAULT_FUNDED
    );
}

#[test]
fn close_rejects_invalid_rent_collector() {
    let (mut env, id, beneficiary_token, tge_ts) = setup(REFERENCE + VAULT_EXCESS);
    let owner = env.actors.owner.insecure_clone();
    let win_mint = env.win_mint;
    claim_everything(&mut env, id, tge_ts);
    let before = snapshot(&env, id, &beneficiary_token);

    for (label, bad) in [
        ("an outside system wallet ≠ W", env.actors.stranger.pubkey()),
        ("the data-less vault_authority PDA", vault_authority_pda()),
    ] {
        env.expire_blockhash();
        let ix = close_vesting_ix(owner.pubkey(), id, beneficiary_token, bad, win_mint);
        let r = send(&mut env, ix, &owner);
        assert_eq!(
            custom_error_code(&r),
            Some(err(VestingError::InvalidRentCollector)),
            "{label} in close's rent_collector slot → InvalidRentCollector (#3): {r:?}"
        );
        assert_eq!(
            snapshot(&env, id, &beneficiary_token),
            before,
            "{label}: the rejection did not touch the state"
        );
    }

    // A POSITIVE: rent_collector == W is accepted.
    env.expire_blockhash();
    let ix = close_ix(&env, id, beneficiary_token);
    send(&mut env, ix, &owner).expect("rent_collector == W is accepted");
    assert!(!env.vesting_exists(id) && !env.vault_exists(id));
}

#[test]
fn close_rejects_foreign_owner_and_non_token_beneficiary_token() {
    let (mut env, id, beneficiary_token, tge_ts) = setup(REFERENCE + VAULT_EXCESS);
    let owner = env.actors.owner.insecure_clone();
    let rent_collector = env.actors.rent_collector.pubkey();
    let win_mint = env.win_mint;
    let foreign = env.actors.beneficiary.pubkey();
    let foreign_ata = env.create_ata(&foreign, &win_mint);
    let stranger_wallet = env.actors.stranger.pubkey();
    claim_everything(&mut env, id, tge_ts);
    let before = snapshot(&env, id, &beneficiary_token);

    for (label, slot, code) in [
        (
            "an outside wallet's ATA (owner ≠ W)",
            foreign_ata,
            err(VestingError::InvalidBeneficiary),
        ),
        (
            "an ordinary system wallet (not a token account)",
            stranger_wallet,
            u32::from(ErrorCode::AccountOwnedByWrongProgram),
        ),
    ] {
        env.expire_blockhash();
        let ix = close_vesting_ix(owner.pubkey(), id, slot, rent_collector, win_mint);
        let r = send(&mut env, ix, &owner);
        assert_eq!(
            custom_error_code(&r),
            Some(code),
            "{label} in close's beneficiary_token slot: {r:?}"
        );
        assert_eq!(
            snapshot(&env, id, &beneficiary_token),
            before,
            "{label}: the rejection did not touch the state"
        );
    }
    assert_eq!(
        env.token_balance(&foreign_ata),
        0,
        "nothing went to the outside ATA"
    );

    // A POSITIVE: the canonical beneficiary_token (W's ATA) is accepted.
    env.expire_blockhash();
    let ix = close_ix(&env, id, beneficiary_token);
    send(&mut env, ix, &owner).expect("the single recipient W's ATA is accepted");
    assert_eq!(
        env.token_balance(&beneficiary_token),
        BENEFICIARY_PREFUNDED + VAULT_EXCESS
    );
}
