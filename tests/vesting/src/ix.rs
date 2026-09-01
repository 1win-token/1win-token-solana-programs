//! Vesting PDA helpers, instruction builders and on-chain state readers.
//!
//! The builders assemble an `Instruction` out of the Anchor-generated
//! `vesting::accounts::*` (metadata via `ToAccountMetas`) and
//! `vesting::instruction::*` (data via `InstructionData`) - the test calls the REAL
//! instruction through the same interface a production client uses
//! (tests-bind-to-real-code).
//!
//! Every builder has a variant with explicit slots (`…_with_slots` /
//! `…_with_config`) for substitutions: a foreign `program_data`, a decoy config, a
//! foreign mint, the address of a non-existent activity. The signature flag is
//! cleared by [`without_signer`].

// DUP: to be merged into svm-core (follow-up) - the module's structure was copied
// from tests/sale/src/ix.rs.
//
// The seeds of every PDA were checked against the program's contexts: `[b"config"]`
// and `[b"vault_authority"]` - initialize.rs / tge.rs / roles.rs, `[b"vesting", id]`
// and `[b"vault", id]` - create.rs, recipient.rs, close.rs, funded.rs. A seed
// mismatch would surface immediately: Anchor would answer ConstraintSeeds or
// AccountNotInitialized, because the address would not match the one the program
// derives.

use anchor_lang::{InstructionData, ToAccountMetas};
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;

use crate::env::TestEnv;

// ---------- PDA helpers (the seeds mirror the schemes of Arch §6.1) ----------

/// The `VestingConfig` singleton, seeds `[b"config"]` (Arch §6.1).
pub fn config_pda() -> Pubkey {
    Pubkey::find_program_address(&[b"config"], &vesting::id()).0
}

/// A `Vesting` activity, seeds `[b"vesting", activity_id_le]` (Arch §6.1).
/// `activity_id` is 8 bytes of little-endian u64, exactly as Anchor lays them out
/// from `activity_id.to_le_bytes()`.
pub fn vesting_pda(activity_id: u64) -> Pubkey {
    Pubkey::find_program_address(&[b"vesting", &activity_id.to_le_bytes()], &vesting::id()).0
}

/// An activity's vault (an SPL token account), seeds `[b"vault", activity_id_le]`
/// (Arch §6.1). The binding runs through the seeds plus `token::mint` /
/// `token::authority`.
pub fn vault_pda(activity_id: u64) -> Pubkey {
    Pubkey::find_program_address(&[b"vault", &activity_id.to_le_bytes()], &vesting::id()).0
}

/// An address derived from the same seeds as the canonical PDA but with a
/// NON-canonical bump: it scans down from "canonical − 1" to the first value that
/// yields a valid address. Returns `(address, bump)`.
///
/// For a given set of seeds there are many such addresses (`find_program_address`
/// returns only the highest suitable bump), and the negatives need precisely one of
/// the others: a PDA check must accept the canonical address only.
///
/// It panics if no bump below the canonical one yields a valid address: without an
/// address the test would be checking something other than what it intended
/// (fail-closed).
pub fn non_canonical_pda(seeds: &[&[u8]]) -> (Pubkey, u8) {
    let canonical = Pubkey::find_program_address(seeds, &vesting::id()).1;
    for bump in (0..canonical).rev() {
        let bump_seed = [bump];
        let mut with_bump: Vec<&[u8]> = seeds.to_vec();
        with_bump.push(&bump_seed);
        if let Ok(address) = Pubkey::create_program_address(&with_bump, &vesting::id()) {
            return (address, bump);
        }
    }
    panic!("no bump below the canonical {canonical} yields a valid address")
}

/// The PDA that owns every activity vault, seeds `[b"vault_authority"]` (Arch §6.1).
/// Its bump is cached in the config (`vault_authority_bump`) and used to sign the
/// transfer and close CPIs.
pub fn vault_authority_pda() -> Pubkey {
    Pubkey::find_program_address(&[b"vault_authority"], &vesting::id()).0
}

/// The `#[event_cpi]` authority, seeds `[b"__event_authority"]` (Arch §6.1; the
/// seeds are set by Anchor itself).
pub fn event_authority_pda() -> Pubkey {
    Pubkey::find_program_address(&[b"__event_authority"], &vesting::id()).0
}

/// A recipient's ATA for the $1WIN mint - the payout addressee in `claim`
/// (Arch §9.8). The mint is passed explicitly: it differs per environment
/// (`TestEnv::win_mint`), and the negatives substitute a "foreign" one
/// (`TestEnv::foreign_mint`).
pub fn recipient_ata(recipient: &Pubkey, token_mint: &Pubkey) -> Pubkey {
    spl_associated_token_account_interface::address::get_associated_token_address(
        recipient, token_mint,
    )
}

// ---------- The initialize arguments ----------

/// The set of values `initialize` was (or will be) executed with: the four
/// instruction arguments plus the two accounts whose addresses it pins into the
/// config (the mint and the single withdrawal recipient).
///
/// It is returned by the `TestEnv::init_default` scenario helper - a test need not
/// reassemble its expectations out of the actors and `now()`, it asserts the
/// config's fields against what actually went into the instruction. Negatives build
/// their deviations by editing one field of a copy (the struct is `Copy`).
#[derive(Clone, Copy, Debug)]
pub struct InitParams {
    pub owner: Pubkey,
    pub admin: Pubkey,
    pub operator: Pubkey,
    pub token_mint: Pubkey,
    pub withdrawal_authority: Pubkey,
    pub tge_ts: i64,
}

