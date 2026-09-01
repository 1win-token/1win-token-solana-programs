//! The token vault's funding: the `assert_funded` check - Arch §9.9a; TRD §4. The
//! instruction changes no state, emits no events and does not check the signer; its
//! only observable effect is whether the transaction passed or failed.
//!
//! Why the tests are laid out the way they are.
//!
//! 1. The instruction is meant to be a link in the atomic minting transaction:
//!    `mint_to` into `token_vault` → `assert_funded` → `SetAuthority(None)`. So the
//!    "passes / does not pass" boundary is checked to the precision of a single token
//!    unit, along with the complete absence of side effects.
//! 2. Under-funding is produced honestly - by minting a smaller amount: that is
//!    exactly how an under-mint arises in production.
//! 3. The reference value is taken from the stored `emission` field rather than from
//!    a constant: a separate pair of cases edits the emission and checks that the
//!    boundary moved with it. Otherwise a number hard-coded into the test would pass
//!    every other case in the file.
//! 4. "Permissionless" is verified positively and across several keys: there is no
//!    `Signer` slot in the context at all, so the only signer of the transaction is
//!    its fee payer, and that same fee payer proves no role is asked for. The
//!    assertion "the key is neither Owner, nor Admin, nor Operator" stands
//!    explicitly, or the test could quietly degenerate.
//! 5. State immutability is checked against the singleton's raw data (every field at
//!    once), its lamports and the vault's balance - both on success and on rejection.
//! 6. The vault's binding to `SaleState` is checked by substituting a foreign token
//!    account of the same mint under the same `vault_authority`: "the same mint and
//!    the same owner" gives no uniqueness, and the address must come from the state.
//! 7. The regular usage is checked whole, in a single transaction: the bundle
//!    `mint_to` → `assert_funded` → `SetAuthority(None)` passes and revokes the mint
//!    authority, while on an under-mint it fails the transaction - the mint authority
//!    stays in place and the vault is empty.

use anchor_lang::error::{ErrorCode as AnchorError, ERROR_CODE_OFFSET};
use sale::errors::SaleError;
use sale_test_common::{
    assert_funded_ix, assert_funded_ix_with_slots, custom_error_code, expected::DEFAULT_EMISSION,
    sale_state_pda, set_emission_ix, token_vault_pda, vault_authority_pda, InitParams, TestEnv,
};
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

/// The sale's default emission: 130,000,000 $1WIN in token units (8 decimals). It is
/// also the funding reference: the vault must hold all of it.
const EMISSION: u64 = DEFAULT_EMISSION;

fn err(variant: SaleError) -> u32 {
    ERROR_CODE_OFFSET + variant as u32
}

fn anchor_err(code: AnchorError) -> u32 {
    code as u32
}

fn custom(result: &litesvm::types::TransactionResult) -> Option<u32> {
    custom_error_code(result)
}

/// An initialized sale with an empty `token_vault`; the PreSale phase.
fn setup() -> (TestEnv, InitParams) {
    let mut env = TestEnv::new();
    let params = env.init_default_sale();
    (env, params)
}

/// Mints `amount` $1WIN into `token_vault` (in production this is the Owner MS before
/// activation).
fn fund_token_vault(env: &mut TestEnv, amount: u64) {
    let win = env.win_mint;
    env.mint_to(&win, &token_vault_pda(), amount);
}

/// Sends `assert_funded` on behalf of `signer` (which is also the fee payer: the
/// instruction has no `Signer` slot).
#[allow(clippy::result_large_err)]
fn send_assert_funded(env: &mut TestEnv, signer: &Keypair) -> litesvm::types::TransactionResult {
    env.send_ixs(&[assert_funded_ix()], &signer.pubkey(), &[signer])
}

