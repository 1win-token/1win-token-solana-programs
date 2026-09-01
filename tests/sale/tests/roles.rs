//! The behaviour of the role instructions (Arch §9.4) - the positive cases (rotation
//! works) plus the negatives (a zero pubkey). tests-bind-to-real-code: a real call,
//! an assertion of the stored `SaleState.<role>` field, and a specific code.
//!
//! The owner handover is two-step (G-3): `propose_owner(new)` names the candidate and
//! `accept_owner()` is executed by the NEW key, thereby proving it is controlled.
//! Until accept the rights stay with the previous owner. A handover in flight is
//! withdrawn by `cancel_owner_proposal()`. `set_admin` / `set_operator` remain
//! one-step - a mistaken assignment there is reversible by the owner itself.

use anchor_lang::error::ERROR_CODE_OFFSET;
use sale::errors::SaleError;
use sale::events::OwnerProposalCancelled;
use sale_test_common::{
    accept_owner_ix, cancel_owner_proposal_ix, custom_error_code, decode_event, propose_owner_ix,
    set_admin_ix, set_operator_ix, TestEnv,
};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

fn err(variant: SaleError) -> u32 {
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

// ===================================================================
// POSITIVE: the full propose → accept path. Before accept the rights are with the
// previous owner; afterwards they are with the new one, and the previous owner loses
// them.
// ===================================================================

#[test]
fn owner_handover_two_step_transfers_control() {
    let mut env = TestEnv::new();
    let old_owner = env.actors.owner.insecure_clone();
    env.init_default_sale();

    // The candidate is a fresh funded wallet: it is the one that signs accept.
    let new_owner = env.new_user();

    // --- Step 1: the owner names a candidate ---
    send(
        &mut env,
        propose_owner_ix(old_owner.pubkey(), new_owner.pubkey()),
        &old_owner,
    )
    .expect("owner: propose_owner passes");
    let s = env.sale_state();
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
        set_admin_ix(new_owner.pubkey(), Pubkey::new_unique()),
        &new_owner,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::Unauthorized)),
        "the candidate before accept → Unauthorized: {r:?}"
    );

    // Before accept the previous owner keeps its rights.
    env.expire_blockhash();
    let interim_admin = Pubkey::new_unique();
    send(
        &mut env,
        set_admin_ix(old_owner.pubkey(), interim_admin),
        &old_owner,
    )
    .expect("the previous owner still works before accept");
    assert_eq!(env.sale_state().admin, interim_admin);

    // --- Step 2: the candidate accepts the role with its own signature ---
    env.expire_blockhash();
    send(&mut env, accept_owner_ix(new_owner.pubkey()), &new_owner)
        .expect("the candidate: accept_owner passes");
    let s = env.sale_state();
    assert_eq!(
        s.owner,
        new_owner.pubkey(),
        "owner was updated in the state"
    );
    assert_eq!(
        s.pending_owner, None,
        "pending_owner was cleared after accept"
    );

    // The new owner CAN invoke an owner-only instruction.
    env.expire_blockhash();
    let fresh_admin = Pubkey::new_unique();
    send(
        &mut env,
        set_admin_ix(new_owner.pubkey(), fresh_admin),
        &new_owner,
    )
    .expect("the new owner: set_admin passes");
    assert_eq!(
        env.sale_state().admin,
        fresh_admin,
        "the new owner really did change the admin"
    );

    // The previous owner can NO longer do so - Unauthorized.
    env.expire_blockhash();
    let r = send(
        &mut env,
        propose_owner_ix(old_owner.pubkey(), Pubkey::new_unique()),
        &old_owner,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::Unauthorized)),
        "the previous owner after the handover → Unauthorized: {r:?}"
    );
}

// ===================================================================
// The NEGATIVES of the second step: accept with no candidate, and accept by a
// foreign key.
// ===================================================================

#[test]
fn accept_owner_without_pending_rejected() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    env.init_default_sale();
    assert_eq!(
        env.sale_state().pending_owner,
        None,
        "after initialize there is no handover in flight"
    );

    // No candidate was nominated - there is nothing to accept.
    let stranger = env.new_user();
    env.expire_blockhash();
    let r = send(&mut env, accept_owner_ix(stranger.pubkey()), &stranger);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::NoPendingOwner)),
        "accept with no pending → NoPendingOwner: {r:?}"
    );
    assert_eq!(
        env.sale_state().owner,
        owner.pubkey(),
        "the owner did not change"
    );
}

#[test]
fn accept_owner_by_wrong_key_rejected() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    env.init_default_sale();

    let candidate = env.new_user();
    let stranger = env.new_user();
    send(
        &mut env,
        propose_owner_ix(owner.pubkey(), candidate.pubkey()),
        &owner,
    )
    .expect("propose_owner passes");

    // Someone other than the nominee tries to accept.
    env.expire_blockhash();
    let r = send(&mut env, accept_owner_ix(stranger.pubkey()), &stranger);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::Unauthorized)),
        "accept by a foreign key → Unauthorized: {r:?}"
    );
    let s = env.sale_state();
    assert_eq!(s.owner, owner.pubkey(), "the owner did not change");
    assert_eq!(
        s.pending_owner,
        Some(candidate.pubkey()),
        "the candidate stayed the same"
    );

    // The nominated candidate can still accept the role.
    env.expire_blockhash();
    send(&mut env, accept_owner_ix(candidate.pubkey()), &candidate)
        .expect("the nominated candidate: accept passes");
    assert_eq!(env.sale_state().owner, candidate.pubkey());
}