// ---------- The create_vesting arguments ----------

/// The five `create_vesting` arguments as one value: the reference amount, the
/// shares, the cliff and the label. A test need not repeat them as a positional
/// list, and a negative edits one field of a copy
/// (`ActivitySpec { tge_bps: 10_001, ..spec.clone() }`).
///
/// `Clone` rather than `Copy`: `label` is a `String` (the limit is in BYTES,
/// `MAX_LABEL_LEN`; the negative feeds multi-byte UTF-8, hence a string type
/// rather than a fixed-length `&str`).
#[derive(Clone, Debug)]
pub struct ActivitySpec {
    pub reference_amount: u64,
    pub tge_bps: u16,
    pub monthly_bps: u16,
    pub cliff_months: u16,
    pub label: String,
}

impl ActivitySpec {
    /// A valid schedule for the given amount: the shares and cliff of the "Ecosystem"    /// bucket (§10.5, `vesting_expected::vesting::ECOSYSTEM_*`) - a real bucket
    /// that has both a TGE share and monthly tranches and no cliff. It passes the whole
    /// validation of §9.5, so the fixture stays valid no matter what the test checks.
    ///
    /// The shares come from `vesting-expected` (the single source of reference values)
    /// rather than from the program's constants: otherwise a shift of the value in the
    /// program would shift the fixture too.
    pub fn valid(reference_amount: u64) -> Self {
        Self {
            reference_amount,
            tge_bps: vesting_expected::vesting::ECOSYSTEM_TGE_BPS,
            monthly_bps: vesting_expected::vesting::ECOSYSTEM_MONTHLY_BPS,
            cliff_months: vesting_expected::vesting::ECOSYSTEM_CLIFF_MONTHS,
            label: String::from("ecosystem"),
        }
    }

    /// The five REAL buckets of §10.5 in table order: Marketing (100% at TGE), Ecosystem,    /// Community Rewards (`monthly_bps = 834` - the flooring is topped up by the
    /// terminal tranche), Treasury (a 9-month cliff) and Liquidity.
    ///
    /// The shares and cliffs come from the reference crate; the amounts are the
    /// reference values of the numeric examples V1-V3, Liquidity and V4 (Community uses the
    /// degenerate scale of 1000 units - at which the flooring loss is visible in whole
    /// units).
    ///
    /// A fixture shared by the LiteSVM and surfpool suites: the set of buckets exists in
    /// one place, so both runs exercise the same schedules.
    pub fn sections_10_5() -> Vec<Self> {
        use vesting_expected::vesting as e;
        vec![
            Self {
                reference_amount: e::V1_REFERENCE,
                tge_bps: e::MARKETING_TGE_BPS,
                monthly_bps: e::MARKETING_MONTHLY_BPS,
                cliff_months: e::MARKETING_CLIFF_MONTHS,
                label: String::from("marketing"),
            },
            Self {
                reference_amount: e::V2_REFERENCE,
                tge_bps: e::ECOSYSTEM_TGE_BPS,
                monthly_bps: e::ECOSYSTEM_MONTHLY_BPS,
                cliff_months: e::ECOSYSTEM_CLIFF_MONTHS,
                label: String::from("ecosystem"),
            },
            Self {
                reference_amount: e::V4_REFERENCE,
                tge_bps: e::COMMUNITY_TGE_BPS,
                monthly_bps: e::COMMUNITY_MONTHLY_BPS,
                cliff_months: e::COMMUNITY_CLIFF_MONTHS,
                label: String::from("community"),
            },
            Self {
                reference_amount: e::V3_REFERENCE,
                tge_bps: e::TREASURY_TGE_BPS,
                monthly_bps: e::TREASURY_MONTHLY_BPS,
                cliff_months: e::TREASURY_CLIFF_MONTHS,
                label: String::from("treasury"),
            },
            Self {
                reference_amount: e::LIQUIDITY_REFERENCE,
                tge_bps: e::LIQUIDITY_TGE_BPS,
                monthly_bps: e::LIQUIDITY_MONTHLY_BPS,
                cliff_months: e::LIQUIDITY_CLIFF_MONTHS,
                label: String::from("liquidity"),
            },
        ]
    }
}

// ---------- Instruction builders ----------

/// `initialize`: `authority` signs and pays the config's rent; `program_data` and the
/// PDAs are canonical.
pub fn initialize_ix(authority: Pubkey, p: &InitParams) -> Instruction {
    initialize_ix_with_slots(
        authority,
        TestEnv::programdata_address(),
        config_pda(),
        p.withdrawal_authority,
        vault_authority_pda(),
        p,
    )
}