/// A `mint_to` into `token_vault` from the environment's mint authority (`payer`).
fn mint_to_vault_ix(env: &TestEnv, amount: u64) -> Instruction {
    spl_token_interface::instruction::mint_to(
        &spl_token_interface::id(),
        &env.win_mint,
        &token_vault_pda(),
        &env.payer.pubkey(),
        &[],
        amount,
    )
    .expect("the mint_to instruction into token_vault")
}

/// `SetAuthority(MintTokens, None)` - the closing link of the funding transaction:
/// after it no further $1WIN can be issued (T-6).
fn revoke_mint_authority_ix(env: &TestEnv) -> Instruction {
    spl_token_interface::instruction::set_authority(
        &spl_token_interface::id(),
        &env.win_mint,
        None,
        spl_token_interface::instruction::AuthorityType::MintTokens,
        &env.payer.pubkey(),
        &[],
    )
    .expect("the SetAuthority(MintTokens, None) instruction")
}

/// The observable state the check must not touch: the singleton's raw data (every
/// field at once - a divergence in any field is caught), its lamports and the vault's
/// balance.
#[derive(Debug, PartialEq, Eq)]
struct Observed {
    sale_state_data: Vec<u8>,
    sale_state_lamports: u64,
    vault_balance: u64,
}

fn observe(env: &TestEnv) -> Observed {
    let account = env
        .svm
        .get_account(&sale_state_pda())
        .expect("sale_state exists");
    Observed {
        sale_state_data: account.data,
        sale_state_lamports: account.lamports,
        vault_balance: env.token_balance(&token_vault_pda()),
    }
}

// ===================================================================
// The funding boundary: exactly the emission passes, one unit less does not
// ===================================================================

#[test]
fn assert_funded_passes_at_exactly_emission() {
    let (mut env, _) = setup();
    fund_token_vault(&mut env, EMISSION);
    let payer = env.payer.insecure_clone();

    let r = send_assert_funded(&mut env, &payer);
    assert!(
        r.is_ok(),
        "the vault is funded to exactly the emission: {r:?}"
    );
    assert_eq!(
        env.token_balance(&token_vault_pda()),
        EMISSION,
        "the check does not move the vault's balance"
    );
}

#[test]
fn assert_funded_rejects_one_unit_short() {
    let (mut env, _) = setup();
    fund_token_vault(&mut env, EMISSION - 1);
    let payer = env.payer.insecure_clone();

    let r = send_assert_funded(&mut env, &payer);
    assert_eq!(
        custom(&r),
        Some(err(SaleError::VaultNotFunded)),
        "an under-mint of one token unit → VaultNotFunded: {r:?}"
    );
}

#[test]
fn assert_funded_rejects_empty_vault() {
    let (mut env, _) = setup();
    let payer = env.payer.insecure_clone();

    let r = send_assert_funded(&mut env, &payer);
    assert_eq!(
        custom(&r),
        Some(err(SaleError::VaultNotFunded)),
        "an empty vault → VaultNotFunded: {r:?}"
    );
}

#[test]
fn assert_funded_accepts_surplus() {
    let (mut env, _) = setup();
    fund_token_vault(&mut env, EMISSION + 1);
    let payer = env.payer.insecure_clone();

    let r = send_assert_funded(&mut env, &payer);
    assert!(
        r.is_ok(),
        "the coverage condition is \"not less than\", not \"exactly\": {r:?}"
    );
}

// ===================================================================
// The reference is the stored emission field, not a constant in the code
// ===================================================================

