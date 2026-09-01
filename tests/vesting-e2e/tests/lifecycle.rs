//! An e2e run of the full Vesting lifecycle against surfpool: the five REAL buckets of
//! §10.5 from `initialize` through to `close_vesting`, including the atomic funding
//! transaction (Arch §2) and its measurement against the 1232-byte packet limit.
//!
//! What a real validator adds on top of the LiteSVM suite:
//! - a deploy through the BPF Loader Upgradeable and an `initialize` by a signer that
//!   genuinely holds the upgrade right (not a ProgramData fixture);
//! - the events are read "the way an indexer sees them": a json-encoded getTransaction
//!   → the inner instructions → the `EVENT_IX_TAG_LE` tag → the discriminators from
//!   target/idl/vesting.json;
//! - preflight, real fees, a real ATA rent and real transaction packets - it is here
//!   that one sees whether the funding bundle fits into 1232 bytes;
//! - the rejections (`ScheduleNotStarted`, `NothingToClaim`, `NotAuthorized`,
//!   `AccountNotInitialized` on a closed activity) arrive as codes over RPC.
//!
//! The reference values are hand-written literals from the `vesting-expected` crate
//! (their derivation lives there, in the doc comments); the program's formula is not
//! reproduced in the test.
//!
//! THE RUN'S COVERAGE: `initialize`, `set_tge`, `set_admin`, `set_operator`,
//! `propose_owner`, `accept_owner`, `set_withdrawal_authority`, `create_vesting`,
//! `set_recipient`, `cancel_vesting`, `claim`, `assert_funded` (inside the funding
//! transaction) and `close_vesting` are executed - that is, every instruction of
//! Arch §9 except `cancel_owner_proposal`; every event of §11 except
//! `OwnerProposalCancelled` is observed.
//!
//! ===== DELIBERATELY DEFERRED (there is no point running these on surfpool) =====
//! - The full negative matrices: roles 10×5, the TGE gate and slot
//!   substitutions - they rest on error codes rather than on the execution
//!   environment and live in the LiteSVM suite, where a run costs three orders of
//!   magnitude less. What is taken here are exactly the rejections that have their own
//!   price on a real validator: preflight, a real rollback of a multi-instruction
//!   transaction, and an account's absence after a close.
//! - A load run (the counterpart of tests/sale-e2e/tests/load.rs): Vesting has no user
//!   path contending for a single account - claims of different buckets do not overlap
//!   by slot.
//! - The full matrices of a rejection's observable effects: here every negative asserts
//!   that the state did not move, but sweeping every form of rejection for every
//!   instruction lives in the LiteSVM suite.
//!
//! The artifacts' freshness is guarded by gates (`vesting_test_common`): `vesting.so`
//! in `E2e::bootstrap` and `target/idl/vesting.json` in
//! `assert_idl_event_discriminators`. Both files are refreshed by the
//! `make e2e-vesting` target through `make idl` → `make build`; the gates catch a run
//! that bypasses the target.

use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use vesting::errors::VestingError;
use vesting_e2e::{
    anchor_err, assert_idl_event_discriminators, vesting_err, E2e, VestingEvent, PACKET_LIMIT,
};
use vesting_expected::vesting::{
    LIQUIDITY_REFERENCE, LIQUIDITY_VESTED_DAY_31, LIQUIDITY_VESTED_DAY_361, V1_REFERENCE,
    V1_VESTED_FROM_TGE, V2_REFERENCE, V2_VESTED_DAY_151, V2_VESTED_DAY_541, V3_REFERENCE,
    V3_VESTED_DAY_271, V3_VESTED_DAY_991, V4_LAST_TRANCHE, V4_REFERENCE, V4_VESTED_MONTHS_11,
    V4_VESTED_MONTHS_12, V5_CLAIMABLE_DAY_541,
};
use vesting_test_common::{
    accept_owner_ix, cancel_vesting_ix, claim_ix, close_vesting_ix, create_vesting_ix,
    funding_tx_ixs, initialize_ix, mint_to_vault_ix, propose_owner_ix, recipient_ata, set_admin_ix,
    set_operator_ix, set_recipient_ix, set_tge_ix, set_withdrawal_authority_ix, vault_pda,
    vesting_pda, ActivitySpec, InitParams,
};

// ---------- The "exactly this event" helpers ----------

