//! The Burn program's `set_limits` / owner handover / `set_admin` / `set_operator` -
//! the positive cases (the state field, the event, and the effect on a subsequent
//! `burn`) and the negatives (the wrong role, a zero key).
//!
//! The owner handover is two-step: `propose_owner(new)` names the candidate and
//! `accept_owner()` is executed by the NEW key, thereby proving it is controlled.
//! Until accept the rights stay with the previous owner. A handover in flight is
//! withdrawn by `cancel_owner_proposal()`. `set_admin` / `set_operator` are one-step -
//! a mistaken assignment is reversible by the owner itself.
//!
//! The separation of roles after handing `burn` to the operator: Admin (the ops
//! multisig) owns ONLY the parameters (`set_limits`) and the Operator (the automation
//! hot key) ONLY the execution of a burn; the permission sets are disjoint (BRD G-1).
//!
//! tests-bind-to-real-code: a real call, an assertion of the stored
//! `BurnState.<...>` field and of a specific error code; every guard gets a positive
//! and a negative within one config.

use anchor_lang::error::ERROR_CODE_OFFSET;
use burn::errors::BurnError;
use burn::events::{LimitsUpdated, OwnerProposalCancelled, RoleKind, RoleUpdated};
use burn_tests::{
    burn_accept_owner_ix, burn_cancel_owner_proposal_ix, burn_ix, burn_propose_owner_ix,
    burn_set_admin_ix, burn_set_limits_ix, burn_set_operator_ix, spl_burn_ix, vault_pda,
    BurnTestEnv,
};
use sale_test_common::{custom_error_code, decode_event, TestEnv};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use spl_token_interface::error::TokenError;

/// The time base (the index derivation is in burn_exec.rs): day 23_149, week 3_307 -
/// the expected accumulator window indices after a burn.
const BASE_TS: i64 = 2_000_077_200;
const DAY0: u64 = 23_149;
const WEEK0: u64 = 3_307;

fn err(variant: BurnError) -> u32 {
    ERROR_CODE_OFFSET + variant as u32
}

// We return the native litesvm TransactionResult as is (as `send_ixs` does).
#[allow(clippy::result_large_err)]
fn send(
    env: &mut TestEnv,
    ix: solana_instruction::Instruction,
    signer: &Keypair,
) -> litesvm::types::TransactionResult {
    env.send_ixs(&[ix], &signer.pubkey(), &[signer])
}

/// Mints `amount` units straight into the vault (created by initialize) and returns
/// its address.
fn fund_vault(env: &mut TestEnv, amount: u64) -> Pubkey {
    let mint = env.win_mint;
    let vault = vault_pda();
    env.mint_to(&mint, &vault, amount);
    vault
}

// ===================================================================
// set_limits: the positive case plus proof that the new limits really apply to burn.
// ===================================================================

#[test]
fn set_limits_updates_state_emits_event_and_applies_to_burn() {
    // The old limits: daily 10 tokens (1e9), weekly 100 (1e10). Admin changes the limits
    // and Operator burns - different roles, different signatures.
    let mut env = TestEnv::new();
    env.warp_to(BASE_TS);
    env.init_burn_with_limits(1_000_000_000, 10_000_000_000);
    let admin = env.actors.admin.insecure_clone();
    let operator = env.actors.operator.insecure_clone();
    let kettle = fund_vault(&mut env, 100_000_000_000);
    let win = env.win_mint;

    // 20 tokens is above the old daily limit - rejected.
    let r = send(
        &mut env,
        burn_ix(operator.pubkey(), kettle, win, 2_000_000_000),
        &operator,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(BurnError::DailyLimitExceeded)),
        "a burn above the old limit → DailyLimitExceeded: {r:?}"
    );

    // Admin raises the limits: daily 50 tokens, the weekly one unchanged.
    let r = send(
        &mut env,
        burn_set_limits_ix(admin.pubkey(), 5_000_000_000, 10_000_000_000),
        &admin,
    );
    r.as_ref().expect("admin: set_limits passes");
    let s = env.burn_state();
    assert_eq!(s.daily_limit, 5_000_000_000, "daily_limit was updated");
    assert_eq!(s.weekly_limit, 10_000_000_000, "weekly_limit was updated");
    let ev: LimitsUpdated = decode_event(&r).expect("the LimitsUpdated event");
    assert_eq!(ev.daily_limit, 5_000_000_000, "event.daily_limit");
    assert_eq!(ev.weekly_limit, 10_000_000_000, "event.weekly_limit");

    // The same burn now passes under the NEW limits.
    env.expire_blockhash();
    send(
        &mut env,
        burn_ix(operator.pubkey(), kettle, win, 2_000_000_000),
        &operator,
    )
    .expect("a burn under the new limits passes");
    let c = env.burn_counters();
    assert_eq!(
        c.day.index, DAY0,
        "day.index after the burn under the new limits"
    );
    assert_eq!(
        c.day.burned, 2_000_000_000,
        "day.burned under the new limits"
    );
    assert_eq!(
        c.week.index, WEEK0,
        "week.index after the burn under the new limits"
    );
    assert_eq!(
        c.week.burned, 2_000_000_000,
        "week.burned under the new limits"
    );
}