#[test]
fn assert_funded_follows_stored_emission() {
    let (mut env, _) = setup();
    let admin = env.actors.admin.insecure_clone();
    let payer = env.payer.insecure_clone();

    fund_token_vault(&mut env, EMISSION);
    let r = send_assert_funded(&mut env, &payer);
    assert!(
        r.is_ok(),
        "at the original emission the vault is funded: {r:?}"
    );

    // Admin raises the emission (in the PreSale phase) - the obligation grew.
    let raised = EMISSION + 1_000;
    env.expire_blockhash();
    env.send_ixs(
        &[set_emission_ix(admin.pubkey(), raised)],
        &admin.pubkey(),
        &[&admin],
    )
    .expect("set_emission in PreSale");
    assert_eq!(env.sale_state().emission, raised, "the emission was raised");

    env.expire_blockhash();
    let r = send_assert_funded(&mut env, &payer);
    assert_eq!(
        custom(&r),
        Some(err(SaleError::VaultNotFunded)),
        "the same balance no longer covers the raised emission: {r:?}"
    );

    // Minting up to the new emission closes the gap.
    fund_token_vault(&mut env, 1_000);
    env.expire_blockhash();
    let r = send_assert_funded(&mut env, &payer);
    assert!(
        r.is_ok(),
        "after minting up the vault is funded again: {r:?}"
    );
}

// ===================================================================
// Roles: the signer is not checked
// ===================================================================

#[test]
fn assert_funded_is_permissionless() {
    let (mut env, _) = setup();
    fund_token_vault(&mut env, EMISSION);

    let stranger = env.new_user();
    let (owner, admin, operator) = (
        env.actors.owner.pubkey(),
        env.actors.admin.pubkey(),
        env.actors.operator.pubkey(),
    );
    assert!(
        stranger.pubkey() != owner && stranger.pubkey() != admin && stranger.pubkey() != operator,
        "the foreign key must not coincide with any role - otherwise the case is degenerate"
    );

    let callers = [
        (env.actors.owner.insecure_clone(), "owner"),
        (env.actors.admin.insecure_clone(), "admin"),
        (env.actors.operator.insecure_clone(), "operator"),
        (stranger, "an outsider"),
    ];
    for (caller, label) in callers {
        env.expire_blockhash();
        let r = send_assert_funded(&mut env, &caller);
        assert!(
            r.is_ok(),
            "{label}: assert_funded is permissionless - the signer's role is not asked for: {r:?}"
        );
    }
}

// ===================================================================
// No effects - on success and on rejection alike
// ===================================================================

#[test]
fn assert_funded_changes_nothing() {
    let (mut env, _) = setup();
    fund_token_vault(&mut env, EMISSION);
    let payer = env.payer.insecure_clone();

    let before = observe(&env);
    send_assert_funded(&mut env, &payer).expect("a funded vault passes the check");
    assert_eq!(observe(&env), before, "a successful check changes no state");

    // The same assertion on a rejection: we understate the funding by raising the
    // obligation.
    let admin = env.actors.admin.insecure_clone();
    env.expire_blockhash();
    env.send_ixs(
        &[set_emission_ix(admin.pubkey(), EMISSION + 1)],
        &admin.pubkey(),
        &[&admin],
    )
    .expect("set_emission in PreSale");

    let before = observe(&env);
    env.expire_blockhash();
    let r = send_assert_funded(&mut env, &payer);
    assert_eq!(
        custom(&r),
        Some(err(SaleError::VaultNotFunded)),
        "the under-mint was rejected: {r:?}"
    );
    assert_eq!(observe(&env), before, "the rejection changes no state");
}

// ===================================================================
// Slot binding: the vault is only the one recorded in SaleState
// ===================================================================

