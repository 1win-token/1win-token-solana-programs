//! Scenario helpers: translating "schedule period / day" into a timestamp and
//! jumping there via the Clock.
//!
//! The counting follows Arch §10.1: period `p` arrives at `tge_ts + p·MONTH`
//! (`p = 0` is the TGE instant), and the day is computed as `day = 1 + p·30`, so
//! day 1 is TGE itself, day 31 is the first monthly tranche without a cliff, and day
//! 991 is Treasury's full release.
//!
//! `MONTH` and `DAY_SECS` come from `vesting_expected::vesting` - the crate of
//! hand-written reference values with zero dependencies. Taking them from the
//! `vesting` crate is forbidden: a shift of the constant in the program would then
//! shift the test's expectations too (tests-bind-to-real-code).
//!
//! The scenario wrappers over activities are `create_activity` / `fund_vault`
//! (fixtures built the honest way) and `set_vault_amount` (a forge), plus
//! sending a claim (`claim_as`) and the chain step "reach a day and claim"
//! (`claim_at_day`).

// DUP: to be merged into svm-core (follow-up) - Sale's counterparts live in ix.rs
// (`init_default_sale`); pulling them into a separate module is a follow-up.

use litesvm::types::TransactionResult;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use vesting_expected::vesting::{DAY_SECS, MONTH};

use crate::env::TestEnv;
use crate::ix::{claim_ix, create_vesting_ix, initialize_ix, vault_pda, ActivitySpec, InitParams};

/// The default TGE offset: one month from `now`. It fits inside `MAX_TGE_DELAY`
/// (365 days) with room to spare and leaves space for both "before TGE" and "after
/// TGE" within one scenario.
pub const DEFAULT_TGE_OFFSET: i64 = MONTH;

/// The timestamp at which period `p` begins relative to TGE: `tge_ts + p·MONTH`
/// (Arch §10.1). `p = 0` is the TGE instant.
///
/// The type is `i64` (rather than `u32`) so that days and periods from
/// `vesting_expected::vesting` substitute in without a cast; a negative period is
/// rejected by a panic. It panics on an i64 overflow too: a test scenario with such
/// a date is plainly wrong (the `tge_ts` ceiling is `MAX_TGE_DELAY`, 365 days).
pub fn at_period(tge_ts: i64, p: i64) -> i64 {
    assert!(p >= 0, "the period number is non-negative: got {p}");
    p.checked_mul(MONTH)
        .and_then(|offset| tge_ts.checked_add(offset))
        .unwrap_or_else(|| panic!("period {p} from tge_ts {tge_ts} does not fit into i64"))
}

impl TestEnv {
    /// A valid, self-consistent set of `initialize` arguments: the roles are the
    /// environment's actors (owner, admin and operator - three distinct non-zero keys),
    /// the mint is the environment's `win_mint` (8 decimals), and
    /// `tge_ts = now + tge_offset`.
    ///
    /// `withdrawal_authority` is the `rent_collector` actor: a single withdrawal
    /// recipient (the "one W" model). Then `rent_collector` in a cancellation or a close
    /// is that same actor, and the canonical `beneficiary_token` is its ATA under
    /// `win_mint`. It differs from owner, admin, operator and vault_authority and is
    /// non-zero, so it is valid.
    ///
    /// The helper does not check the `tge_offset` ceiling: `MAX_TGE_DELAY` is the
    /// subject of a boundary test, and duplicating it here would reject the boundary
    /// scenario before the instruction sees it. A `tge_offset ≤ 0`, on the other hand,
    /// is rejected by a panic: the helper is called "a valid set" and must stay one.
    /// Negatives edit a field of the returned copy (`p.tge_ts = env.now()`) instead of
    /// asking it for arguments that are invalid by construction.
    pub fn valid_init_params(&self, tge_offset: i64) -> InitParams {
        assert!(
            tge_offset > 0,
            "TGE must be strictly in the future: got offset {tge_offset}"
        );
        let tge_ts = self
            .now()
            .checked_add(tge_offset)
            .unwrap_or_else(|| panic!("offset {tge_offset} from now does not fit into i64"));
        InitParams {
            owner: self.actors.owner.pubkey(),
            admin: self.actors.admin.pubkey(),
            operator: self.actors.operator.pubkey(),
            token_mint: self.win_mint,
            withdrawal_authority: self.actors.rent_collector.pubkey(),
            tge_ts,
        }
    }

    /// Creates the singleton config with valid parameters
    /// ([`TestEnv::valid_init_params`]) and returns the values that were used. The
    /// signer is the `owner` actor, which is also the environment's default upgrade
    /// authority.
    ///
    /// It panics on rejection: `initialize` here is a fixture, not a guard under test,
    /// and its failure means a broken environment (for instance, a `TestEnv` assembled
    /// through `with_upgrade_authority` with a foreign key).
    pub fn init_default(&mut self, tge_offset: i64) -> InitParams {
        let authority = self.actors.owner.insecure_clone();
        let params = self.valid_init_params(tge_offset);
        self.send_ixs(
            &[initialize_ix(authority.pubkey(), &params)],
            &authority.pubkey(),
            &[&authority],
        )
        .expect("init_default: initialize must succeed");
        params
    }