// ===================================================================
// set_limits: the negatives - a non-admin, zero values.
// ===================================================================

#[test]
fn set_limits_rejects_non_admin() {
    let mut env = TestEnv::new();
    env.init_default_burn();
    let owner = env.actors.owner.insecure_clone();
    let operator = env.actors.operator.insecure_clone();
    let stranger = env.new_user();
    let before = env.burn_state();

    // Owner is NOT admin: the role separation holds even for "our own" keys.
    let r = send(&mut env, burn_set_limits_ix(owner.pubkey(), 1, 1), &owner);
    assert_eq!(
        custom_error_code(&r),
        Some(err(BurnError::Unauthorized)),
        "owner in set_limits → Unauthorized: {r:?}"
    );

    // Operator executes the burn but does NOT own the parameters (BRD G-1).
    let r = send(
        &mut env,
        burn_set_limits_ix(operator.pubkey(), 1, 1),
        &operator,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(BurnError::Unauthorized)),
        "operator in set_limits → Unauthorized: {r:?}"
    );

    // An unrelated wallet.
    let r = send(
        &mut env,
        burn_set_limits_ix(stranger.pubkey(), 1, 1),
        &stranger,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(BurnError::Unauthorized)),
        "an outsider in set_limits → Unauthorized: {r:?}"
    );

    let after = env.burn_state();
    assert_eq!(after.daily_limit, before.daily_limit, "daily is untouched");
    assert_eq!(
        after.weekly_limit, before.weekly_limit,
        "weekly is untouched"
    );
}

#[test]
fn set_limits_rejects_zero_values() {
    let mut env = TestEnv::new();
    env.init_default_burn();
    let admin = env.actors.admin.insecure_clone();
    let before = env.burn_state();

    // daily == 0.
    let r = send(
        &mut env,
        burn_set_limits_ix(admin.pubkey(), 0, 10_000_000_000),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(BurnError::InvalidParams)),
        "daily == 0 → InvalidParams: {r:?}"
    );

    // weekly == 0.
    let r = send(
        &mut env,
        burn_set_limits_ix(admin.pubkey(), 10_000_000_000, 0),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(BurnError::InvalidParams)),
        "weekly == 0 → InvalidParams: {r:?}"
    );

    let after = env.burn_state();
    assert_eq!(after.daily_limit, before.daily_limit, "daily is untouched");
    assert_eq!(
        after.weekly_limit, before.weekly_limit,
        "weekly is untouched"
    );
}

// ===================================================================
// The owner handover: the full propose → accept path. Before accept the rights are
// with the previous owner; afterwards they are with the new one, and the previous
// owner loses them.
// ===================================================================