/// Same as [`initialize_ix`], but four slots are passed explicitly: `program_data`
/// (the negative - the ProgramData of a foreign program over which the signer
/// does hold the upgrade right), `config` and `vault_authority` (the seed negatives)
/// and `withdrawal_authority` (substituting a non-system-owned address: an ATA, a
/// PDA).
///
/// What lands in the config is the address of the SLOT, not of the
/// `p.withdrawal_authority` field: a value differing from the field is a deliberate
/// substitution, and the assertions against `p` will not hold on it.
pub fn initialize_ix_with_slots(
    authority: Pubkey,
    program_data: Pubkey,
    config: Pubkey,
    withdrawal_authority: Pubkey,
    vault_authority: Pubkey,
    p: &InitParams,
) -> Instruction {
    let accounts = vesting::accounts::Initialize {
        authority,
        vesting_program: vesting::id(),
        program_data,
        config,
        withdrawal_authority,
        vault_authority,
        token_mint: p.token_mint,
        system_program: solana_sdk_ids::system_program::id(),
        event_authority: event_authority_pda(),
        program: vesting::id(),
    };
    let data = vesting::instruction::Initialize {
        owner: p.owner,
        admin: p.admin,
        operator: p.operator,
        tge_ts: p.tge_ts,
    };
    Instruction {
        program_id: vesting::id(),
        accounts: accounts.to_account_metas(None),
        data: data.data(),
    }
}

/// `set_tge`: `admin` signs and the config is the canonical PDA.
pub fn set_tge_ix(admin: Pubkey, tge_ts: i64) -> Instruction {
    set_tge_ix_with_config(admin, config_pda(), tge_ts)
}

/// Same as [`set_tge_ix`], but the `config` slot is passed explicitly - for
/// substituting a decoy config (program-owned, with a correct discriminator, at an
/// address not derived from the `[b"config"]` seeds → `ConstraintSeeds`).
pub fn set_tge_ix_with_config(admin: Pubkey, config: Pubkey, tge_ts: i64) -> Instruction {
    let accounts = vesting::accounts::SetTge {
        config,
        admin,
        event_authority: event_authority_pda(),
        program: vesting::id(),
    };
    Instruction {
        program_id: vesting::id(),
        accounts: accounts.to_account_metas(None),
        data: vesting::instruction::SetTge { tge_ts }.data(),
    }
}

/// The metadata of the shared `OwnerUpdate` context (`set_admin`, `set_operator`,
/// `propose_owner`, `cancel_owner_proposal`).
fn owner_update_metas(config: Pubkey, owner: Pubkey) -> Vec<solana_instruction::AccountMeta> {
    vesting::accounts::OwnerUpdate {
        config,
        owner,
        event_authority: event_authority_pda(),
        program: vesting::id(),
    }
    .to_account_metas(None)
}

/// `set_admin`: `owner` signs and the config is the canonical PDA.
pub fn set_admin_ix(owner: Pubkey, admin: Pubkey) -> Instruction {
    set_admin_ix_with_config(owner, config_pda(), admin)
}

/// Same as [`set_admin_ix`], but the `config` slot is passed explicitly.
pub fn set_admin_ix_with_config(owner: Pubkey, config: Pubkey, admin: Pubkey) -> Instruction {
    Instruction {
        program_id: vesting::id(),
        accounts: owner_update_metas(config, owner),
        data: vesting::instruction::SetAdmin { admin }.data(),
    }
}

/// `set_operator`: `owner` signs and the config is the canonical PDA.
pub fn set_operator_ix(owner: Pubkey, operator: Pubkey) -> Instruction {
    set_operator_ix_with_config(owner, config_pda(), operator)
}

/// Same as [`set_operator_ix`], but the `config` slot is passed explicitly.
pub fn set_operator_ix_with_config(owner: Pubkey, config: Pubkey, operator: Pubkey) -> Instruction {
    Instruction {
        program_id: vesting::id(),
        accounts: owner_update_metas(config, owner),
        data: vesting::instruction::SetOperator { operator }.data(),
    }
}

/// `propose_owner`: the sitting `owner` signs and the config is the canonical PDA.
pub fn propose_owner_ix(owner: Pubkey, new_owner: Pubkey) -> Instruction {
    propose_owner_ix_with_config(owner, config_pda(), new_owner)
}

/// Same as [`propose_owner_ix`], but the `config` slot is passed explicitly.
pub fn propose_owner_ix_with_config(
    owner: Pubkey,
    config: Pubkey,
    new_owner: Pubkey,
) -> Instruction {
    Instruction {
        program_id: vesting::id(),
        accounts: owner_update_metas(config, owner),
        data: vesting::instruction::ProposeOwner { new_owner }.data(),
    }
}

/// `cancel_owner_proposal`: the sitting `owner` signs and the config is the
/// canonical PDA.
pub fn cancel_owner_proposal_ix(owner: Pubkey) -> Instruction {
    cancel_owner_proposal_ix_with_config(owner, config_pda())
}

/// Same as [`cancel_owner_proposal_ix`], but the `config` slot is passed explicitly.
pub fn cancel_owner_proposal_ix_with_config(owner: Pubkey, config: Pubkey) -> Instruction {
    Instruction {
        program_id: vesting::id(),
        accounts: owner_update_metas(config, owner),
        data: vesting::instruction::CancelOwnerProposal {}.data(),
    }
}

/// `accept_owner`: the CANDIDATE signs (the `new_owner` slot), not the sitting
/// owner; the config is the canonical PDA.
pub fn accept_owner_ix(new_owner: Pubkey) -> Instruction {
    accept_owner_ix_with_config(new_owner, config_pda())
}

/// Same as [`accept_owner_ix`], but the `config` slot is passed explicitly.
pub fn accept_owner_ix_with_config(new_owner: Pubkey, config: Pubkey) -> Instruction {
    let accounts = vesting::accounts::AcceptOwner {
        config,
        new_owner,
        event_authority: event_authority_pda(),
        program: vesting::id(),
    };
    Instruction {
        program_id: vesting::id(),
        accounts: accounts.to_account_metas(None),
        data: vesting::instruction::AcceptOwner {}.data(),
    }
}