    /// Creates an activity on the `spec` schedule for recipient `recipient` and returns
    /// the `activity_id` assigned to it. The signer is the `owner` actor and the mint is
    /// the environment's `win_mint` (the one pinned in the config).
    ///
    /// It panics on rejection: `create_vesting` here is a fixture, not a guard under
    /// test (the negatives exercise that guard through the builder directly). A failure
    /// means a broken environment: no config, TGE already reached, or an invalid
    /// schedule.
    pub fn create_activity_for(&mut self, recipient: &Pubkey, spec: &ActivitySpec) -> u64 {
        let owner = self.actors.owner.insecure_clone();
        let activity_id = self.next_activity_id();
        let token_mint = self.win_mint;
        self.send_ixs(
            &[create_vesting_ix(
                owner.pubkey(),
                *recipient,
                activity_id,
                token_mint,
                spec,
            )],
            &owner.pubkey(),
            &[&owner],
        )
        .expect("create_activity: create_vesting must succeed");
        activity_id
    }

    /// [`TestEnv::create_activity_for`] with the default recipient -
    /// `actors.recipients[0]`. A separate wrapper because only the binding tests
    /// tell recipients apart, while for the rest the recipient is
    /// irrelevant and would only get in the way of reading the scenario.
    pub fn create_activity(&mut self, spec: &ActivitySpec) -> u64 {
        let recipient = self.actors.recipients[0].pubkey();
        self.create_activity_for(&recipient, spec)
    }

    /// Funds an activity's vault: mints `amount` token units of $1WIN straight into it.
    /// That is exactly how an activity is funded in reality (Arch §2: an atomic
    /// transaction of `mint_to` into every vault → `assert_funded` →
    /// `SetAuthority(None)`), so the fixture is honest - neither the balance nor the
    /// supply is faked.
    pub fn fund_vault(&mut self, activity_id: u64, amount: u64) {
        let mint = self.win_mint;
        self.mint_to(&mint, &vault_pda(activity_id), amount);
    }

    /// Forges an activity vault's balance (a wrapper over [`TestEnv::set_token_amount`]
    /// keyed by `activity_id`).
    ///
    /// Unreachable by honest means: the vault's balance is only ever moved down
    /// by `claim` and `cancel_vesting`/`close_vesting`, and both stay consistent with the
    /// bookkeeping - there is no other way to produce "funded below the reference
    /// amount" after `fund_vault` (the `InsufficientVaultBalance` negatives). The
    /// forge only edits the token account's `amount` field; the mint's supply is left
    /// alone - it does not affect the program's logic, which reads the balance.
    pub fn set_vault_amount(&mut self, activity_id: u64, amount: u64) {
        self.set_token_amount(&vault_pda(activity_id), amount);
    }

    /// Warps the Clock to day `day` of the schedule of an activity whose TGE is at
    /// `tge_ts`: `tge_ts + (day − 1)·DAY_SECS` (Arch §10.1, day 1 = TGE itself). Returns
    /// the timestamp that was set.
    ///
    /// `tge_ts` is passed explicitly rather than read from the config: before V1 the
    /// `initialize` instruction does not exist and there is nothing to read - fail-closed
    /// instead of "substitute zero if there is no config".
    pub fn warp_to_day(&mut self, tge_ts: i64, day: i64) -> i64 {
        assert!(
            day >= 1,
            "schedule day numbering starts at 1 (day 1 = TGE): got {day}"
        );
        let target = (day - 1)
            .checked_mul(DAY_SECS)
            .and_then(|offset| tge_ts.checked_add(offset))
            .unwrap_or_else(|| panic!("day {day} from tge_ts {tge_ts} does not fit into i64"));
        self.warp_to(target);
        target
    }

    /// Sends `claim(activity_id)` on behalf of `actor`: its key goes BOTH into the
    /// `signer` slot AND into the signers - pass it only into the slot and the rejection
    /// would come from the missing signature rather than from the role.
    ///
    /// The fee payer is the environment's `payer` rather than the actor itself:
    /// otherwise the fee would mix into the actor's lamport movement and the assertion
    /// "paid exactly the ATA rent" would become unverifiable.
    ///
    /// `recipient` is a context slot, not a signer: the right to claim belongs to the
    /// recipient OR the operator (Arch §8), so the signer and the addressee differ, and
    /// the I4 negatives put a foreign key into the slot.
    ///
    /// The result is returned as is: the claim is the subject under test, not a fixture,
    /// and the test itself states the expectation (success or a specific code).
    /// result_large_err: we return the native litesvm::types::TransactionResult - the
    /// same argument as for `send_ixs`.
    #[allow(clippy::result_large_err)]
    pub fn claim_as(
        &mut self,
        actor: &Keypair,
        activity_id: u64,
        recipient: Pubkey,
    ) -> TransactionResult {
        let ix = claim_ix(actor.pubkey(), activity_id, recipient, self.win_mint);
        let payer = self.payer.insecure_clone();
        self.send_ixs(&[ix], &payer.pubkey(), &[&payer, actor])
    }

    /// A step of the claim chain: warp to day `day` of the schedule and claim the
    /// activity as its recipient ([`TestEnv::warp_to_day`] + [`TestEnv::claim_as`], where
    /// the addressee is the signer itself).
    ///
    /// The blockhash is rotated right here: two claims of one activity differ only in
    /// time, their transaction bytes coincide, and without a fresh blockhash the second
    /// would be filtered out as `AlreadyProcessed` BEFORE execution - the program would
    /// not run at all.
    ///
    /// Forward only, and only between days: a repeat within the same day is
    /// [`TestEnv::claim_as`] after [`TestEnv::expire_blockhash`]; here it is rejected by
    /// `warp_to`'s panic (rolling time back is forbidden by construction).
    #[allow(clippy::result_large_err)]
    pub fn claim_at_day(
        &mut self,
        tge_ts: i64,
        day: i64,
        activity_id: u64,
        recipient: &Keypair,
    ) -> TransactionResult {
        self.warp_to_day(tge_ts, day);
        self.expire_blockhash();
        self.claim_as(recipient, activity_id, recipient.pubkey())
    }
}