#[test]
fn owner_handover_two_step_transfers_control() {
    let mut env = TestEnv::new();
    env.init_default_burn();
    let old_owner = env.actors.owner.insecure_clone();

    // The candidate is a fresh funded wallet: it is the one that signs accept.
    let new_owner = env.new_user();

    // --- Step 1: the owner names a candidate ---
    send(
        &mut env,
        burn_propose_owner_ix(old_owner.pubkey(), new_owner.pubkey()),
        &old_owner,
    )
    .expect("owner: propose_owner passes");
    let s = env.burn_state();
    assert_eq!(
        s.pending_owner,
        Some(new_owner.pubkey()),
        "the candidate was written into pending_owner"
    );
    assert_eq!(
        s.owner,
        old_owner.pubkey(),
        "the owner is still the previous one - propose hands over no rights"
    );

    // Before accept the candidate holds NO owner rights.
    env.expire_blockhash();
    let r = send(
        &mut env,
        burn_set_admin_ix(new_owner.pubkey(), Pubkey::new_unique()),
        &new_owner,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(BurnError::Unauthorized)),
        "the candidate before accept → Unauthorized: {r:?}"
    );

    // Before accept the previous owner keeps its rights.
    env.expire_blockhash();
    let interim_admin = Pubkey::new_unique();
    send(
        &mut env,
        burn_set_admin_ix(old_owner.pubkey(), interim_admin),
        &old_owner,
    )
    .expect("the previous owner still works before accept");
    assert_eq!(env.burn_state().admin, interim_admin);

    // --- Step 2: the candidate accepts the role with its own signature ---
    env.expire_blockhash();
    let r = send(
        &mut env,
        burn_accept_owner_ix(new_owner.pubkey()),
        &new_owner,
    );
    r.as_ref().expect("the candidate: accept_owner passes");
    let s = env.burn_state();
    assert_eq!(
        s.owner,
        new_owner.pubkey(),
        "owner was updated in the state"
    );
    assert_eq!(
        s.pending_owner, None,
        "pending_owner was cleared after accept"
    );
    let ev: RoleUpdated = decode_event(&r).expect("the RoleUpdated event");
    assert!(
        matches!(ev.role, RoleKind::Owner),
        "event.role == RoleKind::Owner"
    );
    assert_eq!(ev.new_pubkey, new_owner.pubkey(), "event.new_pubkey");

    // The new owner CAN run an owner-only instruction (set_admin).
    env.expire_blockhash();
    let fresh_admin = Pubkey::new_unique();
    send(
        &mut env,
        burn_set_admin_ix(new_owner.pubkey(), fresh_admin),
        &new_owner,
    )
    .expect("the new owner: set_admin passes");
    assert_eq!(
        env.burn_state().admin,
        fresh_admin,
        "the new owner really did change the admin"
    );

    // The old owner can NO longer do so.
    env.expire_blockhash();
    let r = send(
        &mut env,
        burn_propose_owner_ix(old_owner.pubkey(), Pubkey::new_unique()),
        &old_owner,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(BurnError::Unauthorized)),
        "the old owner after the handover → Unauthorized: {r:?}"
    );
}

// ===================================================================
// The negatives of the second step: accept with no candidate, and accept by a
// foreign key.
// ===================================================================

#[test]
fn accept_owner_without_pending_rejected() {
    let mut env = TestEnv::new();
    env.init_default_burn();
    let owner = env.actors.owner.insecure_clone();
    assert_eq!(
        env.burn_state().pending_owner,
        None,
        "after initialize there is no handover in flight"
    );

    // No candidate was nominated - there is nothing to accept.
    let stranger = env.new_user();
    env.expire_blockhash();
    let r = send(&mut env, burn_accept_owner_ix(stranger.pubkey()), &stranger);
    assert_eq!(
        custom_error_code(&r),
        Some(err(BurnError::NoPendingOwner)),
        "accept with no pending → NoPendingOwner: {r:?}"
    );
    assert_eq!(
        env.burn_state().owner,
        owner.pubkey(),
        "the owner did not change"
    );
}