/// `set_withdrawal_authority`: `admin` signs, and the config and `vault_authority`
/// are the canonical PDAs. The new withdrawal target is passed as an account rather
/// than an argument - as in `initialize` - which is why it is explicit even in the
/// canonical builder.
pub fn set_withdrawal_authority_ix(admin: Pubkey, withdrawal_authority: Pubkey) -> Instruction {
    set_withdrawal_authority_ix_with_slots(
        admin,
        config_pda(),
        withdrawal_authority,
        vault_authority_pda(),
    )
}

/// Same as [`set_withdrawal_authority_ix`], but the `config` and `vault_authority`
/// slots are passed explicitly - for a decoy config and for substituting
/// `vault_authority` (the "the withdrawal target != the vault authority" check).
pub fn set_withdrawal_authority_ix_with_slots(
    admin: Pubkey,
    config: Pubkey,
    withdrawal_authority: Pubkey,
    vault_authority: Pubkey,
) -> Instruction {
    let accounts = vesting::accounts::SetWithdrawalAuthority {
        config,
        withdrawal_authority,
        vault_authority,
        admin,
        event_authority: event_authority_pda(),
        program: vesting::id(),
    };
    Instruction {
        program_id: vesting::id(),
        accounts: accounts.to_account_metas(None),
        data: vesting::instruction::SetWithdrawalAuthority {}.data(),
    }
}

/// `create_vesting`: `owner` signs and pays the rent of both accounts being created;
/// the PDAs are canonical.
///
/// `next_id` is the identifier the instruction will assign to the activity (= the
/// current `config.activity_count`, read by [`TestEnv::next_activity_id`]). The
/// builder needs it because the `vesting` and `vault` addresses are derived from it,
/// while the builder itself never reads chain state - it is a pure function, like
/// every other builder. Pass the wrong `next_id` and Anchor answers ConstraintSeeds:
/// the address will not match the one the program derives from
/// `config.activity_count`.
pub fn create_vesting_ix(
    owner: Pubkey,
    recipient: Pubkey,
    next_id: u64,
    token_mint: Pubkey,
    spec: &ActivitySpec,
) -> Instruction {
    create_vesting_ix_with_slots(
        owner,
        config_pda(),
        vesting_pda(next_id),
        vault_pda(next_id),
        recipient,
        vault_authority_pda(),
        token_mint,
        spec,
    )
}

/// Same as [`create_vesting_ix`], but every derived slot is passed explicitly - for
/// the negatives: a decoy `config`, a foreign `token_mint`
/// (`ConstraintAddress`), a token account or `Pubkey::default()` in the `recipient`
/// slot, substituting `vault_authority`, and `vesting` / `vault` addresses
/// that do not come from the current counter.
#[allow(clippy::too_many_arguments)]
pub fn create_vesting_ix_with_slots(
    owner: Pubkey,
    config: Pubkey,
    vesting: Pubkey,
    vault: Pubkey,
    recipient: Pubkey,
    vault_authority: Pubkey,
    token_mint: Pubkey,
    spec: &ActivitySpec,
) -> Instruction {
    let accounts = vesting::accounts::CreateVesting {
        config,
        owner,
        vesting,
        vault,
        recipient,
        vault_authority,
        token_mint,
        token_program: spl_token_interface::id(),
        system_program: solana_sdk_ids::system_program::id(),
        rent: solana_sdk_ids::sysvar::rent::id(),
        event_authority: event_authority_pda(),
        program: vesting::id(),
    };
    let data = vesting::instruction::CreateVesting {
        reference_amount: spec.reference_amount,
        tge_bps: spec.tge_bps,
        monthly_bps: spec.monthly_bps,
        cliff_months: spec.cliff_months,
        label: spec.label.clone(),
    };
    Instruction {
        program_id: vesting::id(),
        accounts: accounts.to_account_metas(None),
        data: data.data(),
    }
}

/// `set_recipient(activity_id)`: `admin` signs, and the config and the activity are
/// the canonical PDAs.
pub fn set_recipient_ix(admin: Pubkey, activity_id: u64, new_recipient: Pubkey) -> Instruction {
    set_recipient_ix_with_slots(
        admin,
        config_pda(),
        vesting_pda(activity_id),
        new_recipient,
        vault_authority_pda(),
        activity_id,
    )
}

/// Same as [`set_recipient_ix`], but the `config`, `vesting` and `vault_authority`
/// slots are passed explicitly - for a decoy config, for a mismatch between the
/// `activity_id` argument and the activity's address (the argument goes into the
/// context's seeds, so a mismatch must fail with `ConstraintSeeds` rather than
/// silently edit someone else's record), and for substituting `vault_authority` (the
/// "recipient != vault_authority" gate).
///
/// The "non-existent activity" negative (`AccountNotInitialized`, 3012) is built with
/// the canonical builder using an id nobody ever created: the address derives, but
/// no account lives at it.
pub fn set_recipient_ix_with_slots(
    admin: Pubkey,
    config: Pubkey,
    vesting: Pubkey,
    new_recipient: Pubkey,
    vault_authority: Pubkey,
    activity_id: u64,
) -> Instruction {
    let accounts = vesting::accounts::SetRecipient {
        config,
        vesting,
        new_recipient,
        vault_authority,
        admin,
        event_authority: event_authority_pda(),
        program: vesting::id(),
    };
    Instruction {
        program_id: vesting::id(),
        accounts: accounts.to_account_metas(None),
        data: vesting::instruction::SetRecipient { activity_id }.data(),
    }
}

