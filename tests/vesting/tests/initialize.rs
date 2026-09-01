//! `initialize` (Arch §9.1) - the positive case plus the negative matrix.
//!
//! Why the tests are laid out the way they are.
//!
//! 1. `initialize`'s authorization rests not on the config (there is none yet) but on
//!    the program's ProgramData. So the authorization negatives are built by
//!    substituting the ENVIRONMENT (`with_upgrade_authority` - the upgrade right
//!    belongs to a foreign key) and by substituting the `program_data` SLOT
//!    (`forge_foreign_program_data` - the ProgramData of a foreign program), rather
//!    than by "the wrong signer" as in every other instruction.
//! 2. The order of the checks in the handler's body is observable: the time gate
//!    (`InvalidTge`) → the mint's decimals (`InvalidMintDecimals`) → the roles
//!    (`InvalidAuthority`). So a role negative must supply a valid `tge_ts` and mint,
//!    or it would fail on an earlier check and verify the wrong invariant. Every
//!    negative deviates EXACTLY one field of the valid set
//!    [`TestEnv::valid_init_params`].
//! 3. The context's types and constraints (`Program`, `Account<ProgramData>`,
//!    `Account<Mint>`) fire during account resolution, BEFORE the body. So a
//!    Token-2022 mint is rejected by an Anchor code rather than a program code, and
//!    the test pins exactly that anchor code: swap the type for `UncheckedAccount`
//!    and the test turns red.
//! 4. Every negative carries a "the singleton was not created" assertion plus a
//!    positive in THE SAME environment: it proves the rejection was caused by
//!    the edited field rather than by a broken test environment. The exception is the
//!    double `initialize`: there the positive was already executed by the first call,
//!    and what is asserted is that its result stayed put.
//! 5. Substituting the derived slots (`config`, `vault_authority`) is checked here
//!    rather than only on a live config: it is `initialize` that caches
//!    `vault_authority_bump` - the one the program signs every subsequent vault
//!    transfer and close with - and the singleton's own address, which every other
//!    instruction looks it up by.
//!
//! ===== DELIBERATELY DEFERRED =====
//! - `MathOverflow` at the `now + MAX_TGE_DELAY` ceiling: it requires `now` within
//!   365 days of `i64::MAX`. `warp_to` cannot get there (the slot is computed as
//!   `delta × SLOTS_PER_SECOND` and would overflow `u64`), so a direct
//!   `set_sysvar(Clock)` is needed - that is a forge of time rather than of an
//!   argument, and it belongs to the substitution matrix.
//! - Proof that `vault_authority_bump` really does sign the CPI: it is observable
//!   only where there is a CPI - a transfer out of an activity vault (V2-V3). Here
//!   only the cache's equality to the canonical bump is asserted.
//! - The role and TGE matrices in full; the `initialize` CU
//! benchmark.

use anchor_lang::error::ErrorCode;
use litesvm::types::TransactionResult;
use solana_instruction_error::InstructionError;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction_error::TransactionError;
use vesting::errors::VestingError;
use vesting::events::Initialized;
use vesting::state::MAX_TGE_DELAY;
use vesting_test_common::vesting_expected::vesting::{ECOSYSTEM_FULL_RELEASE_DAY, V2_REFERENCE};
use vesting_test_common::{
    cancel_vesting_ix, close_vesting_ix, config_pda, custom_error_code, decode_event, err,
    initialize_ix, initialize_ix_with_slots, vault_authority_pda, vault_pda, vesting_pda,
    ActivitySpec, InitParams, TestEnv, ACTOR_LAMPORTS, DEFAULT_TGE_OFFSET, WIN_DECIMALS,
};

/// The reference amount of the activities used to verify that the single withdrawal
/// recipient W works: the V2 bucket of §10.5 (100,000,000 $1WIN in token units).
const REFERENCE: u64 = V2_REFERENCE;

/// Sends `initialize` with the canonical slots: `authority` signs and pays the
/// config's rent.
/// We return the native litesvm TransactionResult as is - just like `send_ixs`.
#[allow(clippy::result_large_err)]
fn send_initialize(env: &mut TestEnv, authority: &Keypair, p: &InitParams) -> TransactionResult {
    let ix = initialize_ix(authority.pubkey(), p);
    env.send_ixs(&[ix], &authority.pubkey(), &[authority])
}