#[test]
fn accept_owner_by_wrong_key_rejected() {
    let mut env = TestEnv::new();
    env.init_default_burn();
    let owner = env.actors.owner.insecure_clone();

    let candidate = env.new_user();
    let stranger = env.new_user();
    send(
        &mut env,
        burn_propose_owner_ix(owner.pubkey(), candidate.pubkey()),
        &owner,
    )
    .expect("propose_owner passes");

    // Someone other than the nominee tries to accept.
    env.expire_blockhash();
    let r = send(&mut env, burn_accept_owner_ix(stranger.pubkey()), &stranger);
    assert_eq!(
        custom_error_code(&r),
        Some(err(BurnError::Unauthorized)),
        "accept by a foreign key → Unauthorized: {r:?}"
    );
    let s = env.burn_state();
    assert_eq!(s.owner, owner.pubkey(), "the owner did not change");
    assert_eq!(
        s.pending_owner,
        Some(candidate.pubkey()),
        "the candidate stayed the same"
    );

    // The nominated candidate can still accept the role.
    env.expire_blockhash();
    send(
        &mut env,
        burn_accept_owner_ix(candidate.pubkey()),
        &candidate,
    )
    .expect("the nominated candidate: accept passes");
    assert_eq!(env.burn_state().owner, candidate.pubkey());
}

// ===================================================================
// What the scheme was introduced for: a typo into an address with no private key
// does not give away control - the owner stays and nominates again.
// ===================================================================

#[test]
fn owner_survives_handover_to_unusable_address() {
    let mut env = TestEnv::new();
    env.init_default_burn();
    let owner = env.actors.owner.insecure_clone();

    // The address is well-formed but has no private key behind it - accept cannot be
    // signed with it, so the second step will never be executed by anyone.
    let typo = Pubkey::new_unique();
    send(
        &mut env,
        burn_propose_owner_ix(owner.pubkey(), typo),
        &owner,
    )
    .expect("propose_owner to the mistaken address passes");
    let s = env.burn_state();
    assert_eq!(
        s.pending_owner,
        Some(typo),
        "the mistaken candidate is nominated"
    );
    assert_eq!(s.owner, owner.pubkey(), "the owner did not change");

    // The rights did not move: the owner is unchanged and works.
    env.expire_blockhash();
    let probe_admin = Pubkey::new_unique();
    send(
        &mut env,
        burn_set_admin_ix(owner.pubkey(), probe_admin),
        &owner,
    )
    .expect("the owner kept its rights after the mistaken propose");
    assert_eq!(env.burn_state().admin, probe_admin);

    // The mistake is fixed by nominating again - the previous candidate is displaced.
    let real = env.new_user();
    env.expire_blockhash();
    send(
        &mut env,
        burn_propose_owner_ix(owner.pubkey(), real.pubkey()),
        &owner,
    )
    .expect("a repeated propose passes");
    assert_eq!(
        env.burn_state().pending_owner,
        Some(real.pubkey()),
        "the mistaken candidate was overwritten"
    );

    // The handover runs to completion with a working key this time.
    env.expire_blockhash();
    send(&mut env, burn_accept_owner_ix(real.pubkey()), &real)
        .expect("the new candidate accepted the role");
    assert_eq!(
        env.burn_state().owner,
        real.pubkey(),
        "the handover is complete"
    );
}

/// A repeated `propose` displaces the previous candidate: pending holds exactly one,
/// and only that one can accept the role.
#[test]
fn re_propose_revokes_previous_candidate() {
    let mut env = TestEnv::new();
    env.init_default_burn();
    let owner = env.actors.owner.insecure_clone();

    let first = env.new_user();
    let second = env.new_user();

    send(
        &mut env,
        burn_propose_owner_ix(owner.pubkey(), first.pubkey()),
        &owner,
    )
    .expect("the first propose");
    env.expire_blockhash();
    send(
        &mut env,
        burn_propose_owner_ix(owner.pubkey(), second.pubkey()),
        &owner,
    )
    .expect("the second propose");

    // The first candidate was displaced - its signature no longer fits.
    env.expire_blockhash();
    let r = send(&mut env, burn_accept_owner_ix(first.pubkey()), &first);
    assert_eq!(
        custom_error_code(&r),
        Some(err(BurnError::Unauthorized)),
        "the displaced candidate → Unauthorized: {r:?}"
    );
    assert_eq!(
        env.burn_state().owner,
        owner.pubkey(),
        "the owner did not change"
    );

    // The standing candidate accepts the role.
    env.expire_blockhash();
    send(&mut env, burn_accept_owner_ix(second.pubkey()), &second)
        .expect("the second candidate accepted the role");
    assert_eq!(env.burn_state().owner, second.pubkey());
}