/// `cancel_vesting(activity_id)`: `owner` signs; the activity, vault and
/// `vault_authority` PDAs are canonical.
///
/// `beneficiary_token` and `rent_collector` are passed explicitly even in the
/// canonical builder: they are the addressees of the remainder and the rent, not
/// derived slots. They also carry the `beneficiary_token == vault_pda(id)` and the
/// foreign-mint (`rent_collector` as a token account) negatives - no slot
/// substitution is needed
/// for those.
pub fn cancel_vesting_ix(
    owner: Pubkey,
    activity_id: u64,
    beneficiary_token: Pubkey,
    rent_collector: Pubkey,
    token_mint: Pubkey,
) -> Instruction {
    cancel_vesting_ix_with_slots(
        owner,
        config_pda(),
        vesting_pda(activity_id),
        vault_pda(activity_id),
        beneficiary_token,
        rent_collector,
        vault_authority_pda(),
        token_mint,
        activity_id,
    )
}

/// Same as [`cancel_vesting_ix`], but every derived slot is passed explicitly - for a
/// decoy config, a foreign `token_mint` and substituting `vault` /
/// `vault_authority`.
#[allow(clippy::too_many_arguments)]
pub fn cancel_vesting_ix_with_slots(
    owner: Pubkey,
    config: Pubkey,
    vesting: Pubkey,
    vault: Pubkey,
    beneficiary_token: Pubkey,
    rent_collector: Pubkey,
    vault_authority: Pubkey,
    token_mint: Pubkey,
    activity_id: u64,
) -> Instruction {
    Instruction {
        program_id: vesting::id(),
        accounts: close_activity_metas(
            config,
            owner,
            vesting,
            vault,
            beneficiary_token,
            rent_collector,
            vault_authority,
            token_mint,
        ),
        data: vesting::instruction::CancelVesting { activity_id }.data(),
    }
}

/// The metadata of the shared `CloseActivity` context (`cancel_vesting` +
/// `close_vesting`, Arch §9.7): the instructions take identical slot sets, so the
/// builders share this half.
#[allow(clippy::too_many_arguments)]
fn close_activity_metas(
    config: Pubkey,
    owner: Pubkey,
    vesting: Pubkey,
    vault: Pubkey,
    beneficiary_token: Pubkey,
    rent_collector: Pubkey,
    vault_authority: Pubkey,
    token_mint: Pubkey,
) -> Vec<solana_instruction::AccountMeta> {
    vesting::accounts::CloseActivity {
        config,
        owner,
        vesting,
        vault,
        beneficiary_token,
        rent_collector,
        vault_authority,
        token_mint,
        token_program: spl_token_interface::id(),
        event_authority: event_authority_pda(),
        program: vesting::id(),
    }
    .to_account_metas(None)
}

/// `close_vesting(activity_id)`: `owner` signs; the slots are the same as
/// [`cancel_vesting_ix`]'s (the context is shared) and only the gate in the body
/// differs - `claimed >= reference_amount` instead of `now < tge_ts`.
pub fn close_vesting_ix(
    owner: Pubkey,
    activity_id: u64,
    beneficiary_token: Pubkey,
    rent_collector: Pubkey,
    token_mint: Pubkey,
) -> Instruction {
    close_vesting_ix_with_slots(
        owner,
        config_pda(),
        vesting_pda(activity_id),
        vault_pda(activity_id),
        beneficiary_token,
        rent_collector,
        vault_authority_pda(),
        token_mint,
        activity_id,
    )
}

/// Same as [`close_vesting_ix`], but every derived slot is passed explicitly.
#[allow(clippy::too_many_arguments)]
pub fn close_vesting_ix_with_slots(
    owner: Pubkey,
    config: Pubkey,
    vesting: Pubkey,
    vault: Pubkey,
    beneficiary_token: Pubkey,
    rent_collector: Pubkey,
    vault_authority: Pubkey,
    token_mint: Pubkey,
    activity_id: u64,
) -> Instruction {
    Instruction {
        program_id: vesting::id(),
        accounts: close_activity_metas(
            config,
            owner,
            vesting,
            vault,
            beneficiary_token,
            rent_collector,
            vault_authority,
            token_mint,
        ),
        data: vesting::instruction::CloseVesting { activity_id }.data(),
    }
}

/// `claim(activity_id)`: `signer` is either the activity's recipient or the operator
/// (Arch §8); the payout addressee is the `recipient` ATA under `token_mint`,
/// derived by the builder rather than supplied by the caller.
///
/// `recipient` is passed explicitly even though the program pins the slot with the
/// `vesting.recipient` field: the builder never reads chain state (a pure function,
/// like the rest), and the I4 negatives put a foreign key into that slot.
pub fn claim_ix(
    signer: Pubkey,
    activity_id: u64,
    recipient: Pubkey,
    token_mint: Pubkey,
) -> Instruction {
    claim_ix_with_slots(
        signer,
        config_pda(),
        vesting_pda(activity_id),
        vault_pda(activity_id),
        recipient,
        recipient_ata(&recipient, &token_mint),
        vault_authority_pda(),
        token_mint,
        spl_token_interface::id(),
        activity_id,
    )
}