fn as_initialized(e: VestingEvent) -> vesting::events::Initialized {
    match e {
        VestingEvent::Initialized(x) => x,
        other => panic!("Initialized was expected, got {}", other.name()),
    }
}
fn as_vesting_created(e: VestingEvent) -> vesting::events::VestingCreated {
    match e {
        VestingEvent::VestingCreated(x) => x,
        other => panic!("VestingCreated was expected, got {}", other.name()),
    }
}
fn as_claimed(e: VestingEvent) -> vesting::events::Claimed {
    match e {
        VestingEvent::Claimed(x) => x,
        other => panic!("Claimed was expected, got {}", other.name()),
    }
}
fn as_vesting_closed(e: VestingEvent) -> vesting::events::VestingClosed {
    match e {
        VestingEvent::VestingClosed(x) => x,
        other => panic!("VestingClosed was expected, got {}", other.name()),
    }
}
fn as_vesting_cancelled(e: VestingEvent) -> vesting::events::VestingCancelled {
    match e {
        VestingEvent::VestingCancelled(x) => x,
        other => panic!("VestingCancelled was expected, got {}", other.name()),
    }
}
fn as_tge_updated(e: VestingEvent) -> vesting::events::TgeUpdated {
    match e {
        VestingEvent::TgeUpdated(x) => x,
        other => panic!("TgeUpdated was expected, got {}", other.name()),
    }
}
fn as_admin_updated(e: VestingEvent) -> vesting::events::AdminUpdated {
    match e {
        VestingEvent::AdminUpdated(x) => x,
        other => panic!("AdminUpdated was expected, got {}", other.name()),
    }
}
fn as_operator_updated(e: VestingEvent) -> vesting::events::OperatorUpdated {
    match e {
        VestingEvent::OperatorUpdated(x) => x,
        other => panic!("OperatorUpdated was expected, got {}", other.name()),
    }
}
fn as_owner_proposed(e: VestingEvent) -> vesting::events::OwnerProposed {
    match e {
        VestingEvent::OwnerProposed(x) => x,
        other => panic!("OwnerProposed was expected, got {}", other.name()),
    }
}
fn as_owner_updated(e: VestingEvent) -> vesting::events::OwnerUpdated {
    match e {
        VestingEvent::OwnerUpdated(x) => x,
        other => panic!("OwnerUpdated was expected, got {}", other.name()),
    }
}
fn as_withdrawal_authority_updated(e: VestingEvent) -> vesting::events::WithdrawalAuthorityUpdated {
    match e {
        VestingEvent::WithdrawalAuthorityUpdated(x) => x,
        other => panic!(
            "WithdrawalAuthorityUpdated was expected, got {}",
            other.name()
        ),
    }
}
fn as_recipient_updated(e: VestingEvent) -> vesting::events::RecipientUpdated {
    match e {
        VestingEvent::RecipientUpdated(x) => x,
        other => panic!("RecipientUpdated was expected, got {}", other.name()),
    }
}

// ---------- The scenario's reference values ----------

/// The TGE's offset from the run's start: one hour. It fits inside `MAX_TGE_DELAY`
/// (365 days) and leaves a "before TGE" window for the `ScheduleNotStarted` negative.
const TGE_OFFSET: i64 = 3_600;

/// How far `set_tge` moves the date forward: another ten minutes. The offsets still sum
/// to well inside `MAX_TGE_DELAY`.
const TGE_SHIFT: i64 = 600;

/// A stray surplus minted into the Liquidity vault beyond the reference amount
/// (Arch §7): it does not move the unlock, will not be released to the recipient and
/// goes to the single withdrawal target when the activity is closed. The value is a
/// multiple of neither a tranche nor the reference amount.
const STRAY: u64 = 4_242;

/// The total emission of the five buckets - a hand-written literal:
/// `V1 5×10¹⁵ + V2 10¹⁶ + Community 1000 + Treasury 4×10¹⁵ + Liquidity 10¹⁶`
/// = `29,000,000,000,000,000 + 1,000`.
const TOTAL_EMISSION: u64 = 29_000_000_000_001_000;

const _: () = assert!(
    TOTAL_EMISSION
        == V1_REFERENCE + V2_REFERENCE + V4_REFERENCE + V3_REFERENCE + LIQUIDITY_REFERENCE,
    "the total-emission literal diverged from the buckets' reference amounts"
);

/// Treasury's remainder by day 991 after the day-271 claim: `4×10¹⁵ − 1.6×10¹⁴ =
/// 3,840,000,000,000,000` (24 monthly tranches of 1.6×10¹⁴ plus the flooring top-up,
/// of which there is none here - 10000 = 25 × 400 with no remainder).
const TREASURY_TAIL: u64 = 3_840_000_000_000_000;

const _: () = assert!(
    TREASURY_TAIL + V3_VESTED_DAY_271 == V3_VESTED_DAY_991,
    "Treasury's tail diverged from the references of days 271 and 991"
);

/// Liquidity's remainder by day 361 after the day-31 claim: `10¹⁶ − 1.75×10¹⁵ =
/// 8,250,000,000,000,000` (11 monthly tranches of 7.5×10¹⁴).
const LIQUIDITY_TAIL: u64 = 8_250_000_000_000_000;

const _: () = assert!(
    LIQUIDITY_TAIL + LIQUIDITY_VESTED_DAY_31 == LIQUIDITY_VESTED_DAY_361,
    "Liquidity's tail diverged from the references of days 31 and 361"
);

// The Ecosystem bucket is paid to two wallets: before the recipient change it is the
// day-151 payout and afterwards the day-541 remainder. Together they must give exactly
// the bucket's reference amount, or the balance assertions below would be checking
// something other than "the whole reference amount, not a unit more".
const _: () = assert!(
    V2_VESTED_DAY_151 + V5_CLAIMABLE_DAY_541 == V2_REFERENCE,
    "the payouts of days 151 and 541 do not sum to the Ecosystem bucket's reference amount"
);