/// The canonical bump of a PDA, derived independently of the program - by
/// `find_program_address` itself.
fn canonical_bump(seeds: &[&[u8]]) -> u8 {
    Pubkey::find_program_address(seeds, &vesting::id()).1
}

/// A positive case "in the same environment": the valid set passes right where
/// the rejection just happened, so the rejection was caused by the edited field
/// rather than by the environment.
fn assert_valid_initialize_succeeds(env: &mut TestEnv, authority: &Keypair) {
    env.expire_blockhash();
    let p = env.valid_init_params(DEFAULT_TGE_OFFSET);
    send_initialize(env, authority, &p).expect("the valid set passes in the same environment");
    assert!(
        env.vesting_config_exists(),
        "the positive in the same environment must create the singleton"
    );
}

// ===================================================================
// POSITIVE
// ===================================================================

#[test]
fn initialize_stores_all_fields_and_emits_event() {
    let mut env = TestEnv::new();
    let authority = env.actors.owner.insecure_clone();
    // The roles are fresh keys distinct from the signer: the signer holds the upgrade
    // authority (that is its only function here) while owner, admin and operator arrive
    // as arguments. Were they to coincide with the signer, substituting an argument with
    // `authority.key()` in the handler's body would slip past every assertion.
    let mut p = env.valid_init_params(DEFAULT_TGE_OFFSET);
    p.owner = Pubkey::new_unique();
    p.admin = Pubkey::new_unique();
    p.operator = Pubkey::new_unique();
    assert_ne!(p.owner, authority.pubkey(), "the owner role ≠ the signer");
    assert_ne!(p.admin, authority.pubkey(), "the admin role ≠ the signer");
    assert_ne!(
        p.operator,
        authority.pubkey(),
        "the operator role ≠ the signer"
    );

    let r = send_initialize(&mut env, &authority, &p);
    r.as_ref().expect("a valid initialize passes");

    let c = env.vesting_config();
    assert_eq!(c.owner, p.owner, "owner was written from the argument");
    assert_eq!(c.admin, p.admin, "admin was written from the argument");
    assert_eq!(
        c.operator, p.operator,
        "operator was written from the argument"
    );
    assert_eq!(
        c.token_mint, p.token_mint,
        "token_mint was pinned to the address of the mint that was passed"
    );
    assert_eq!(
        c.withdrawal_authority, p.withdrawal_authority,
        "withdrawal_authority was written from the argument - the single withdrawal recipient"
    );
    assert_eq!(c.tge_ts, p.tge_ts, "tge_ts was written from the argument");
    assert_eq!(
        c.activity_count, 0,
        "the activity counter starts at zero - the next activity_id"
    );
    assert_eq!(
        c.vault_authority_bump,
        canonical_bump(&[b"vault_authority"]),
        "the canonical bump of vault_authority is cached: the vault CPIs of V2-V3 are signed with it, and an empty value would break them"
    );

    let ev: Initialized = decode_event(&r).expect("initialize emits Initialized");
    assert_eq!(ev.owner, p.owner, "the event: owner");
    assert_eq!(ev.admin, p.admin, "the event: admin");
    assert_eq!(ev.operator, p.operator, "the event: operator");
    assert_eq!(ev.token_mint, p.token_mint, "the event: token_mint");
    assert_eq!(
        ev.withdrawal_authority, p.withdrawal_authority,
        "the event: withdrawal_authority"
    );
    assert_eq!(ev.tge_ts, p.tge_ts, "the event: tge_ts");
}

// ===================================================================
// NEGATIVE: hijacking the singleton
// ===================================================================