/// Same as [`claim_ix`], but every derived slot is passed explicitly - for the
/// negatives: a decoy config, the vault or the record of ANOTHER activity (I2),
/// a foreign ATA in the addressee slot (I4), a foreign `token_mint`, and substituting
/// `token_program` and `vault_authority`.
///
/// Two slots are deliberately NOT exposed as parameters -
/// `associated_token_program` and `system_program`: T3.x has no substitutions for
/// them, and their failure class is the same one already covered by substituting
/// `token_program` (`Program<'info, T>` checks the address itself,
/// `InvalidProgramId`). They will be needed by the full substitution matrix
/// - and are to be added then, together with their first consumer.
#[allow(clippy::too_many_arguments)]
pub fn claim_ix_with_slots(
    signer: Pubkey,
    config: Pubkey,
    vesting: Pubkey,
    vault: Pubkey,
    recipient: Pubkey,
    recipient_ata: Pubkey,
    vault_authority: Pubkey,
    token_mint: Pubkey,
    token_program: Pubkey,
    activity_id: u64,
) -> Instruction {
    let accounts = vesting::accounts::Claim {
        config,
        vesting,
        vault,
        recipient,
        recipient_ata,
        vault_authority,
        token_mint,
        signer,
        token_program,
        associated_token_program: spl_associated_token_account_interface::program::ID,
        system_program: solana_sdk_ids::system_program::id(),
        event_authority: event_authority_pda(),
        program: vesting::id(),
    };
    Instruction {
        program_id: vesting::id(),
        accounts: accounts.to_account_metas(None),
        data: vesting::instruction::Claim { activity_id }.data(),
    }
}

/// `assert_funded(activity_id)`: every slot is canonical.
///
/// The instruction has no signer at all (Arch §9.9), so the builder takes no signer
/// parameter: the transaction is signed only by its fee payer, and in a test that is
/// any key - which is exactly how permissionless access is verified.
pub fn assert_funded_ix(activity_id: u64, token_mint: Pubkey) -> Instruction {
    assert_funded_ix_with_slots(
        config_pda(),
        vesting_pda(activity_id),
        vault_pda(activity_id),
        vault_authority_pda(),
        token_mint,
        activity_id,
    )
}

/// Same as [`assert_funded_ix`], but every slot is passed explicitly - for a decoy
/// config, a foreign `token_mint` and substituting `vault` / `vault_authority`.
pub fn assert_funded_ix_with_slots(
    config: Pubkey,
    vesting: Pubkey,
    vault: Pubkey,
    vault_authority: Pubkey,
    token_mint: Pubkey,
    activity_id: u64,
) -> Instruction {
    let accounts = vesting::accounts::AssertFunded {
        config,
        vesting,
        vault,
        vault_authority,
        token_mint,
    };
    Instruction {
        program_id: vesting::id(),
        accounts: accounts.to_account_metas(None),
        data: vesting::instruction::AssertFunded { activity_id }.data(),
    }
}

/// Clears the `is_signer` flag on the metas of account `key` in an already assembled
/// instruction: the key that was passed is correct, but there is no signature. The
/// expected answer is `AccountNotSigner` (3010), raised during account resolution
/// before the instruction's body.
///
/// A separate combinator rather than a parameter on every builder:
/// `to_account_metas` derives the flag from the `Signer<'info>` type, and it must be
/// cleared in exactly one place and identically for every instruction.
///
/// The flag is cleared on ALL of the key's metas: one key may occupy two slots at
/// once (in `claim` the recipient fills both `recipient` and `signer`), and a
/// transaction message ORs together the metas of a single address - clearing the flag
/// on just one of them would leave the test with a signed transaction, checking the
/// wrong thing.
///
/// It panics if the account is absent from the instruction or if none of its metas is
/// a signer: in either case the test would be checking something other than what it
/// intended (fail-closed).
pub fn without_signer(mut ix: Instruction, key: &Pubkey) -> Instruction {
    let mut found = false;
    let mut was_signer = false;
    for meta in ix.accounts.iter_mut().filter(|m| m.pubkey == *key) {
        found = true;
        was_signer |= meta.is_signer;
        meta.is_signer = false;
    }
    assert!(
        found,
        "account {key} is absent from the instruction - there is no signature to clear"
    );
    assert!(
        was_signer,
        "account {key} is not a signer anyway - the test is not checking a missing signature"
    );
    ix
}

// ---------- The funding transaction (Arch §2) ----------

/// A `mint_to` of a bucket's emission straight into an activity's vault - the first
/// link of the funding transaction. The vault is addressed by its own PDA
/// (`vault_pda`) rather than an ATA: it has no wallet owner, and its authority is
/// `vault_authority`.
pub fn mint_to_vault_ix(
    mint: &Pubkey,
    mint_authority: &Pubkey,
    activity_id: u64,
    amount: u64,
) -> Instruction {
    spl_token_interface::instruction::mint_to(
        &spl_token_interface::id(),
        mint,
        &vault_pda(activity_id),
        mint_authority,
        &[],
        amount,
    )
    .expect("the mint_to instruction into an activity vault")
}