/// The SPL Token program's `TokenError::FixedSupply` code: the sixth variant of its
/// enum (`NotRentExempt` = 0 … `FixedSupply` = 5), arriving as `Custom(5)`. A literal:
/// this is a foreign interface, and taking its code from the crate would mean checking
/// the encoding against itself. The value was confirmed by execution (and by the
/// LiteSVM suite).
const SPL_ERROR_FIXED_SUPPLY: u32 = 5;

/// The instructions of a single claim: `set_compute_unit_limit` plus the `claim`
/// itself.
///
/// The prefix exists for the transaction's BYTE uniqueness: the slot ticker is stopped
/// during the run (deterministic time), the blockhash is not rotated, and two claims of
/// one activity by one key would be byte-for-byte identical - the validator would
/// filter the second out as already processed BEFORE execution, and the program would
/// not run at all. The limit does not affect the payout itself: 400k is well above a
/// claim's cost with an ATA creation.
fn claim_ixs(
    token_mint: &Pubkey,
    activity_id: u64,
    signer: &Pubkey,
    recipient: &Pubkey,
    cu_nonce: u32,
) -> Vec<Instruction> {
    vec![
        ComputeBudgetInstruction::set_compute_unit_limit(400_000 + cu_nonce),
        claim_ix(*signer, activity_id, *recipient, *token_mint),
    ]
}

