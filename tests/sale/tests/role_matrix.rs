//! The role matrix on SVM (Arch §8) - for every privileged instruction the correct
//! role passes and any wrong actor gets Unauthorized (6002).
//! tests-bind-to-real-code: a real call, a specific code.
//!
//! A note on the order of the checks (confirmed by execution, not assumed): Anchor
//! verifies `has_one = admin` / `has_one = owner` during account resolution, BEFORE
//! the handler's body. So a wrong signer fails with Unauthorized regardless of the
//! phase. The negatives are placed in PreSale (where the instruction WOULD pass for
//! the correct role - meaning the failure is on the role guard rather than the phase
//! one). The positive case of every Admin instruction is likewise executed in its
//! permitted phase, so that "the correct role passes" is genuinely asserted.
//!
//! ===== DELIBERATELY DEFERRED =====
//! Instructions outside iteration 1 (activate_claim, blacklist_*, refund, purchase,
//! claim_*, withdraw_proceeds, sweep_unclaimed, close_*) - their §8 role rows are
//! covered in the corresponding iterations. Only the implemented ones are here:
//! Admin-only (set_price/emission/tx_limits/sale_start_date/sale_end_date/claim_schedule,
//! pause/unpause) and Owner-only (propose_owner/cancel_owner_proposal/set_admin/
//! set_operator).

use anchor_lang::error::ERROR_CODE_OFFSET;
use sale::errors::SaleError;
use sale_test_common::{
    cancel_owner_proposal_ix, custom_error_code, pause_ix, propose_owner_ix, set_admin_ix,
    set_claim_schedule_ix, set_emission_ix, set_operator_ix, set_price_ix, set_sale_schedule_ix,
    set_tx_limits_ix, unpause_ix, TestEnv,
};
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

fn err(variant: SaleError) -> u32 {
    ERROR_CODE_OFFSET + variant as u32
}

// We return the native litesvm TransactionResult as is (as `send_ixs` does).
#[allow(clippy::result_large_err)]
fn send(env: &mut TestEnv, ix: Instruction, signer: &Keypair) -> litesvm::types::TransactionResult {
    env.send_ixs(&[ix], &signer.pubkey(), &[signer])
}

/// Three "wrong" roles for a given target role. They are chosen from the
/// environment's actors; each is an independent funded key. `exclude` is the target
/// role, which is NOT included (it is the "correct" actor).
fn wrong_actors(env: &TestEnv, exclude: &str) -> Vec<(&'static str, Keypair)> {
    let all: [(&'static str, Keypair); 4] = [
        ("owner", env.actors.owner.insecure_clone()),
        ("admin", env.actors.admin.insecure_clone()),
        ("operator", env.actors.operator.insecure_clone()),
        ("user", env.actors.users[0].insecure_clone()),
    ];
    all.into_iter().filter(|(l, _)| *l != exclude).collect()
}

/// Every "wrong" actor calling `build_ix(actor_pubkey)` must fail with Unauthorized.
/// `build_ix` puts the given pubkey both into the signer account's metadata
/// (admin/owner) and into the signatures - so the failure happens on the role guard
/// (has_one) rather than on a missing signature.
fn assert_wrong_actors_unauthorized<F>(env: &mut TestEnv, wrong: &[(&str, Keypair)], build_ix: F)
where
    F: Fn(Pubkey) -> Instruction,
{
    for (label, actor) in wrong {
        env.expire_blockhash();
        let ix = build_ix(actor.pubkey());
        let r = send(env, ix, actor);
        assert_eq!(
            custom_error_code(&r),
            Some(err(SaleError::Unauthorized)),
            "{label} invoked a privileged instruction → Unauthorized was expected: {r:?}"
        );
    }
}

// ===================================================================
// Admin-only instructions: the correct admin passes, owner/operator/user → 6002.
// The negatives run in PreSale (where the instruction would pass for admin).
// ===================================================================

#[test]
fn admin_setters_reject_wrong_signers() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let p = env.init_default_sale();
    // We stay in PreSale.

    let wrong = wrong_actors(&env, "admin"); // owner / operator / user

    // set_price
    assert_wrong_actors_unauthorized(&mut env, &wrong, |a| set_price_ix(a, 40_000));
    // set_emission
    assert_wrong_actors_unauthorized(&mut env, &wrong, |a| set_emission_ix(a, 1_000));
    // set_tx_limits
    assert_wrong_actors_unauthorized(&mut env, &wrong, |a| set_tx_limits_ix(a, 1, 2));
    // set_sale_schedule
    let sd = p.sale_start_date + 5;
    let ed = p.sale_end_date + 5;
    assert_wrong_actors_unauthorized(&mut env, &wrong, |a| set_sale_schedule_ix(a, sd, ed));
    // set_claim_schedule
    let cs = p.sale_end_date + 2_000;
    let cd = 100 * 24 * 60 * 60;
    assert_wrong_actors_unauthorized(&mut env, &wrong, |a| set_claim_schedule_ix(a, cs, cd));

    // POSITIVE: the correct admin passes in PreSale (set_price writes).
    env.expire_blockhash();
    send(&mut env, set_price_ix(admin.pubkey(), 41_000), &admin)
        .expect("the correct admin: set_price passes in PreSale");
    assert_eq!(
        env.sale_state().price_usdc,
        41_000,
        "admin really did change the price"
    );
}