// ===================================================================
// What the scheme was introduced for: a typo into an address with no private key
// does not give away control - the owner stays and nominates again.
// ===================================================================

#[test]
fn owner_survives_handover_to_unusable_address() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    env.init_default_sale();

    // The address is well-formed but has no private key behind it - accept cannot be
    // signed with it, so the second step will never be executed by anyone.
    let typo = Pubkey::new_unique();
    send(&mut env, propose_owner_ix(owner.pubkey(), typo), &owner)
        .expect("propose_owner to the mistaken address passes");
    assert_eq!(env.sale_state().pending_owner, Some(typo));

    // The rights did not move: the owner is unchanged and works.
    assert_eq!(
        env.sale_state().owner,
        owner.pubkey(),
        "the owner did not change"
    );
    env.expire_blockhash();
    let probe_admin = Pubkey::new_unique();
    send(&mut env, set_admin_ix(owner.pubkey(), probe_admin), &owner)
        .expect("the owner kept its rights after the mistaken propose");
    assert_eq!(env.sale_state().admin, probe_admin);

    // The mistake is fixed by nominating again - the previous candidate is displaced.
    let real = env.new_user();
    env.expire_blockhash();
    send(
        &mut env,
        propose_owner_ix(owner.pubkey(), real.pubkey()),
        &owner,
    )
    .expect("a repeated propose passes");
    assert_eq!(
        env.sale_state().pending_owner,
        Some(real.pubkey()),
        "the mistaken candidate was overwritten"
    );

    // The handover runs to completion with a working key this time.
    env.expire_blockhash();
    send(&mut env, accept_owner_ix(real.pubkey()), &real)
        .expect("the new candidate accepted the role");
    assert_eq!(
        env.sale_state().owner,
        real.pubkey(),
        "the handover is complete"
    );
}

/// A repeated `propose` displaces the previous candidate: pending holds exactly one,
/// and only that one can accept the role.
#[test]
fn re_propose_revokes_previous_candidate() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    env.init_default_sale();

    let first = env.new_user();
    let second = env.new_user();

    send(
        &mut env,
        propose_owner_ix(owner.pubkey(), first.pubkey()),
        &owner,
    )
    .expect("the first propose");
    env.expire_blockhash();
    send(
        &mut env,
        propose_owner_ix(owner.pubkey(), second.pubkey()),
        &owner,
    )
    .expect("the second propose");

    // The first candidate was displaced - its signature no longer fits.
    env.expire_blockhash();
    let r = send(&mut env, accept_owner_ix(first.pubkey()), &first);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::Unauthorized)),
        "the displaced candidate → Unauthorized: {r:?}"
    );
    assert_eq!(
        env.sale_state().owner,
        owner.pubkey(),
        "the owner did not change"
    );

    // The standing candidate accepts the role.
    env.expire_blockhash();
    send(&mut env, accept_owner_ix(second.pubkey()), &second)
        .expect("the second candidate accepted the role");
    assert_eq!(env.sale_state().owner, second.pubkey());
}

// ===================================================================
// Withdrawing a handover in flight: the owner removes the candidate and it can no
// longer accept the role. Without a reset the nominated key could accept it at any
// time.
// ===================================================================

#[test]
fn cancel_owner_proposal_clears_pending_and_blocks_candidate() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    env.init_default_sale();

    let candidate = env.new_user();
    send(
        &mut env,
        propose_owner_ix(owner.pubkey(), candidate.pubkey()),
        &owner,
    )
    .expect("propose_owner passes");
    assert_eq!(
        env.sale_state().pending_owner,
        Some(candidate.pubkey()),
        "the candidate is nominated"
    );

    // --- The withdrawal ---
    env.expire_blockhash();
    let r = send(&mut env, cancel_owner_proposal_ix(owner.pubkey()), &owner);
    assert!(r.is_ok(), "owner: cancel_owner_proposal passes: {r:?}");
    let _ev: OwnerProposalCancelled =
        decode_event(&r).expect("the OwnerProposalCancelled event is emitted");
    let s = env.sale_state();
    assert_eq!(s.pending_owner, None, "pending_owner was cleared");
    assert_eq!(s.owner, owner.pubkey(), "the owner did not change");

    // The withdrawn candidate can no longer accept the role.
    env.expire_blockhash();
    let r = send(&mut env, accept_owner_ix(candidate.pubkey()), &candidate);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::NoPendingOwner)),
        "accept after the withdrawal → NoPendingOwner: {r:?}"
    );
    assert_eq!(
        env.sale_state().owner,
        owner.pubkey(),
        "the owner did not change"
    );

    // The owner kept its rights and can start the handover afresh.
    env.expire_blockhash();
    let real = env.new_user();
    send(
        &mut env,
        propose_owner_ix(owner.pubkey(), real.pubkey()),
        &owner,
    )
    .expect("after the withdrawal propose passes again");
    env.expire_blockhash();
    send(&mut env, accept_owner_ix(real.pubkey()), &real)
        .expect("the new candidate accepted the role");
    assert_eq!(
        env.sale_state().owner,
        real.pubkey(),
        "the handover is complete"
    );
}