/// `SetAuthority(MintTokens, None)` - the closing link of the funding transaction:
/// after it no further $1WIN can be issued, neither for the sale nor for the vesting
/// (T-6).
pub fn revoke_mint_authority_ix(mint: &Pubkey, mint_authority: &Pubkey) -> Instruction {
    spl_token_interface::instruction::set_authority(
        &spl_token_interface::id(),
        mint,
        None,
        spl_token_interface::instruction::AuthorityType::MintTokens,
        mint_authority,
        &[],
    )
    .expect("the SetAuthority(MintTokens, None) instruction")
}

/// The instructions of ONE atomic funding transaction (Arch §2) in the order the spec
/// requires: `mint_to` into every vault → `assert_funded` for every activity →
/// `SetAuthority(None)` on the mint.
///
/// `funding` holds `(activity_id, amount)` pairs: how much is minted into each
/// activity's vault. `assert_funded` is placed on EVERY activity of the list, so an
/// under-mint of any one of them fails the whole transaction - before the mint
/// authority is revoked.
///
/// A function shared by the LiteSVM and surfpool runs: the transaction's shape (the
/// set and order of instructions) exists in one place, and measuring its size against
/// the 1232-byte limit applies to exactly the bundle the LiteSVM suite executes.
pub fn funding_tx_ixs(
    mint: &Pubkey,
    mint_authority: &Pubkey,
    funding: &[(u64, u64)],
) -> Vec<Instruction> {
    assert!(
        !funding.is_empty(),
        "a funding transaction with no activities would revoke the mint authority blindly"
    );
    let mut ixs = Vec::with_capacity(funding.len() * 2 + 1);
    for (activity_id, amount) in funding {
        ixs.push(mint_to_vault_ix(
            mint,
            mint_authority,
            *activity_id,
            *amount,
        ));
    }
    for (activity_id, _) in funding {
        ixs.push(assert_funded_ix(*activity_id, *mint));
    }
    ixs.push(revoke_mint_authority_ix(mint, mint_authority));
    ixs
}

// ---------- State readers ----------

impl TestEnv {
    /// Deserializes the `VestingConfig` singleton from the chain (stripping the 8-byte
    /// discriminator). Panics if the account is absent - a missing singleton must never
    /// be read silently as "an empty config" (fail-closed).
    pub fn vesting_config(&self) -> vesting::state::VestingConfig {
        let acc = self
            .svm
            .get_account(&config_pda())
            .expect("VestingConfig does not exist");
        let mut data: &[u8] = &acc.data[8..];
        anchor_lang::AnchorDeserialize::deserialize(&mut data).expect("VestingConfig deserializes")
    }

    /// Whether the `VestingConfig` singleton exists on the chain (the `initialize`
    /// negatives: a rejection must not leave a config behind).
    pub fn vesting_config_exists(&self) -> bool {
        self.svm.get_account(&config_pda()).is_some()
    }

    /// Writes a `VestingConfig` with arbitrary fields at address `address`: owned by the
    /// Vesting program, with a genuine discriminator (`try_serialize`) and the same size
    /// as the account `initialize` produces.
    ///
    /// Such state is unreachable by honest means in the two scenarios the forge
    /// exists for: a config with a valid discriminator at a NON-canonical address
    /// (`initialize` only ever creates the PDA under the `[b"config"]` seeds, so a decoy
    /// config cannot be obtained anywhere) and a config with a field outside validation -
    /// `tge_ts = 0`, for instance, which neither `initialize` nor `set_tge` will write
    /// (`InvalidTge`) while `vested()` still has to be checked on it.
    pub fn forge_config(&mut self, address: &Pubkey, config: &vesting::state::VestingConfig) {
        let mut data = Vec::new();
        anchor_lang::AccountSerialize::try_serialize(config, &mut data)
            .expect("VestingConfig serializes");
        self.forge_program_account(address, data);
    }

    /// Copies a `Vesting` activity record to address `target`, placing the value `bump`
    /// into the copy's LAST byte.
    ///
    /// Such state is unreachable by honest means: a record with a valid
    /// discriminator at a NON-canonical address cannot be obtained anywhere - the program
    /// only ever creates it at the canonical PDA. Needed by the address-canonicity
    /// negatives ([`non_canonical_pda`]), which pass the `bump` of the decoy address.
    ///
    /// The last byte is where the cached bump would live if the record had one: with the
    /// decoy address's bump the copy is self-consistent (address ↔ stored bump), and only
    /// recomputing the canonical bump can reject it. Today the record carries no such
    /// cache, and that byte is the high byte of `cliff_months`; the corrupted value does
    /// not affect the negative's outcome, because the address is checked during context
    /// resolution, before any field is read in the body.
    ///
    /// The helper is unsuitable for the config: its last byte is `vault_authority_bump`,
    /// which the separate `vault_authority` slot is checked against, and overwriting it
    /// would break that check instead of checking the config's address. A copy of the
    /// config is substituted as is, through [`TestEnv::forge_config`].
    pub fn forge_vesting_copy(&mut self, activity_id: u64, target: &Pubkey, bump: u8) {
        let mut data = self
            .svm
            .get_account(&vesting_pda(activity_id))
            .expect("the activity record exists")
            .data;
        let last = data.last_mut().expect("the record has data");
        *last = bump;
        self.forge_program_account(target, data);
    }

    /// Deserializes the `Vesting` activity at `activity_id`. Panics if the account is
    /// absent.
    pub fn vesting(&self, activity_id: u64) -> vesting::state::Vesting {
        let acc = self
            .svm
            .get_account(&vesting_pda(activity_id))
            .expect("the activity does not exist");
        let mut data: &[u8] = &acc.data[8..];
        anchor_lang::AnchorDeserialize::deserialize(&mut data).expect("Vesting deserializes")
    }