#[test]
fn assert_funded_binds_vault_to_sale_state() {
    let (mut env, _) = setup();
    let win = env.win_mint;
    let payer = env.payer.insecure_clone();

    // A foreign token account under the same mint and the same vault owner: "the same
    // mint and the same authority" gives no uniqueness.
    let side_vault = env.create_ata(&vault_authority_pda(), &win);
    env.mint_to(&win, &side_vault, EMISSION);
    assert_eq!(
        env.token_balance(&token_vault_pda()),
        0,
        "the canonical vault is empty - otherwise the substitution would have passed on the merits too"
    );

    env.expire_blockhash();
    let r = env.send_ixs(
        &[assert_funded_ix_with_slots(sale_state_pda(), side_vault)],
        &payer.pubkey(),
        &[&payer],
    );
    assert_eq!(
        custom(&r),
        Some(anchor_err(AnchorError::ConstraintAddress)),
        "substituting token_vault → ConstraintAddress (2012): {r:?}"
    );

    // A foreign `sale_state`: the emission is read only from the singleton at its seeds.
    env.expire_blockhash();
    let r = env.send_ixs(
        &[assert_funded_ix_with_slots(
            Pubkey::new_unique(),
            token_vault_pda(),
        )],
        &payer.pubkey(),
        &[&payer],
    );
    assert_eq!(
        custom(&r),
        Some(anchor_err(AnchorError::AccountNotInitialized)),
        "a decoy sale_state → AccountNotInitialized (3012): {r:?}"
    );

    // The positive path of the same config: the canonical vault, honestly funded, passes
    // the check.
    fund_token_vault(&mut env, EMISSION);
    env.expire_blockhash();
    let r = send_assert_funded(&mut env, &payer);
    assert!(
        r.is_ok(),
        "the canonical vault holding the emission passes the check: {r:?}"
    );
}

// ===================================================================
// Regular usage: the gate before the mint authority is revoked
// ===================================================================

#[test]
fn assert_funded_gates_mint_authority_revocation() {
    let (mut env, _) = setup();
    let payer = env.payer.insecure_clone();
    let win = env.win_mint;
    assert_eq!(
        env.mint_authority(&win),
        Some(payer.pubkey()),
        "before the funding transaction the mint authority is in place"
    );

    let ixs = [
        mint_to_vault_ix(&env, EMISSION),
        assert_funded_ix(),
        revoke_mint_authority_ix(&env),
    ];
    let r = env.send_ixs(&ixs, &payer.pubkey(), &[&payer]);
    assert!(
        r.is_ok(),
        "mint → check → revoke the mint authority in one transaction: {r:?}"
    );
    assert_eq!(
        env.token_balance(&token_vault_pda()),
        EMISSION,
        "the sale's emission is in the vault"
    );
    assert_eq!(
        env.mint_authority(&win),
        None,
        "the mint authority was revoked - no further issuance is possible"
    );
}

#[test]
fn underfunded_vault_reverts_the_whole_revocation_tx() {
    let (mut env, _) = setup();
    let payer = env.payer.insecure_clone();
    let win = env.win_mint;

    let ixs = [
        mint_to_vault_ix(&env, EMISSION - 1),
        assert_funded_ix(),
        revoke_mint_authority_ix(&env),
    ];
    let r = env.send_ixs(&ixs, &payer.pubkey(), &[&payer]);
    assert_eq!(
        custom(&r),
        Some(err(SaleError::VaultNotFunded)),
        "the under-mint fails the transaction at the check: {r:?}"
    );
    assert_eq!(
        env.token_balance(&token_vault_pda()),
        0,
        "a full rollback: what was minted did not stay in the vault"
    );
    assert_eq!(
        env.mint_authority(&win),
        Some(payer.pubkey()),
        "the mint authority was not revoked - the under-mint can still be fixed"
    );
}

// ===================================================================
// Phase: the check is available at any point in the program's life
// ===================================================================

#[test]
fn assert_funded_is_not_gated_by_phase() {
    let (mut env, params) = setup();
    fund_token_vault(&mut env, EMISSION);
    let payer = env.payer.insecure_clone();

    let deadline = params.claim_start_date + params.claim_duration;
    let stops = [
        (params.sale_start_date - 1, "PreSale"),
        (params.sale_start_date, "Sale"),
        (params.sale_end_date, "SaleEnded"),
        (deadline, "ClaimClosed by time"),
    ];
    for (ts, label) in stops {
        env.warp_to(ts);
        env.expire_blockhash();
        let r = send_assert_funded(&mut env, &payer);
        assert!(
            r.is_ok(),
            "{label}: the funding check is not phase-gated: {r:?}"
        );
    }
}