// ===================================================================
// Withdrawing a handover in flight: the owner removes the candidate and it can no
// longer accept the role. Without a reset the nominated key could accept it at any
// time.
// ===================================================================

#[test]
fn cancel_owner_proposal_clears_pending_and_blocks_candidate() {
    let mut env = TestEnv::new();
    env.init_default_burn();
    let owner = env.actors.owner.insecure_clone();

    let candidate = env.new_user();
    send(
        &mut env,
        burn_propose_owner_ix(owner.pubkey(), candidate.pubkey()),
        &owner,
    )
    .expect("propose_owner passes");
    assert_eq!(
        env.burn_state().pending_owner,
        Some(candidate.pubkey()),
        "the candidate is nominated"
    );

    // --- The withdrawal ---
    env.expire_blockhash();
    let r = send(
        &mut env,
        burn_cancel_owner_proposal_ix(owner.pubkey()),
        &owner,
    );
    r.as_ref().expect("owner: cancel_owner_proposal passes");
    let ev: OwnerProposalCancelled =
        decode_event(&r).expect("the OwnerProposalCancelled event is emitted");
    assert_eq!(
        ev.pending_owner,
        candidate.pubkey(),
        "the event names the withdrawn candidate"
    );
    let s = env.burn_state();
    assert_eq!(s.pending_owner, None, "pending_owner was cleared");
    assert_eq!(s.owner, owner.pubkey(), "the owner did not change");

    // The withdrawn candidate can no longer accept the role.
    env.expire_blockhash();
    let r = send(
        &mut env,
        burn_accept_owner_ix(candidate.pubkey()),
        &candidate,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(BurnError::NoPendingOwner)),
        "accept after the withdrawal → NoPendingOwner: {r:?}"
    );
    assert_eq!(
        env.burn_state().owner,
        owner.pubkey(),
        "the owner did not change"
    );

    // The owner kept its rights and can start the handover afresh.
    env.expire_blockhash();
    let real = env.new_user();
    send(
        &mut env,
        burn_propose_owner_ix(owner.pubkey(), real.pubkey()),
        &owner,
    )
    .expect("after the withdrawal propose passes again");
    env.expire_blockhash();
    send(&mut env, burn_accept_owner_ix(real.pubkey()), &real)
        .expect("the new candidate accepted the role");
    assert_eq!(
        env.burn_state().owner,
        real.pubkey(),
        "the handover is complete"
    );
}

#[test]
fn cancel_owner_proposal_without_pending_rejected() {
    let mut env = TestEnv::new();
    env.init_default_burn();
    let owner = env.actors.owner.insecure_clone();
    assert_eq!(
        env.burn_state().pending_owner,
        None,
        "after initialize there is no handover in flight"
    );

    // There is nothing to withdraw - the instruction rejects rather than passing
    // vacuously.
    env.expire_blockhash();
    let r = send(
        &mut env,
        burn_cancel_owner_proposal_ix(owner.pubkey()),
        &owner,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(BurnError::NoPendingOwner)),
        "cancel with no pending → NoPendingOwner: {r:?}"
    );

    // A repeated withdrawal after a successful one is also rejected (it is not an
    // idempotent no-op).
    let candidate = env.new_user();
    env.expire_blockhash();
    send(
        &mut env,
        burn_propose_owner_ix(owner.pubkey(), candidate.pubkey()),
        &owner,
    )
    .expect("propose_owner passes");
    env.expire_blockhash();
    send(
        &mut env,
        burn_cancel_owner_proposal_ix(owner.pubkey()),
        &owner,
    )
    .expect("the first withdrawal passes");
    env.expire_blockhash();
    let r = send(
        &mut env,
        burn_cancel_owner_proposal_ix(owner.pubkey()),
        &owner,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(BurnError::NoPendingOwner)),
        "the second withdrawal → NoPendingOwner: {r:?}"
    );
    assert_eq!(
        env.burn_state().owner,
        owner.pubkey(),
        "the owner did not change"
    );
}