#[test]
fn initialize_rejects_signer_without_upgrade_authority() {
    // The upgrade right belongs to a separate key; the owner actor (which is also the
    // future config's owner) does not hold it and must be rejected.
    let real_authority = Keypair::new();
    let mut env = TestEnv::with_upgrade_authority(real_authority.pubkey());
    env.svm
        .airdrop(&real_authority.pubkey(), ACTOR_LAMPORTS)
        .expect("an airdrop to the real upgrade authority");
    let impostor = env.actors.owner.insecure_clone();
    let p = env.valid_init_params(DEFAULT_TGE_OFFSET);

    let r = send_initialize(&mut env, &impostor, &p);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::NotUpgradeAuthority)),
        "a signer without the upgrade authority → NotUpgradeAuthority: {r:?}"
    );
    assert!(
        !env.vesting_config_exists(),
        "the rejection leaves no singleton behind - otherwise the hijack would have partly succeeded (the account being occupied)"
    );

    // POSITIVE: the real upgrade authority passes in the same environment.
    env.expire_blockhash();
    let p = env.valid_init_params(DEFAULT_TGE_OFFSET);
    send_initialize(&mut env, &real_authority, &p)
        .expect("the holder of the upgrade authority: initialize passes");
    assert_eq!(
        env.vesting_config().owner,
        p.owner,
        "the config was created by the real upgrade authority"
    );
}

#[test]
fn initialize_rejects_program_data_of_another_program() {
    // [load-bearing] The signer DOES hold the upgrade authority of the ProgramData
    // that was passed, but that ProgramData belongs to a different program. Without the
    // `vesting_program.programdata_address()? == Some(program_data.key())` constraint the
    // `program_data.upgrade_authority_address == Some(authority)` check passes and the
    // singleton goes to an outsider - the positive path still works, and no other
    // hijacking test would notice.
    //
    // The mutation check was performed: with the constraint removed this test turns red
    // (`initialize` passes and the config is created by an outsider); the constraint was
    // put back.
    let mut env = TestEnv::new();
    let impostor = env.actors.stranger.insecure_clone();
    let foreign_program_data = env.forge_foreign_program_data(impostor.pubkey());
    let p = env.valid_init_params(DEFAULT_TGE_OFFSET);

    let ix = initialize_ix_with_slots(
        impostor.pubkey(),
        foreign_program_data,
        config_pda(),
        p.withdrawal_authority,
        vault_authority_pda(),
        &p,
    );
    let r = env.send_ixs(&[ix], &impostor.pubkey(), &[&impostor]);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::NotUpgradeAuthority)),
        "a foreign program's ProgramData → NotUpgradeAuthority: {r:?}"
    );
    assert!(
        !env.vesting_config_exists(),
        "hijacking the singleton through a foreign ProgramData created no config"
    );

    // POSITIVE: this program's canonical ProgramData plus its upgrade authority.
    let authority = env.actors.owner.insecure_clone();
    assert_valid_initialize_succeeds(&mut env, &authority);
}

// ===================================================================
// NEGATIVE: substituting the derived slots
// ===================================================================

#[test]
fn initialize_rejects_non_canonical_config_slot() {
    let mut env = TestEnv::new();
    let authority = env.actors.owner.insecure_clone();
    let p = env.valid_init_params(DEFAULT_TGE_OFFSET);

    // The singleton's address is derived by Anchor from the seeds `[b"config"]`. Were
    // `initialize` to accept an arbitrary address, there could be any number of configs,
    // while the other nine instructions look up exactly the canonical one and would find
    // none of them.
    let substituted = Pubkey::new_unique();
    let ix = initialize_ix_with_slots(
        authority.pubkey(),
        TestEnv::programdata_address(),
        substituted,
        p.withdrawal_authority,
        vault_authority_pda(),
        &p,
    );
    let r = env.send_ixs(&[ix], &authority.pubkey(), &[&authority]);
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintSeeds)),
        "a non-canonical address in the config slot → ConstraintSeeds: {r:?}"
    );
    assert!(
        !env.account_exists(&substituted),
        "the rejection created no account at the decoy address"
    );
    assert!(
        !env.vesting_config_exists(),
        "the singleton was not created"
    );

    assert_valid_initialize_succeeds(&mut env, &authority);
}