#[test]
fn pause_unpause_reject_wrong_signers() {
    // The role guard (has_one) fires during resolution EARLIER than the phase one - we
    // check in Sale, where both instructions would pass for admin, so the failure is
    // purely about the role.
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let p = env.init_default_sale();
    env.warp_to(p.sale_start_date); // Sale

    let wrong = wrong_actors(&env, "admin"); // owner / operator / user

    assert_wrong_actors_unauthorized(&mut env, &wrong, pause_ix);
    assert_wrong_actors_unauthorized(&mut env, &wrong, unpause_ix);

    // POSITIVE: the correct admin sets and clears the pause in Sale.
    env.expire_blockhash();
    send(&mut env, pause_ix(admin.pubkey()), &admin).expect("admin: pause in Sale");
    assert!(env.sale_state().paused);
    env.expire_blockhash();
    send(&mut env, unpause_ix(admin.pubkey()), &admin).expect("admin: unpause in Sale");
    assert!(!env.sale_state().paused);
}

// ===================================================================
// Owner-only instructions: the correct owner passes, admin/operator/user → 6002.
// Any phase (the role instructions are not phase-gated).
// ===================================================================

#[test]
fn owner_role_setters_reject_wrong_signers() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    env.init_default_sale();

    let wrong = wrong_actors(&env, "owner"); // admin / operator / user

    let new_key = Pubkey::new_unique();
    // propose_owner (accept_owner is gated by pending rather than by role - those tests
    // live in roles.rs)
    assert_wrong_actors_unauthorized(&mut env, &wrong, |o| propose_owner_ix(o, new_key));
    // cancel_owner_proposal: the role guard fires before the pending check, so no
    // nominated candidate is needed here.
    assert_wrong_actors_unauthorized(&mut env, &wrong, cancel_owner_proposal_ix);
    // set_admin
    assert_wrong_actors_unauthorized(&mut env, &wrong, |o| set_admin_ix(o, new_key));
    // set_operator
    assert_wrong_actors_unauthorized(&mut env, &wrong, |o| set_operator_ix(o, new_key));

    // POSITIVE: the correct owner changes the operator (minimally invasive - owner and
    // admin are left alone so as not to break has_one for any further assertions in this
    // env).
    env.expire_blockhash();
    let fresh_operator = Pubkey::new_unique();
    send(
        &mut env,
        set_operator_ix(owner.pubkey(), fresh_operator),
        &owner,
    )
    .expect("owner: set_operator passes");
    assert_eq!(
        env.sale_state().operator,
        fresh_operator,
        "owner really did change the operator"
    );
}