#[test]
fn cancel_owner_proposal_without_pending_rejected() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    env.init_default_sale();
    assert_eq!(
        env.sale_state().pending_owner,
        None,
        "after initialize there is no handover in flight"
    );

    // There is nothing to withdraw - the instruction rejects rather than passing
    // vacuously.
    env.expire_blockhash();
    let r = send(&mut env, cancel_owner_proposal_ix(owner.pubkey()), &owner);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::NoPendingOwner)),
        "cancel with no pending → NoPendingOwner: {r:?}"
    );

    // A repeated withdrawal after a successful one is also rejected (it is not an
    // idempotent no-op).
    let candidate = env.new_user();
    env.expire_blockhash();
    send(
        &mut env,
        propose_owner_ix(owner.pubkey(), candidate.pubkey()),
        &owner,
    )
    .expect("propose_owner passes");
    env.expire_blockhash();
    send(&mut env, cancel_owner_proposal_ix(owner.pubkey()), &owner)
        .expect("the first withdrawal passes");
    env.expire_blockhash();
    let r = send(&mut env, cancel_owner_proposal_ix(owner.pubkey()), &owner);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::NoPendingOwner)),
        "the second withdrawal → NoPendingOwner: {r:?}"
    );
    assert_eq!(
        env.sale_state().owner,
        owner.pubkey(),
        "the owner did not change"
    );
}

/// Only the owner may withdraw a handover: a nominated candidate cannot withdraw
/// itself (a role is revoked by whoever granted it).
#[test]
fn cancel_owner_proposal_by_candidate_rejected() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    env.init_default_sale();

    let candidate = env.new_user();
    send(
        &mut env,
        propose_owner_ix(owner.pubkey(), candidate.pubkey()),
        &owner,
    )
    .expect("propose_owner passes");

    env.expire_blockhash();
    let r = send(
        &mut env,
        cancel_owner_proposal_ix(candidate.pubkey()),
        &candidate,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::Unauthorized)),
        "cancel by the candidate → Unauthorized: {r:?}"
    );
    assert_eq!(
        env.sale_state().pending_owner,
        Some(candidate.pubkey()),
        "the nomination stands"
    );

    // The candidate can still accept the role - the rejection was purely about the role.
    env.expire_blockhash();
    send(&mut env, accept_owner_ix(candidate.pubkey()), &candidate)
        .expect("the candidate: accept passes");
    assert_eq!(env.sale_state().owner, candidate.pubkey());
}

#[test]
fn set_admin_positive_updates_field() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    env.init_default_sale();

    let new_admin = Pubkey::new_unique();
    send(&mut env, set_admin_ix(owner.pubkey(), new_admin), &owner)
        .expect("owner: set_admin passes");
    assert_eq!(
        env.sale_state().admin,
        new_admin,
        "admin was updated in the state"
    );
}

#[test]
fn set_operator_positive_updates_field() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    env.init_default_sale();

    let new_operator = Pubkey::new_unique();
    send(
        &mut env,
        set_operator_ix(owner.pubkey(), new_operator),
        &owner,
    )
    .expect("owner: set_operator passes");
    assert_eq!(
        env.sale_state().operator,
        new_operator,
        "operator was updated in the state"
    );
}

// ===================================================================
// NEGATIVE: a zero pubkey → InvalidAuthority (a zero role means an unmanageable
// account).
// ===================================================================

#[test]
fn set_role_rejects_zero_pubkey() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    env.init_default_sale();

    let before = env.sale_state();

    // propose_owner(default)
    env.expire_blockhash();
    let r = send(
        &mut env,
        propose_owner_ix(owner.pubkey(), Pubkey::default()),
        &owner,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::InvalidAuthority)),
        "propose_owner(default) → InvalidAuthority: {r:?}"
    );

    // set_admin(default)
    env.expire_blockhash();
    let r = send(
        &mut env,
        set_admin_ix(owner.pubkey(), Pubkey::default()),
        &owner,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::InvalidAuthority)),
        "set_admin(default) → InvalidAuthority: {r:?}"
    );

    // set_operator(default)
    env.expire_blockhash();
    let r = send(
        &mut env,
        set_operator_ix(owner.pubkey(), Pubkey::default()),
        &owner,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::InvalidAuthority)),
        "set_operator(default) → InvalidAuthority: {r:?}"
    );

    // Not a single field changed.
    let after = env.sale_state();
    assert_eq!(after.owner, before.owner, "owner is untouched");
    assert_eq!(after.pending_owner, None, "no candidate is nominated");
    assert_eq!(after.admin, before.admin, "admin is untouched");
    assert_eq!(after.operator, before.operator, "operator is untouched");
}