#[test]
fn initialize_rejects_non_canonical_vault_authority_slot() {
    let mut env = TestEnv::new();
    let authority = env.actors.owner.insecure_clone();
    let p = env.valid_init_params(DEFAULT_TGE_OFFSET);

    // The `vault_authority` slot is bound by the seeds `[b"vault_authority"]`, and it is
    // its bump that goes into the config. Were the substitution to pass, a foreign
    // address's bump would land in `vault_authority_bump` and the signature for the
    // canonical PDA would fail in every vault transfer and every close: the funds of
    // every activity would become unreachable. On top of that the "withdrawal_authority
    // != vault_authority" gate would be disabled - it compares against the slot.
    let ix = initialize_ix_with_slots(
        authority.pubkey(),
        TestEnv::programdata_address(),
        config_pda(),
        p.withdrawal_authority,
        Pubkey::new_unique(),
        &p,
    );
    let r = env.send_ixs(&[ix], &authority.pubkey(), &[&authority]);
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintSeeds)),
        "a non-canonical address in the vault_authority slot → ConstraintSeeds: {r:?}"
    );
    assert!(
        !env.vesting_config_exists(),
        "the singleton was not created"
    );

    assert_valid_initialize_succeeds(&mut env, &authority);
    assert_eq!(
        env.vesting_config().vault_authority_bump,
        canonical_bump(&[b"vault_authority"]),
        "the bump of the CANONICAL vault_authority is cached"
    );
}

// ===================================================================
// NEGATIVE: a repeated initialize
// ===================================================================

#[test]
fn second_initialize_reverts_and_keeps_first_config() {
    let mut env = TestEnv::new();
    let authority = env.actors.owner.insecure_clone();
    let first = env.init_default(DEFAULT_TGE_OFFSET);
    let before = env.vesting_config();

    // A second call with DIFFERENT values: an attempt at re-initialization rather than a
    // repeat of the same transaction.
    env.expire_blockhash();
    let mut p2 = first;
    p2.admin = Pubkey::new_unique();
    p2.operator = Pubkey::new_unique();
    p2.tge_ts = first.tge_ts + 1;
    let r = send_initialize(&mut env, &authority, &p2);

    // A FACT observed by execution (LiteSVM + Anchor 1.0.2): `#[account(init)]` is
    // translated into a CPI to the System Program, which on an already existing account
    // answers "account already in use" = Custom(0) FROM the System Program. So this is
    // not a 6xxx Vesting code: Anchor's structural init guard fired BEFORE the handler's
    // body and not a single program check ran.
    let failed = r.as_ref().expect_err("a repeated initialize must revert");
    assert!(
        matches!(
            failed.err,
            TransactionError::InstructionError(_, InstructionError::Custom(0))
        ),
        "a repeated init → System `account already in use` = Custom(0): {:?}",
        failed.err
    );

    let after = env.vesting_config();
    assert_eq!(
        after.owner, before.owner,
        "the owner of the first init is intact"
    );
    assert_eq!(
        after.admin, before.admin,
        "admin was not overwritten by the second init"
    );
    assert_eq!(
        after.operator, before.operator,
        "operator was not overwritten by the second init"
    );
    assert_eq!(
        after.tge_ts, before.tge_ts,
        "tge_ts was not overwritten by the second init"
    );
    assert_eq!(
        after.activity_count, before.activity_count,
        "the activity counter was not zeroed by the repeated init"
    );
}

// ===================================================================
// the roles: a zero key is rejected, coinciding roles are allowed
// ===================================================================

#[test]
fn initialize_rejects_default_owner() {
    let mut env = TestEnv::new();
    let authority = env.actors.owner.insecure_clone();
    let mut p = env.valid_init_params(DEFAULT_TGE_OFFSET);
    p.owner = Pubkey::default();

    let r = send_initialize(&mut env, &authority, &p);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InvalidAuthority)),
        "a zero owner → InvalidAuthority (nobody could manage the config): {r:?}"
    );
    assert!(
        !env.vesting_config_exists(),
        "the singleton was not created"
    );

    assert_valid_initialize_succeeds(&mut env, &authority);
}

#[test]
fn initialize_rejects_default_admin() {
    let mut env = TestEnv::new();
    let authority = env.actors.owner.insecure_clone();
    let mut p = env.valid_init_params(DEFAULT_TGE_OFFSET);
    p.admin = Pubkey::default();

    let r = send_initialize(&mut env, &authority, &p);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InvalidAuthority)),
        "a zero admin → InvalidAuthority (nobody could change the configuration): {r:?}"
    );
    assert!(
        !env.vesting_config_exists(),
        "the singleton was not created"
    );

    assert_valid_initialize_succeeds(&mut env, &authority);
}