    /// Whether a `Vesting` activity exists (asserting the fact of creation or closing -
    /// closing is a replay guard, and a repeat will not find the account).
    pub fn vesting_exists(&self, activity_id: u64) -> bool {
        self.svm.get_account(&vesting_pda(activity_id)).is_some()
    }

    /// Whether an activity's vault exists (asserting creation at `create_vesting` and
    /// closing at `cancel_vesting` / `close_vesting`).
    pub fn vault_exists(&self, activity_id: u64) -> bool {
        self.svm.get_account(&vault_pda(activity_id)).is_some()
    }

    /// An activity vault's balance in token units. Panics if the vault is absent: a
    /// closed vault must never be read silently as "zero" (fail-closed) - the fact of
    /// closing is checked by [`TestEnv::vault_exists`].
    pub fn vault_balance(&self, activity_id: u64) -> u64 {
        self.token_balance(&vault_pda(activity_id))
    }

    /// Whether an account exists at an address. Needed by the "no side effect was left
    /// behind" assertions: [`TestEnv::token_balance`] panics on a non-existent account
    /// (fail-closed), so absence is queried separately.
    pub fn account_exists(&self, address: &Pubkey) -> bool {
        self.svm.get_account(address).is_some()
    }

    /// Whether a recipient's ATA under the environment's mint exists. The load-bearing
    /// assertion that `init_if_needed` creates the account in `try_accounts`, that is
    /// BEFORE the body's checks, and "there is no partial payout" rests solely on the
    /// whole transaction reverting - the proof being precisely the absence of the account
    /// after a rejection.
    pub fn recipient_ata_exists(&self, recipient: &Pubkey) -> bool {
        self.account_exists(&recipient_ata(recipient, &self.win_mint))
    }

    /// A recipient's ATA balance under the environment's mint; 0 if the account does not
    /// exist yet.
    ///
    /// The zero on a non-existent account is not a "silent default" here: the value is
    /// always taken together with [`TestEnv::recipient_ata_exists`] (both go into the
    /// state snapshot before and after a rejection), and the distinction between "no
    /// account" and "an empty account" is carried by the existence assertion itself.
    pub fn recipient_ata_balance(&self, recipient: &Pubkey) -> u64 {
        let ata = recipient_ata(recipient, &self.win_mint);
        if self.account_exists(&ata) {
            self.token_balance(&ata)
        } else {
            0
        }
    }

    /// The identifier the NEXT `create_vesting` will assign (= `config.activity_count`).
    /// Needed by the [`create_vesting_ix`] builder: the addresses of both accounts being
    /// created derive from the counter, and a test should not keep its own copy of it.
    pub fn next_activity_id(&self) -> u64 {
        self.vesting_config().activity_count
    }

    /// Forges `config.activity_count` (the config's other fields are preserved).
    ///
    /// Unreachable by honest means: the counter only grows on a successful
    /// `create_vesting`, by one per call, and never rolls back - driving it to `u64::MAX`
    /// (the `MathOverflow` negative) would mean executing 2⁶⁴ transactions.
    pub fn forge_activity_count(&mut self, activity_count: u64) {
        let mut config = self.vesting_config();
        config.activity_count = activity_count;
        self.forge_config(&config_pda(), &config);
    }

    /// Forges `config.tge_ts` (the config's other fields are preserved).
    ///
    /// Precisely the values the forge exists for are unreachable by honest means:
    /// `initialize` and `set_tge` accept only `tge_ts > now` (Arch §4.3), so neither zero
    /// ("TGE is not set") nor a negative instant ever reaches the config. Without this
    /// forge the `tge_ts > 0` guard in `claim` would stay uncovered. Putting
    /// a real date back through the same helper gives the positive case in the same
    /// environment.
    pub fn forge_tge_ts(&mut self, tge_ts: i64) {
        let mut config = self.vesting_config();
        config.tge_ts = tge_ts;
        self.forge_config(&config_pda(), &config);
    }

    /// Forges an activity's `vesting.claimed` (its other fields are preserved).
    ///
    /// Only the values of `vested()` at the schedule's landmark days are reachable
    /// honestly (the sole writer of `claimed` is `claim`), and each costs a chain of
    /// claims with time warps. The forge sets an ARBITRARY intermediate value at once
    /// Needed by the funding test: `assert_funded` checks the coverage of the UNRELEASED
    /// remainder (`vault.amount + claimed >= reference_amount`), and without a non-zero
    /// `claimed` the second half of that invariant is never checked at all. The check
    /// after REAL claims, with no forge, stands separately.
    ///
    /// A second consumer is the `claimed == 0` safeguard in `cancel_vesting`: the
    /// combination "before TGE and `claimed > 0`" cannot be built by anything at all,
    /// because `claim` is gated on `now >= tge_ts` and that gate sits in another
    /// instruction.
    pub fn forge_claimed(&mut self, activity_id: u64, claimed: u64) {
        let mut vesting = self.vesting(activity_id);
        vesting.claimed = claimed;
        let mut data = Vec::new();
        anchor_lang::AccountSerialize::try_serialize(&vesting, &mut data)
            .expect("Vesting serializes");
        self.forge_program_account(&vesting_pda(activity_id), data);
    }
}