/// Only the owner may withdraw a handover: a nominated candidate cannot withdraw
/// itself (a role is revoked by whoever granted it).
#[test]
fn cancel_owner_proposal_by_candidate_rejected() {
    let mut env = TestEnv::new();
    env.init_default_burn();
    let owner = env.actors.owner.insecure_clone();

    let candidate = env.new_user();
    send(
        &mut env,
        burn_propose_owner_ix(owner.pubkey(), candidate.pubkey()),
        &owner,
    )
    .expect("propose_owner passes");

    env.expire_blockhash();
    let r = send(
        &mut env,
        burn_cancel_owner_proposal_ix(candidate.pubkey()),
        &candidate,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(BurnError::Unauthorized)),
        "cancel by the candidate → Unauthorized: {r:?}"
    );
    assert_eq!(
        env.burn_state().pending_owner,
        Some(candidate.pubkey()),
        "the nomination stands"
    );

    // The candidate can still accept the role - the rejection was purely about the role.
    env.expire_blockhash();
    send(
        &mut env,
        burn_accept_owner_ix(candidate.pubkey()),
        &candidate,
    )
    .expect("the candidate: accept passes");
    assert_eq!(env.burn_state().owner, candidate.pubkey());
}

// ===================================================================
// set_operator: rotation switches the right to burn.
// ===================================================================

#[test]
fn set_operator_rotation_switches_burn_rights() {
    // The operator is a role only - the vault is the program's - so the rotation is
    // complete the moment the state field changes: nothing at the token layer
    // accompanies it, for the old key or for the new one.
    let mut env = TestEnv::new();
    env.warp_to(BASE_TS);
    env.init_default_burn();
    let owner = env.actors.owner.insecure_clone();
    let old_operator = env.actors.operator.insecure_clone();
    let vault = fund_vault(&mut env, 10_000_000_000);
    let win = env.win_mint;

    // The old operator's positive case: burn 10 tokens, the daily window = 1e9.
    send(
        &mut env,
        burn_ix(old_operator.pubkey(), vault, win, 1_000_000_000),
        &old_operator,
    )
    .expect("the old operator: burn passes");
    let c = env.burn_counters();
    assert_eq!(c.day.index, DAY0, "day.index after the old operator's burn");
    assert_eq!(
        c.day.burned, 1_000_000_000,
        "day.burned of the old operator"
    );

    // The owner changes the operator.
    let new_operator = env.new_user();
    let r = send(
        &mut env,
        burn_set_operator_ix(owner.pubkey(), new_operator.pubkey()),
        &owner,
    );
    r.as_ref().expect("owner: set_operator passes");
    assert_eq!(
        env.burn_state().operator,
        new_operator.pubkey(),
        "operator was updated"
    );
    let ev: RoleUpdated = decode_event(&r).expect("the RoleUpdated event");
    assert!(
        matches!(ev.role, RoleKind::Operator),
        "event.role == RoleKind::Operator"
    );
    assert_eq!(ev.new_pubkey, new_operator.pubkey(), "event.new_pubkey");

    // The old operator can NO longer burn through the program (the role's has_one
    // rejects it) ...
    env.expire_blockhash();
    let r = send(
        &mut env,
        burn_ix(old_operator.pubkey(), vault, win, 1_000_000_000),
        &old_operator,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(BurnError::Unauthorized)),
        "the old operator after the rotation → Unauthorized: {r:?}"
    );
    // ... nor around it: the key never held a token-level right over the vault.
    env.expire_blockhash();
    let r = send(
        &mut env,
        spl_burn_ix(vault, win, old_operator.pubkey(), 1_000_000_000),
        &old_operator,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(TokenError::OwnerMismatch as u32),
        "the old operator's direct SPL Burn → OwnerMismatch: {r:?}"
    );
    assert_eq!(
        env.token_balance(&vault),
        9_000_000_000,
        "the vault is untouched by the old operator"
    );

    // The new operator can - out of the same vault, with no token-layer step: a burn
    // of 20 tokens; the accumulator windows run THROUGH (per period, not per
    // operator): 1e9 + 2e9 = 3e9.
    env.expire_blockhash();
    send(
        &mut env,
        burn_ix(new_operator.pubkey(), vault, win, 2_000_000_000),
        &new_operator,
    )
    .expect("the new operator: burn passes");
    let c = env.burn_counters();
    assert_eq!(c.day.index, DAY0, "day.index after the new operator's burn");
    assert_eq!(
        c.day.burned, 3_000_000_000,
        "the daily window runs through for the new operator"
    );
    assert_eq!(
        c.week.index, WEEK0,
        "week.index after the new operator's burn"
    );
    assert_eq!(
        c.week.burned, 3_000_000_000,
        "the weekly window runs through for the new operator"
    );
    assert_eq!(
        env.token_balance(&vault),
        7_000_000_000,
        "the vault after both operators' burns"
    );
}