#[test]
fn initialize_rejects_default_operator() {
    let mut env = TestEnv::new();
    let authority = env.actors.owner.insecure_clone();
    let mut p = env.valid_init_params(DEFAULT_TGE_OFFSET);
    p.operator = Pubkey::default();

    let r = send_initialize(&mut env, &authority, &p);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InvalidAuthority)),
        "a zero operator → InvalidAuthority: {r:?}"
    );
    assert!(
        !env.vesting_config_exists(),
        "the singleton was not created"
    );

    assert_valid_initialize_succeeds(&mut env, &authority);
}

/// One key for all three roles is accepted: there is no constraint on roles
/// coinciding, only the address being non-zero is checked.
#[test]
fn initialize_accepts_one_key_for_all_three_roles() {
    let mut env = TestEnv::new();
    let authority = env.actors.owner.insecure_clone();
    let shared = Pubkey::new_unique();
    let mut p = env.valid_init_params(DEFAULT_TGE_OFFSET);
    p.owner = shared;
    p.admin = shared;
    p.operator = shared;

    let r = send_initialize(&mut env, &authority, &p);
    r.as_ref().expect("identical roles do not stop initialize");

    let c = env.vesting_config();
    assert_eq!(c.owner, shared, "owner was written from the argument");
    assert_eq!(c.admin, shared, "admin was written from the argument");
    assert_eq!(c.operator, shared, "operator was written from the argument");
}

// ===================================================================
// NEGATIVE: the mint
// ===================================================================

#[test]
fn initialize_rejects_mint_with_wrong_decimals() {
    let mut env = TestEnv::new();
    let authority = env.actors.owner.insecure_clone();
    let mut p = env.valid_init_params(DEFAULT_TGE_OFFSET);
    // A mint with 9 decimals - the typical "like SOL" mistake instead of $1WIN's 8.
    p.token_mint = env.create_mint_for_test(9);

    let r = send_initialize(&mut env, &authority, &p);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InvalidMintDecimals)),
        "decimals != 8 → InvalidMintDecimals: {r:?}"
    );
    assert!(
        !env.vesting_config_exists(),
        "the singleton was not created"
    );

    assert_valid_initialize_succeeds(&mut env, &authority);
}

#[test]
fn initialize_rejects_token_2022_mint_by_account_owner_check() {
    // A positive test of the fact that Token-2022 is rejected not by a check in
    // the body but by the `anchor_spl::token::Mint` TYPE - which requires the
    // `spl_token` owner. The mint is created with the CORRECT 8 decimals, so the body's
    // check would have passed: the code in the assertion proves the rejection is
    // structural. Changing the slot's type to `UncheckedAccount` turns this test red.
    let mut env = TestEnv::new();
    let authority = env.actors.owner.insecure_clone();
    let mut p = env.valid_init_params(DEFAULT_TGE_OFFSET);
    p.token_mint = env.create_token2022_mint(WIN_DECIMALS);

    let expected = u32::from(ErrorCode::AccountOwnedByWrongProgram);
    assert_eq!(
        expected, 3007,
        "the nominal anchor code AccountOwnedByWrongProgram (Anchor 1.0.2)"
    );

    let r = send_initialize(&mut env, &authority, &p);
    assert_eq!(
        custom_error_code(&r),
        Some(expected),
        "a mint under Token-2022 → the anchor AccountOwnedByWrongProgram, not a 6xxx program code: {r:?}"
    );
    assert!(
        !env.vesting_config_exists(),
        "the singleton was not created"
    );

    assert_valid_initialize_succeeds(&mut env, &authority);
}

// ===================================================================
// NEGATIVE and BOUNDARY: tge_ts
// ===================================================================

#[test]
fn initialize_rejects_tge_at_now() {
    let mut env = TestEnv::new();
    let authority = env.actors.owner.insecure_clone();
    let mut p = env.valid_init_params(DEFAULT_TGE_OFFSET);
    // Exactly `now` - the upper edge of the invalid range (the condition is
    // `tge_ts > now`).
    p.tge_ts = env.now();

    let r = send_initialize(&mut env, &authority, &p);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InvalidTge)),
        "tge_ts == now → InvalidTge (TGE must be strictly in the future): {r:?}"
    );
    assert!(
        !env.vesting_config_exists(),
        "the singleton was not created"
    );

    assert_valid_initialize_succeeds(&mut env, &authority);
}