/// The full lifecycle of the five buckets of §10.5 on a real validator simulator.
///
/// The landmark days (Arch §10.1, `day = 1 + 30·p`): 1 (TGE, Marketing in full), 31
/// (Liquidity), 151 (Ecosystem, claimed by the Operator, after which the Ecosystem recipient is
/// reassigned), 270/271 (the edge of Treasury's cliff), 331/361 (Community: 11 tranches of
/// 83, then the terminal top-up of 87), 541 (Ecosystem in full - the remainder now goes to
/// the new recipient), 991 (Treasury in full) - and then all five activities are
/// closed.
#[test]
#[ignore = "e2e: requires surfpool, run it through make e2e-vesting"]
fn five_sections_full_lifecycle_on_surfpool() {
    // The event discriminators are checked against the IDL before the scenario: an indexer
    // reads events by the IDL, and a divergence would invalidate every event assertion
    // below.
    assert_idl_event_discriminators();

    let env = E2e::bootstrap(8911, 5, true);
    let win = env.win_mint;
    let payer = env.payer.insecure_clone();
    let owner = env.actors.owner.insecure_clone();
    let admin = env.actors.admin.insecure_clone();
    let operator = env.actors.operator.insecure_clone();
    let withdrawal = env.actors.withdrawal.insecure_clone();
    let recipients: Vec<_> = env
        .actors
        .recipients
        .iter()
        .map(|k| k.insecure_clone())
        .collect();
    // Short names for the recipients' keys: the indices match the order of the buckets of
    // §10.5 (0 - Marketing, 1 - Ecosystem, 2 - Community, 3 - Treasury, 4 - Liquidity).
    let (r0, r1, r2, r3, r4) = (
        recipients[0].pubkey(),
        recipients[1].pubkey(),
        recipients[2].pubkey(),
        recipients[3].pubkey(),
        recipients[4].pubkey(),
    );

    // ---------- initialize ----------
    let tge_ts = env.now() + TGE_OFFSET;
    let params = InitParams {
        owner: owner.pubkey(),
        admin: admin.pubkey(),
        operator: operator.pubkey(),
        token_mint: win,
        withdrawal_authority: withdrawal.pubkey(),
        tge_ts,
    };
    // The signer is the holder of the upgrade authority (which also deployed the program)
    // rather than the Owner-by-argument: the right to init is bound to an on-chain fact
    // (Arch §9.1).
    let sig = env.send(
        &[initialize_ix(owner.pubkey(), &params)],
        &owner.pubkey(),
        &[&owner],
    );
    let ev = as_initialized(env.single_event(&sig));
    assert_eq!(ev.owner, owner.pubkey());
    assert_eq!(ev.admin, admin.pubkey());
    assert_eq!(ev.operator, operator.pubkey());
    assert_eq!(ev.token_mint, win);
    assert_eq!(ev.withdrawal_authority, withdrawal.pubkey());
    assert_eq!(ev.tge_ts, tge_ts);

    let cfg = env.vesting_config();
    assert_eq!(cfg.owner, owner.pubkey());
    assert_eq!(cfg.admin, admin.pubkey());
    assert_eq!(cfg.operator, operator.pubkey());
    assert_eq!(cfg.token_mint, win);
    assert_eq!(cfg.withdrawal_authority, withdrawal.pubkey());
    assert_eq!(cfg.tge_ts, tge_ts);
    assert_eq!(cfg.activity_count, 0, "there are no activities yet");

    // ---------- parameters before TGE: the date and all three roles ----------
    // The TGE date moves while it has not arrived (Arch §4.2) and Admin signs for it; the
    // scenario then proceeds from the NEW date - otherwise the landmark days would be
    // counted against a schedule that does not exist.
    let tge_ts = tge_ts + TGE_SHIFT;
    let sig = env.send(
        &[set_tge_ix(admin.pubkey(), tge_ts)],
        &admin.pubkey(),
        &[&admin],
    );
    assert_eq!(as_tge_updated(env.single_event(&sig)).tge_ts, tge_ts);
    assert_eq!(env.vesting_config().tge_ts, tge_ts);

    // Admin is reassigned and restored: the restoration is needed by the scenario - the
    // date and the recipient are edited further on by the `admin` actor itself.
    for candidate in [withdrawal.pubkey(), admin.pubkey()] {
        let sig = env.send(
            &[set_admin_ix(owner.pubkey(), candidate)],
            &owner.pubkey(),
            &[&owner],
        );
        assert_eq!(as_admin_updated(env.single_event(&sig)).admin, candidate);
        assert_eq!(env.vesting_config().admin, candidate);
    }

    // Operator is reassigned and restored: both changes are observable in the event and in
    // the config. The restoration is needed by the scenario - on day 151 it is `operator`
    // that claims.
    for candidate in [withdrawal.pubkey(), operator.pubkey()] {
        let sig = env.send(
            &[set_operator_ix(owner.pubkey(), candidate)],
            &owner.pubkey(),
            &[&owner],
        );
        assert_eq!(
            as_operator_updated(env.single_event(&sig)).operator,
            candidate
        );
        assert_eq!(env.vesting_config().operator, candidate);
    }

    // Ownership is handed to `payer` and back. The handover is two-step: the sitting owner
    // names a candidate and the candidate signs the acceptance. The reverse handover is
    // nominated by `payer` already - the right follows the config's field rather than the
    // key the program was initialized with.
    let sig = env.send(
        &[propose_owner_ix(owner.pubkey(), payer.pubkey())],
        &owner.pubkey(),
        &[&owner],
    );
    assert_eq!(
        as_owner_proposed(env.single_event(&sig)).pending_owner,
        payer.pubkey()
    );
    assert_eq!(env.vesting_config().pending_owner, Some(payer.pubkey()));
    assert_eq!(env.vesting_config().owner, owner.pubkey());

    let sig = env.send(
        &[accept_owner_ix(payer.pubkey())],
        &payer.pubkey(),
        &[&payer],
    );
    assert_eq!(
        as_owner_updated(env.single_event(&sig)).new_owner,
        payer.pubkey()
    );
    assert_eq!(env.vesting_config().owner, payer.pubkey());
    assert_eq!(env.vesting_config().pending_owner, None);

    let sig = env.send(
        &[propose_owner_ix(payer.pubkey(), owner.pubkey())],
        &payer.pubkey(),
        &[&payer],
    );
    assert_eq!(
        as_owner_proposed(env.single_event(&sig)).pending_owner,
        owner.pubkey()
    );
    let sig = env.send(
        &[accept_owner_ix(owner.pubkey())],
        &owner.pubkey(),
        &[&owner],
    );
    assert_eq!(
        as_owner_updated(env.single_event(&sig)).new_owner,
        owner.pubkey()
    );
    assert_eq!(env.vesting_config().owner, owner.pubkey());
    assert_eq!(env.vesting_config().pending_owner, None);

    // The withdrawal target is reassigned and restored: the restoration is needed by the
    // scenario - on the activities' cancellation and close the addressee of the remainder
    // and the rent is the `withdrawal` actor. Admin signs, as for `set_tge` and
    // `set_recipient`.
    for candidate in [payer.pubkey(), withdrawal.pubkey()] {
        let sig = env.send(
            &[set_withdrawal_authority_ix(admin.pubkey(), candidate)],
            &admin.pubkey(),
            &[&admin],
        );
        assert_eq!(
            as_withdrawal_authority_updated(env.single_event(&sig)).withdrawal_authority,
            candidate
        );
        assert_eq!(env.vesting_config().withdrawal_authority, candidate);
    }

    // ---------- create_vesting ×5 (before TGE) ----------
    let sections = ActivitySpec::sections_10_5();
    // The (activity_id, reference_amount) pairs in the order of the table of §10.5.
    let mut funding: Vec<(u64, u64)> = Vec::new();
    for (i, spec) in sections.iter().enumerate() {
        let recipient = recipients[i].pubkey();
        // The identifier is taken from the chain: the instruction itself assigns it from
        // `config.activity_count`, and the test keeps no copy of the counter.
        let activity_id = env.vesting_config().activity_count;
        let sig = env.send(
            &[create_vesting_ix(
                owner.pubkey(),
                recipient,
                activity_id,
                win,
                spec,
            )],
            &owner.pubkey(),
            &[&owner],
        );
        let ev = as_vesting_created(env.single_event(&sig));
        assert_eq!(ev.activity_id, activity_id);
        assert_eq!(ev.recipient, recipient);
        assert_eq!(ev.reference_amount, spec.reference_amount);
        assert_eq!(ev.tge_bps, spec.tge_bps);
        assert_eq!(ev.monthly_bps, spec.monthly_bps);
        assert_eq!(ev.cliff_months, spec.cliff_months);
        // `label` is not stored on-chain (Arch §5.2) - it is visible only in the event.
        assert_eq!(ev.label, spec.label);

        let record = env.vesting(activity_id);
        assert_eq!(record.recipient, recipient);
        assert_eq!(record.reference_amount, spec.reference_amount);
        assert_eq!(record.claimed, 0);
        assert_eq!(record.tge_bps, spec.tge_bps);
        assert_eq!(record.monthly_bps, spec.monthly_bps);
        assert_eq!(record.cliff_months, spec.cliff_months);
        assert_eq!(
            env.vault_balance(activity_id),
            0,
            "the vault was created empty: funding is a separate transaction"
        );
        funding.push((activity_id, spec.reference_amount));
    }
    assert_eq!(env.vesting_config().activity_count, 5);

    // The single withdrawal target (I12): `withdrawal_authority`'s ATA under $1WIN - the
    // addressee of the remainder both on a cancellation and on a close.
    let beneficiary_token = env.create_ata(&withdrawal.pubkey(), &win);

    // ---------- a mistaken activity: create → cancel before TGE ----------
    let cancelled_id = env.vesting_config().activity_count;
    env.send(
        &[create_vesting_ix(
            owner.pubkey(),
            r0,
            cancelled_id,
            win,
            &ActivitySpec::valid(V4_REFERENCE),
        )],
        &owner.pubkey(),
        &[&owner],
    );
    let vesting_rent = env.lamports(&vesting_pda(cancelled_id));
    let vault_rent = env.lamports(&vault_pda(cancelled_id));
    let withdrawal_before = env.lamports(&withdrawal.pubkey());
    let sig = env.send(
        &[cancel_vesting_ix(
            owner.pubkey(),
            cancelled_id,
            beneficiary_token,
            withdrawal.pubkey(),
            win,
        )],
        &owner.pubkey(),
        &[&owner],
    );
    let ev = as_vesting_cancelled(env.single_event(&sig));
    assert_eq!(ev.activity_id, cancelled_id);
    assert_eq!(
        ev.amount, 0,
        "the vault was empty - there was nothing to drain"
    );
    assert_eq!(ev.beneficiary_token, beneficiary_token);
    assert_eq!(ev.rent_collector, withdrawal.pubkey());
    assert!(!env.account_exists(&vesting_pda(cancelled_id)));
    assert!(!env.account_exists(&vault_pda(cancelled_id)));
    assert_eq!(
        env.lamports(&withdrawal.pubkey()),
        withdrawal_before + vesting_rent + vault_rent,
        "the rent of both accounts went to the withdrawal target (the fee payer is Owner, so the sum is exact)"
    );
    assert_eq!(
        env.vesting_config().activity_count,
        6,
        "the id counter is not decremented on a cancellation - the cancelled activity's address is not reused (I8)"
    );

    // ---------- the recipient is reassigned before TGE ----------
    // There and back: the second change returns the bucket to its rightful recipient, from
    // which all the Marketing claims proceed.
    for candidate in [withdrawal.pubkey(), r0] {
        let sig = env.send(
            &[set_recipient_ix(admin.pubkey(), funding[0].0, candidate)],
            &admin.pubkey(),
            &[&admin],
        );
        let ev = as_recipient_updated(env.single_event(&sig));
        assert_eq!(ev.activity_id, funding[0].0);
        assert_eq!(ev.new_recipient, candidate);
        assert_eq!(env.vesting(funding[0].0).recipient, candidate);
    }

    let (marketing, ecosystem, community, treasury, liquidity) = (
        funding[0].0,
        funding[1].0,
        funding[2].0,
        funding[3].0,
        funding[4].0,
    );

    // ---------- the atomic funding transaction (Arch §2) ----------
    //
    // THE MEASUREMENT (this run): a bundle of 11 instructions (5 `mint_to` + 5
    // `assert_funded` + `SetAuthority(None)`) for five buckets = 817 BYTES out of 1232,
    // that is 415 bytes of headroom; CU = 74,000. The verdict: it fits, and the fallback
    // of "`assert_funded` in a separate transaction" is unnecessary. The headroom allows
    // roughly 7 more buckets (about 50 bytes per bucket: two account indices and two
    // instructions), but the bundle of Arch §2 also includes the `mint_to` into
    // TokenSale's `token_vault` - so the measurement is repeated whenever the set of
    // buckets is extended.
    //
    // The Liquidity vault receives the reference amount PLUS the stray surplus: the
    // coverage check permits it and the unlock does not see it (Arch §7).
    let mut amounts = funding.clone();
    amounts[4].1 += STRAY;

    assert_eq!(
        env.mint_authority(&win),
        Some(payer.pubkey()),
        "precondition: the issuer still holds the right to mint further"
    );
    assert_eq!(env.mint_supply(&win), 0, "precondition: the supply is zero");

    // An under-mint of one vault fails the whole bundle - before the authority is revoked.
    let mut short = amounts.clone();
    short[2].1 -= 1;
    env.send_expect_custom(
        &funding_tx_ixs(&win, &payer.pubkey(), &short),
        &payer.pubkey(),
        &[&payer],
        vesting_err(VestingError::InsufficientVaultBalance),
    );
    assert_eq!(
        env.mint_supply(&win),
        0,
        "the mints rolled back along with the transaction: the bundle is atomic"
    );
    assert_eq!(
        env.mint_authority(&win),
        Some(payer.pubkey()),
        "the right to issue further is in place - the check stands BEFORE SetAuthority(None)"
    );

    // Full funding: 5 mint_to + 5 assert_funded + SetAuthority(None).
    let ixs = funding_tx_ixs(&win, &payer.pubkey(), &amounts);
    assert_eq!(ixs.len(), 11, "the shape of the bundle of Arch §2");
    let tx = env.build_tx(&ixs, &payer.pubkey(), &[&payer]);
    let size = E2e::tx_size(&tx);
    println!("the funding transaction for five buckets: {size} bytes out of {PACKET_LIMIT}");
    assert!(
        size <= PACKET_LIMIT,
        "the funding transaction does not fit into the packet: {size} > {PACKET_LIMIT}"
    );
    let sig = env.send_prebuilt(&tx);
    println!(
        "the funding transaction's CU: {:?}",
        env.compute_units(&sig)
    );
    assert!(
        env.events_of(&sig).is_empty(),
        "assert_funded emits no events (Arch §11: nine events against ten instructions)"
    );

    for (id, amount) in &amounts {
        assert_eq!(
            env.vault_balance(*id),
            *amount,
            "activity {id}: the vault holds exactly what was minted"
        );
    }
    assert_eq!(
        env.mint_supply(&win),
        TOTAL_EMISSION + STRAY,
        "exactly what went into the five vaults was issued"
    );
    assert_eq!(
        env.mint_authority(&win),
        None,
        "the bundle's closing link revoked the right to issue further (T-6)"
    );
    // No further issuance is possible after the revocation - SPL Token itself rejects it.
    let marketing_before = env.vault_balance(marketing);
    env.send_expect_custom(
        &[mint_to_vault_ix(&win, &payer.pubkey(), marketing, 1)],
        &payer.pubkey(),
        &[&payer],
        SPL_ERROR_FIXED_SUPPLY,
    );
    assert_eq!(
        env.vault_balance(marketing),
        marketing_before,
        "the rejected further issuance did not touch the vault"
    );
    assert_eq!(
        env.mint_supply(&win),
        TOTAL_EMISSION + STRAY,
        "the supply did not move: not a unit was issued"
    );

    // ---------- before TGE there are no claims ----------
    env.send_expect_custom(
        &claim_ixs(&win, marketing, &r0, &r0, 0),
        &r0,
        &[&recipients[0]],
        vesting_err(VestingError::ScheduleNotStarted),
    );
    assert!(
        !env.account_exists(&recipient_ata(&r0, &win)),
        "the pre-TGE rejection left not even an ATA (init_if_needed rolled back with the tx)"
    );

    // ---------- day 1: Marketing - 100% at the TGE instant ----------
    env.warp_to_day(tge_ts, 1);
    let sig = env.send(
        &claim_ixs(&win, marketing, &r0, &r0, 1),
        &r0,
        &[&recipients[0]],
    );
    let ev = as_claimed(env.single_event(&sig));
    assert_eq!(ev.activity_id, marketing);
    assert_eq!(ev.recipient, r0);
    assert_eq!(ev.amount, V1_VESTED_FROM_TGE);
    assert_eq!(ev.claimed, V1_VESTED_FROM_TGE);
    assert_eq!(env.recipient_ata_balance(&r0), V1_VESTED_FROM_TGE);
    assert_eq!(
        env.vault_balance(marketing),
        0,
        "the vault emptied out exactly"
    );
    assert_eq!(env.vesting(marketing).claimed, V1_VESTED_FROM_TGE);

    // A repeat on the same day: everything is unlocked and everything was released.
    env.send_expect_custom(
        &claim_ixs(&win, marketing, &r0, &r0, 2),
        &r0,
        &[&recipients[0]],
        vesting_err(VestingError::NothingToClaim),
    );
    assert_eq!(
        env.recipient_ata_balance(&r0),
        V1_VESTED_FROM_TGE,
        "the repeated claim added not a single unit to the recipient"
    );
    assert_eq!(
        env.vesting(marketing).claimed,
        V1_VESTED_FROM_TGE,
        "the released counter did not move - there was no second payout"
    );

    // ---------- day 31: Liquidity - the TGE share plus the first monthly tranche ----------
    env.warp_to_day(tge_ts, 31);
    let sig = env.send(
        &claim_ixs(&win, liquidity, &r4, &r4, 3),
        &r4,
        &[&recipients[4]],
    );
    let ev = as_claimed(env.single_event(&sig));
    assert_eq!(ev.amount, LIQUIDITY_VESTED_DAY_31);
    assert_eq!(env.recipient_ata_balance(&r4), LIQUIDITY_VESTED_DAY_31);
    assert_eq!(
        env.vault_balance(liquidity),
        LIQUIDITY_REFERENCE + STRAY - LIQUIDITY_VESTED_DAY_31,
        "the vault holds the undelivered obligation plus the whole stray surplus"
    );

    // ---------- day 151: Ecosystem, claimed by the Operator ----------
    env.warp_to_day(tge_ts, 151);
    let sig = env.send(
        &claim_ixs(&win, ecosystem, &operator.pubkey(), &r1, 4),
        &operator.pubkey(),
        &[&operator],
    );
    let ev = as_claimed(env.single_event(&sig));
    assert_eq!(ev.amount, V2_VESTED_DAY_151);
    assert_eq!(
        ev.recipient, r1,
        "the recipient in the event is the bucket's owner rather than the signer"
    );
    assert_eq!(
        env.recipient_ata_balance(&r1),
        V2_VESTED_DAY_151,
        "the payout went to the recipient even though the Operator signed (Arch §8)"
    );
    assert!(
        !env.account_exists(&recipient_ata(&operator.pubkey(), &win)),
        "the Operator did not acquire a $1WIN account of its own"
    );

    // An outsider (neither the recipient nor the Operator) has no right to claim.
    let games_vault_before = env.vault_balance(ecosystem);
    env.send_expect_custom(
        &claim_ixs(&win, ecosystem, &payer.pubkey(), &r1, 5),
        &payer.pubkey(),
        &[&payer],
        vesting_err(VestingError::NotAuthorized),
    );
    assert_eq!(
        env.vault_balance(ecosystem),
        games_vault_before,
        "the rejection of the outsider did not move the bucket's vault"
    );
    assert_eq!(
        env.recipient_ata_balance(&r1),
        V2_VESTED_DAY_151,
        "the payout to the recipient stayed the same"
    );

    // ---------- the Ecosystem recipient is reassigned AFTER TGE and a partial payout ----------
    // From here the bucket is claimed by r0: what was released to r1 stays with it, and
    // the undelivered remainder goes to the new wallet.
    let sig = env.send(
        &[set_recipient_ix(admin.pubkey(), ecosystem, r0)],
        &admin.pubkey(),
        &[&admin],
    );
    let ev = as_recipient_updated(env.single_event(&sig));
    assert_eq!(ev.activity_id, ecosystem);
    assert_eq!(ev.new_recipient, r0);
    assert_eq!(env.vesting(ecosystem).recipient, r0);
    assert_eq!(
        env.vesting(ecosystem).claimed,
        V2_VESTED_DAY_151,
        "the accounting of what was released continues from its previous value"
    );

    // ---------- day 270 / 271: the edge of Treasury's cliff ----------
    env.warp_to_day(tge_ts, 270);
    env.send_expect_custom(
        &claim_ixs(&win, treasury, &r3, &r3, 6),
        &r3,
        &[&recipients[3]],
        vesting_err(VestingError::NothingToClaim),
    );
    assert_eq!(
        env.vesting(treasury).claimed,
        0,
        "inside the cliff nothing was released"
    );

    env.warp_to_day(tge_ts, 271);
    let sig = env.send(
        &claim_ixs(&win, treasury, &r3, &r3, 7),
        &r3,
        &[&recipients[3]],
    );
    let ev = as_claimed(env.single_event(&sig));
    assert_eq!(
        ev.amount, V3_VESTED_DAY_271,
        "the first monthly tranche lands exactly on day 271"
    );
    assert_eq!(env.vesting(treasury).claimed, V3_VESTED_DAY_271);

    // ---------- day 331 / 361: Community - the flooring and the terminal top-up ----------
    env.warp_to_day(tge_ts, 331);
    let sig = env.send(
        &claim_ixs(&win, community, &r2, &r2, 8),
        &r2,
        &[&recipients[2]],
    );
    let ev = as_claimed(env.single_event(&sig));
    assert_eq!(
        ev.amount, V4_VESTED_MONTHS_11,
        "11 tranches of 83 = 913 units, the reference of 1000 is not topped up yet"
    );
    assert_eq!(
        env.vault_balance(community),
        V4_REFERENCE - V4_VESTED_MONTHS_11
    );

    env.warp_to_day(tge_ts, 361);
    let sig = env.send(
        &claim_ixs(&win, community, &r2, &r2, 9),
        &r2,
        &[&recipients[2]],
    );
    let ev = as_claimed(env.single_event(&sig));
    assert_eq!(
        ev.amount, V4_LAST_TRANCHE,
        "the terminal tranche of 87 > the nominal 83: the lost flooring is topped up"
    );
    assert_eq!(ev.claimed, V4_VESTED_MONTHS_12);
    assert_eq!(
        env.recipient_ata_balance(&r2),
        V4_REFERENCE,
        "the Community recipient received EXACTLY the reference amount - not a unit more"
    );
    assert_eq!(env.vault_balance(community), 0);

    // That same day 361 is Liquidity's terminal period.
    let sig = env.send(
        &claim_ixs(&win, liquidity, &r4, &r4, 10),
        &r4,
        &[&recipients[4]],
    );
    let ev = as_claimed(env.single_event(&sig));
    assert_eq!(ev.amount, LIQUIDITY_TAIL);
    assert_eq!(ev.claimed, LIQUIDITY_VESTED_DAY_361);
    assert_eq!(env.recipient_ata_balance(&r4), LIQUIDITY_REFERENCE);
    assert_eq!(
        env.vault_balance(liquidity),
        STRAY,
        "the stray surplus was NOT released to the recipient: the unlock is computed against the reference amount"
    );

    // ---------- day 541: Ecosystem in full, now to the new recipient ----------
    env.warp_to_day(tge_ts, 541);
    let sig = env.send(
        &claim_ixs(&win, ecosystem, &r0, &r0, 11),
        &r0,
        &[&recipients[0]],
    );
    let ev = as_claimed(env.single_event(&sig));
    assert_eq!(
        ev.amount, V5_CLAIMABLE_DAY_541,
        "what accrued over the skipped periods is released in a single claim"
    );
    assert_eq!(ev.claimed, V2_VESTED_DAY_541);
    assert_eq!(
        ev.recipient, r0,
        "the payout's addressee is the reassigned wallet"
    );
    assert_eq!(
        env.recipient_ata_balance(&r1),
        V2_VESTED_DAY_151,
        "the previous Ecosystem recipient received not a single unit after the change"
    );
    assert_eq!(
        env.recipient_ata_balance(&r0),
        V1_VESTED_FROM_TGE + V5_CLAIMABLE_DAY_541,
        "the new recipient received the undelivered Ecosystem remainder on top of its own bucket"
    );
    assert_eq!(env.vault_balance(ecosystem), 0);

    // ---------- day 991: Treasury in full ----------
    env.warp_to_day(tge_ts, 991);
    let sig = env.send(
        &claim_ixs(&win, treasury, &r3, &r3, 12),
        &r3,
        &[&recipients[3]],
    );
    let ev = as_claimed(env.single_event(&sig));
    assert_eq!(ev.amount, TREASURY_TAIL);
    assert_eq!(ev.claimed, V3_VESTED_DAY_991);
    assert_eq!(env.recipient_ata_balance(&r3), V3_REFERENCE);
    assert_eq!(env.vault_balance(treasury), 0);

    // ---------- close_vesting for all five ----------
    // The remainder and the rent go to the SINGLE withdrawal target (I12):
    // `rent_collector` is `withdrawal_authority` itself and `beneficiary_token` is its ATA
    // under $1WIN (created before the mistaken activity was cancelled above).
    for (i, (id, reference)) in funding.iter().enumerate() {
        // A surplus was left only in the Liquidity vault - the rest hold exactly zero.
        let leftover = if i == 4 { STRAY } else { 0 };
        assert_eq!(
            env.vault_balance(*id),
            leftover,
            "activity {id}: the remainder"
        );
        assert_eq!(
            env.vesting(*id).claimed,
            *reference,
            "activity {id}: the obligation was released in full - only then is it closable"
        );

        let vesting_rent = env.lamports(&vesting_pda(*id));
        let vault_rent = env.lamports(&vault_pda(*id));
        let withdrawal_before = env.lamports(&withdrawal.pubkey());
        let sig = env.send(
            &[close_vesting_ix(
                owner.pubkey(),
                *id,
                beneficiary_token,
                withdrawal.pubkey(),
                win,
            )],
            &owner.pubkey(),
            &[&owner],
        );
        let ev = as_vesting_closed(env.single_event(&sig));
        assert_eq!(ev.activity_id, *id);
        assert_eq!(
            ev.amount, leftover,
            "the event carries the drained remainder"
        );
        assert_eq!(ev.beneficiary_token, beneficiary_token);
        assert_eq!(ev.rent_collector, withdrawal.pubkey());
        assert!(
            !env.account_exists(&vesting_pda(*id)),
            "the record is closed"
        );
        assert!(!env.account_exists(&vault_pda(*id)), "the vault is closed");
        assert_eq!(
            env.lamports(&withdrawal.pubkey()),
            withdrawal_before + vesting_rent + vault_rent,
            "the rent of BOTH accounts went to the withdrawal target (the fee payer is Owner, so the sum is exact)"
        );
    }
    assert_eq!(
        env.token_balance(&beneficiary_token),
        STRAY,
        "the withdrawal target received only the stray surplus: the obligations went to the recipients"
    );
    assert_eq!(
        env.vesting_config().activity_count,
        6,
        "the id counter is decremented neither on a cancellation nor on a close - the addresses are not reused (I8)"
    );

    // A repeated claim on a CLOSED activity: the account is gone and Anchor rejects it at
    // resolution - the close IS the replay guard.
    env.send_expect_custom(
        &claim_ixs(&win, marketing, &r0, &r0, 13),
        &r0,
        &[&recipients[0]],
        anchor_err(anchor_lang::error::ErrorCode::AccountNotInitialized),
    );
    // The payout did not change as a result.
    assert_eq!(
        env.recipient_ata_balance(&r0),
        V1_VESTED_FROM_TGE + V5_CLAIMABLE_DAY_541
    );

    println!("[e2e] the five buckets of §10.5: the full cycle passed on surfpool");
}