// ===================================================================
// burn: the right belongs to the Operator ONLY - neither owner, nor admin, nor an
// outsider burns out of the vault (BRD G-1).
// ===================================================================

#[test]
fn burn_rejects_every_role_but_operator() {
    let mut env = TestEnv::new();
    env.warp_to(BASE_TS);
    env.init_default_burn();
    let owner = env.actors.owner.insecure_clone();
    let admin = env.actors.admin.insecure_clone();
    let operator = env.actors.operator.insecure_clone();
    let stranger = env.new_user();
    let vault = fund_vault(&mut env, 10_000_000_000);
    let win = env.win_mint;

    // Everyone addresses the very same vault: the rejection comes from the role check
    // alone (nothing else about the call differs from the operator's).
    for actor in [&owner, &admin, &stranger] {
        env.expire_blockhash();
        let r = send(
            &mut env,
            burn_ix(actor.pubkey(), vault, win, 1_000_000_000),
            actor,
        );
        assert_eq!(
            custom_error_code(&r),
            Some(err(BurnError::Unauthorized)),
            "{} in burn → Unauthorized: {r:?}",
            actor.pubkey()
        );
        assert_eq!(
            env.token_balance(&vault),
            10_000_000_000,
            "the vault is untouched by the rejected role"
        );
    }
    // Nothing was burned: the accumulator holds the init values.
    let c = env.burn_counters();
    assert_eq!(c.day.burned, 0, "day.burned after the rejections");
    assert_eq!(c.week.burned, 0, "week.burned after the rejections");

    // The positive counterpart in the same config: the operator burns.
    env.expire_blockhash();
    send(
        &mut env,
        burn_ix(operator.pubkey(), vault, win, 1_000_000_000),
        &operator,
    )
    .expect("operator: burn passes");
    let c = env.burn_counters();
    assert_eq!(c.day.index, DAY0, "day.index after the operator's burn");
    assert_eq!(
        c.day.burned, 1_000_000_000,
        "day.burned after the operator's burn"
    );
    assert_eq!(c.week.index, WEEK0, "week.index after the operator's burn");
    assert_eq!(
        env.token_balance(&vault),
        9_000_000_000,
        "the vault dropped by the burned amount"
    );
}

// ===================================================================
// set_admin: rotation switches the right to set_limits (not to burn).
// ===================================================================