#[test]
fn initialize_rejects_tge_beyond_max_delay() {
    let mut env = TestEnv::new();
    let authority = env.actors.owner.insecure_clone();
    let mut p = env.valid_init_params(DEFAULT_TGE_OFFSET);
    // One second past the ceiling - the lower edge of the invalid range from above.
    p.tge_ts = env.now() + MAX_TGE_DELAY + 1;

    let r = send_initialize(&mut env, &authority, &p);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InvalidTge)),
        "tge_ts == now + MAX_TGE_DELAY + 1 → InvalidTge: {r:?}"
    );
    assert!(
        !env.vesting_config_exists(),
        "the singleton was not created"
    );

    assert_valid_initialize_succeeds(&mut env, &authority);
}

#[test]
fn initialize_accepts_tge_exactly_at_max_delay() {
    // The ceiling is inclusive: `tge_ts <= now + MAX_TGE_DELAY`. The counterpart to the
    // test above - together they pin the boundary to the second.
    assert_eq!(
        MAX_TGE_DELAY,
        365 * 86_400,
        "MAX_TGE_DELAY is fixed by agreement (365 days); changing it moves both boundaries of this test"
    );

    let mut env = TestEnv::new();
    let authority = env.actors.owner.insecure_clone();
    let p = env.valid_init_params(MAX_TGE_DELAY);
    assert_eq!(
        p.tge_ts,
        env.now() + MAX_TGE_DELAY,
        "the argument sits exactly on the ceiling"
    );

    send_initialize(&mut env, &authority, &p).expect("a tge_ts exactly on the ceiling is accepted");
    assert_eq!(
        env.vesting_config().tge_ts,
        p.tge_ts,
        "the boundary value was written into the config unaltered"
    );
}

// ===================================================================
// D.5 [#3] - NEGATIVE: withdrawal_authority
//
// The single withdrawal recipient W is fixed at `initialize` (fix #3) by a
// `SystemAccount` slot: the type is itself the check, while `!= vault_authority` is
// held by the body's gate (→ InvalidAuthority). Without them `CloseActivity` could
// not structurally verify the addressees of the remainder and the rent. Both
// negatives edit exactly one slot of the valid set, as the role negatives do,
// and the positive in the same environment pins that W was written into the config.
//
// The first negative covers the "the slot holds a non-system-owned account" class
// through two substitutions (following the pattern) and proves the recorded W
// works by executing both activity-closing instructions on it: with a program-owned
// W the `rent_collector` slot in `CloseActivity` would not resolve at all, and the
// activity's remainder and rent would stay locked.
// ===================================================================