#[test]
fn set_admin_rotation_switches_limits_rights() {
    let mut env = TestEnv::new();
    env.init_default_burn();
    let owner = env.actors.owner.insecure_clone();
    let old_admin = env.actors.admin.insecure_clone();

    // The old admin's positive case.
    send(
        &mut env,
        burn_set_limits_ix(old_admin.pubkey(), 1_000_000_000, 10_000_000_000),
        &old_admin,
    )
    .expect("the old admin: set_limits passes");
    assert_eq!(
        env.burn_state().daily_limit,
        1_000_000_000,
        "the limit set by the old admin"
    );

    // The owner changes the admin.
    let new_admin = env.new_user();
    let r = send(
        &mut env,
        burn_set_admin_ix(owner.pubkey(), new_admin.pubkey()),
        &owner,
    );
    r.as_ref().expect("owner: set_admin passes");
    assert_eq!(
        env.burn_state().admin,
        new_admin.pubkey(),
        "admin was updated"
    );
    let ev: RoleUpdated = decode_event(&r).expect("the RoleUpdated event");
    assert!(
        matches!(ev.role, RoleKind::Admin),
        "event.role == RoleKind::Admin"
    );
    assert_eq!(ev.new_pubkey, new_admin.pubkey(), "event.new_pubkey");

    // The old admin can NO longer do so.
    env.expire_blockhash();
    let r = send(
        &mut env,
        burn_set_limits_ix(old_admin.pubkey(), 2_000_000_000, 20_000_000_000),
        &old_admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(BurnError::Unauthorized)),
        "the old admin after the rotation → Unauthorized: {r:?}"
    );
    assert_eq!(
        env.burn_state().daily_limit,
        1_000_000_000,
        "the limit was untouched by the rejection"
    );

    // The new admin can.
    env.expire_blockhash();
    send(
        &mut env,
        burn_set_limits_ix(new_admin.pubkey(), 3_000_000_000, 30_000_000_000),
        &new_admin,
    )
    .expect("the new admin: set_limits passes");
    let s = env.burn_state();
    assert_eq!(s.daily_limit, 3_000_000_000, "daily from the new admin");
    assert_eq!(s.weekly_limit, 30_000_000_000, "weekly from the new admin");
}

// ===================================================================
// propose_owner / cancel_owner_proposal / set_admin / set_operator: the negatives -
// a non-owner, a zero key.
// ===================================================================

#[test]
fn set_role_rejects_non_owner_and_zero_pubkey() {
    let mut env = TestEnv::new();
    env.init_default_burn();
    let owner = env.actors.owner.insecure_clone();
    let admin = env.actors.admin.insecure_clone();
    let operator = env.actors.operator.insecure_clone();
    let before = env.burn_state();

    // Admin and operator are NOT owner: every owner instruction rejects them.
    for (label, actor) in [("admin", &admin), ("operator", &operator)] {
        for (ix_label, ix) in [
            (
                "propose_owner",
                burn_propose_owner_ix(actor.pubkey(), Pubkey::new_unique()),
            ),
            (
                "cancel_owner_proposal",
                burn_cancel_owner_proposal_ix(actor.pubkey()),
            ),
            (
                "set_admin",
                burn_set_admin_ix(actor.pubkey(), Pubkey::new_unique()),
            ),
            (
                "set_operator",
                burn_set_operator_ix(actor.pubkey(), Pubkey::new_unique()),
            ),
        ] {
            env.expire_blockhash();
            let r = send(&mut env, ix, actor);
            assert_eq!(
                custom_error_code(&r),
                Some(err(BurnError::Unauthorized)),
                "{label} in {ix_label} → Unauthorized: {r:?}"
            );
        }
    }

    // A zero key from a legitimate owner - InvalidAuthority.
    for (ix_label, ix) in [
        (
            "propose_owner",
            burn_propose_owner_ix(owner.pubkey(), Pubkey::default()),
        ),
        (
            "set_admin",
            burn_set_admin_ix(owner.pubkey(), Pubkey::default()),
        ),
        (
            "set_operator",
            burn_set_operator_ix(owner.pubkey(), Pubkey::default()),
        ),
    ] {
        env.expire_blockhash();
        let r = send(&mut env, ix, &owner);
        assert_eq!(
            custom_error_code(&r),
            Some(err(BurnError::InvalidAuthority)),
            "{ix_label}(default) → InvalidAuthority: {r:?}"
        );
    }

    // Not a single field changed.
    let after = env.burn_state();
    assert_eq!(after.owner, before.owner, "owner is untouched");
    assert_eq!(after.pending_owner, None, "no candidate is nominated");
    assert_eq!(after.admin, before.admin, "admin is untouched");
    assert_eq!(after.operator, before.operator, "operator is untouched");
}