#[test]
fn initialize_rejects_non_system_withdrawal_authority() {
    let mut env = TestEnv::new();
    let authority = env.actors.owner.insecure_clone();

    let expected = u32::from(ErrorCode::AccountNotSystemOwned);
    assert_eq!(
        expected, 3011,
        "the nominal anchor code AccountNotSystemOwned (Anchor 1.0.2)"
    );

    let win_mint = env.win_mint;
    let beneficiary = env.actors.beneficiary.pubkey();
    let token_account = env.create_ata(&beneficiary, &win_mint);

    let substitutions: [(&str, Pubkey); 2] = [
        // Pubkey::default() is the System Program's own address; its account belongs to the
        // loader rather than to the System Program, so the slot's type rejects it before the
        // body (the same class as the `recipient` slot).
        (
            "Pubkey::default() (the System Program's address)",
            Pubkey::default(),
        ),
        // A program-owned account - the other half of the class: a W with a program owner
        // would make cancelling and closing an activity structurally impossible (the
        // `rent_collector` in `CloseActivity` is of the same `SystemAccount` type), meaning
        // the remainder and the rent would stay locked forever.
        ("a token account", token_account),
    ];
    for (case, slot) in substitutions {
        env.expire_blockhash();
        let p = env.valid_init_params(DEFAULT_TGE_OFFSET);
        let ix = initialize_ix_with_slots(
            authority.pubkey(),
            TestEnv::programdata_address(),
            config_pda(),
            slot,
            vault_authority_pda(),
            &p,
        );
        let r = env.send_ixs(&[ix], &authority.pubkey(), &[&authority]);
        assert_eq!(
            custom_error_code(&r),
            Some(expected),
            "{case} in the withdrawal_authority slot → AccountNotSystemOwned during account resolution, before the body: {r:?}"
        );
        assert!(
            !env.vesting_config_exists(),
            "{case}: the singleton was not created"
        );
    }

    // POSITIVE: an ordinary wallet in the slot is accepted and its address goes
    // into the config as the single withdrawal recipient.
    env.expire_blockhash();
    let withdrawal = env.new_wallet().pubkey();
    let mut p = env.valid_init_params(DEFAULT_TGE_OFFSET);
    p.withdrawal_authority = withdrawal;
    send_initialize(&mut env, &authority, &p)
        .expect("a system owner in the withdrawal_authority slot passes");
    assert_eq!(
        env.vesting_config().withdrawal_authority,
        withdrawal,
        "the SLOT's address was written into the config as the single withdrawal recipient"
    );

    // And both activity-closing instructions really do execute against the recorded W: a
    // cancellation before TGE and the close of a fully drawn one. That is exactly what
    // would break irrecoverably with a program-owned W - the activity's remainder and
    // rent would have no addressee.
    let mint = env.win_mint;
    let beneficiary_token = env.create_ata(&withdrawal, &mint);
    let recipient = env.actors.recipients[0].insecure_clone();
    let cancelled = env.create_activity_for(&recipient.pubkey(), &ActivitySpec::valid(REFERENCE));
    let closed = env.create_activity_for(&recipient.pubkey(), &ActivitySpec::valid(REFERENCE));
    env.fund_vault(cancelled, REFERENCE);
    env.fund_vault(closed, REFERENCE);

    env.expire_blockhash();
    let ix = cancel_vesting_ix(
        authority.pubkey(),
        cancelled,
        beneficiary_token,
        withdrawal,
        mint,
    );
    env.send_ixs(&[ix], &authority.pubkey(), &[&authority])
        .expect("cancel_vesting withdraws the remainder to an account under W's custody");
    assert!(
        !env.vesting_exists(cancelled) && !env.vault_exists(cancelled),
        "the cancellation closed both of the activity's accounts"
    );
    assert_eq!(
        env.token_balance(&beneficiary_token),
        REFERENCE,
        "the cancelled activity's remainder went to an account under W's custody"
    );

    env.claim_at_day(p.tge_ts, ECOSYSTEM_FULL_RELEASE_DAY, closed, &recipient)
        .expect("the terminal claim releases the whole reference amount");
    env.expire_blockhash();
    let vault_rent = env.lamports_of(&vault_pda(closed));
    let record_rent = env.lamports_of(&vesting_pda(closed));
    let w_lamports_before = env.lamports_of(&withdrawal);
    let ix = close_vesting_ix(
        authority.pubkey(),
        closed,
        beneficiary_token,
        withdrawal,
        mint,
    );
    env.send_ixs(&[ix], &authority.pubkey(), &[&authority])
        .expect("close_vesting closes a fully drawn activity");
    assert!(
        !env.vesting_exists(closed) && !env.vault_exists(closed),
        "the close closed both of the activity's accounts"
    );
    assert_eq!(
        env.lamports_of(&withdrawal) - w_lamports_before,
        vault_rent + record_rent,
        "the rent of both accounts arrived at W - the rent addressee works"
    );
}

#[test]
fn initialize_rejects_withdrawal_authority_equal_to_vault_authority() {
    let mut env = TestEnv::new();
    let authority = env.actors.owner.insecure_clone();
    let mut p = env.valid_init_params(DEFAULT_TGE_OFFSET);
    // The load-bearing gate of finding #3: with W == vault_authority the remainder and
    // the rent from a cancellation or a close would end up in the program's own custody,
    // and the program has no instruction that takes funds off vault_authority - the money
    // would be locked forever.
    p.withdrawal_authority = vault_authority_pda();

    let r = send_initialize(&mut env, &authority, &p);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InvalidAuthority)),
        "withdrawal_authority == vault_authority → InvalidAuthority: {r:?}"
    );
    assert!(
        !env.vesting_config_exists(),
        "the singleton was not created"
    );

    // POSITIVE: a W outside the vaults' PDA is accepted and written into the
    // field.
    assert_valid_initialize_succeeds(&mut env, &authority);
    assert_eq!(
        env.vesting_config().withdrawal_authority,
        env.actors.rent_collector.pubkey(),
        "a W ≠ vault_authority was written into the config"
    );
}
