//! Test harness for the Burn program: typed instruction builders, PDA helpers and an
//! extension of [`TestEnv`] with state readers.
//!
//! A mirror of `ix.rs` from `sale-test-common` for the Burn program: the builders
//! assemble an `Instruction` out of the Anchor-generated `burn::accounts::*`
//! (metadata via `ToAccountMetas`) and `burn::instruction::*` (data via
//! `InstructionData`) - the test calls the REAL handler through the same interface a
//! production client uses (tests-bind-to-real-code). The PDA helpers copy the seeds
//! from the program's contexts (`initialize.rs` / `burn.rs`). The state readers are
//! attached to [`TestEnv`] through the [`BurnTestEnv`] extension trait (the shared
//! environment engine lives in `sale-test-common`, and a foreign type cannot be
//! given an inherent impl). The raw SPL Token builders at the bottom drive the
//! token layer directly - funding the vault the way a client does and the "around
//! the program" negatives.

use anchor_lang::{InstructionData, ToAccountMetas};
use sale_test_common::TestEnv;
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

// ---------- init_default_burn constants ----------

/// The default daily limit: 500 tokens × 10^8 units = 50_000_000_000.
pub const DEFAULT_DAILY_LIMIT: u64 = 50_000_000_000;
/// The default weekly limit: 2,000 tokens × 10^8 units = 200_000_000_000.
pub const DEFAULT_WEEKLY_LIMIT: u64 = 200_000_000_000;

// ---------- PDA helpers (the seeds are copied from the burn contexts) ----------

/// The `BurnState` singleton, seeds `[b"burn_state"]` (initialize.rs).
pub fn burn_state_pda() -> Pubkey {
    Pubkey::find_program_address(&[b"burn_state"], &burn::id()).0
}

/// The `BurnCounters` singleton accumulator, seeds `[b"burn_counters"]` (created in
/// initialize.rs, mutated in burn.rs).
pub fn burn_counters_pda() -> Pubkey {
    Pubkey::find_program_address(&[b"burn_counters"], &burn::id()).0
}

/// The Burn program's `#[event_cpi]` authority, seeds `[b"__event_authority"]`.
pub fn burn_event_authority_pda() -> Pubkey {
    Pubkey::find_program_address(&[b"__event_authority"], &burn::id()).0
}

/// The vault, seeds `[b"vault"]` (created in initialize.rs, debited in burn.rs).
pub fn vault_pda() -> Pubkey {
    Pubkey::find_program_address(&[b"vault"], &burn::id()).0
}

/// The data-less PDA that owns the vault and signs the burn CPI, seeds
/// `[b"vault_authority"]`.
pub fn vault_authority_pda() -> Pubkey {
    Pubkey::find_program_address(&[b"vault_authority"], &burn::id()).0
}

// ---------- Instruction builders ----------

/// `initialize`: `authority` (the upgrade authority) signs and pays the rent of both
/// singletons (`burn_state` + `burn_counters`) and of the vault; `token_mint` is the
/// $1WIN mint (the tests swap it for the decimals negative).
#[allow(clippy::too_many_arguments)]
pub fn burn_initialize_ix(
    authority: Pubkey,
    token_mint: Pubkey,
    owner: Pubkey,
    admin: Pubkey,
    operator: Pubkey,
    daily_limit: u64,
    weekly_limit: u64,
) -> Instruction {
    burn_initialize_ix_with_program_data(
        authority,
        TestEnv::burn_programdata_address(),
        token_mint,
        owner,
        admin,
        operator,
        daily_limit,
        weekly_limit,
    )
}

/// Same as [`burn_initialize_ix`], but `program_data` is passed explicitly - for the
/// "foreign ProgramData" negative (the `burn_program.programdata_address()`
/// constraint).
#[allow(clippy::too_many_arguments)]
pub fn burn_initialize_ix_with_program_data(
    authority: Pubkey,
    program_data: Pubkey,
    token_mint: Pubkey,
    owner: Pubkey,
    admin: Pubkey,
    operator: Pubkey,
    daily_limit: u64,
    weekly_limit: u64,
) -> Instruction {
    let accounts = burn::accounts::Initialize {
        authority,
        burn_program: burn::id(),
        program_data,
        burn_state: burn_state_pda(),
        burn_counters: burn_counters_pda(),
        token_mint,
        vault: vault_pda(),
        vault_authority: vault_authority_pda(),
        token_program: spl_token_interface::id(),
        system_program: solana_sdk_ids::system_program::id(),
        event_authority: burn_event_authority_pda(),
        program: burn::id(),
    };
    let data = burn::instruction::Initialize {
        owner,
        admin,
        operator,
        daily_limit,
        weekly_limit,
    };
    Instruction {
        program_id: burn::id(),
        accounts: accounts.to_account_metas(None),
        data: data.data(),
    }
}

/// `set_limits`: `admin` signs (the `AdminUpdate` context).
pub fn burn_set_limits_ix(admin: Pubkey, daily_limit: u64, weekly_limit: u64) -> Instruction {
    let accounts = burn::accounts::AdminUpdate {
        burn_state: burn_state_pda(),
        admin,
        event_authority: burn_event_authority_pda(),
        program: burn::id(),
    };
    Instruction {
        program_id: burn::id(),
        accounts: accounts.to_account_metas(None),
        data: burn::instruction::SetLimits {
            daily_limit,
            weekly_limit,
        }
        .data(),
    }
}

/// The metadata of the `OwnerUpdate` context (propose_owner / cancel_owner_proposal /
/// set_admin / set_operator).
fn burn_owner_ix(owner: Pubkey, data: Vec<u8>) -> Instruction {
    let accounts = burn::accounts::OwnerUpdate {
        burn_state: burn_state_pda(),
        owner,
        event_authority: burn_event_authority_pda(),
        program: burn::id(),
    };
    Instruction {
        program_id: burn::id(),
        accounts: accounts.to_account_metas(None),
        data,
    }
}

/// `propose_owner`: the sitting `owner` signs and `new` is the candidate.
pub fn burn_propose_owner_ix(owner: Pubkey, new: Pubkey) -> Instruction {
    burn_owner_ix(owner, burn::instruction::ProposeOwner { new }.data())
}

/// `cancel_owner_proposal`: the sitting `owner` withdraws the nominated candidate.
pub fn burn_cancel_owner_proposal_ix(owner: Pubkey) -> Instruction {
    burn_owner_ix(owner, burn::instruction::CancelOwnerProposal {}.data())
}

/// `accept_owner`: the candidate signs; the sitting owner takes no part in the
/// context.
pub fn burn_accept_owner_ix(new_owner: Pubkey) -> Instruction {
    let accounts = burn::accounts::AcceptOwner {
        burn_state: burn_state_pda(),
        new_owner,
        event_authority: burn_event_authority_pda(),
        program: burn::id(),
    };
    Instruction {
        program_id: burn::id(),
        accounts: accounts.to_account_metas(None),
        data: burn::instruction::AcceptOwner {}.data(),
    }
}

/// `set_admin`: `owner` signs.
pub fn burn_set_admin_ix(owner: Pubkey, new: Pubkey) -> Instruction {
    burn_owner_ix(owner, burn::instruction::SetAdmin { new }.data())
}

/// `set_operator`: `owner` signs.
pub fn burn_set_operator_ix(owner: Pubkey, new: Pubkey) -> Instruction {
    burn_owner_ix(owner, burn::instruction::SetOperator { new }.data())
}

/// `burn`: the canonical set - the `burn_counters` accumulator and the
/// `vault_authority` PDA at their own seeds; the program takes the period indices
/// ONLY from Clock (there are no index arguments). `operator` signs (and pays no
/// rent - nothing is created); `vault` is [`vault_pda`], or a decoy in the
/// negatives.
pub fn burn_ix(operator: Pubkey, vault: Pubkey, token_mint: Pubkey, amount: u64) -> Instruction {
    burn_ix_raw(
        operator,
        burn_counters_pda(),
        vault,
        vault_authority_pda(),
        token_mint,
        amount,
    )
}

/// Same as [`burn_ix`], but `burn_counters` is passed explicitly - for the negatives
/// that swap the accumulator for a foreign account (the seeds and discriminator
/// constraints).
pub fn burn_ix_with_counters(
    operator: Pubkey,
    burn_counters: Pubkey,
    vault: Pubkey,
    token_mint: Pubkey,
    amount: u64,
) -> Instruction {
    burn_ix_raw(
        operator,
        burn_counters,
        vault,
        vault_authority_pda(),
        token_mint,
        amount,
    )
}

/// Same as [`burn_ix`], but `vault_authority` is passed explicitly - for the "bring
/// your own authority" negative (the seeds constraint).
pub fn burn_ix_with_vault_authority(
    operator: Pubkey,
    vault: Pubkey,
    vault_authority: Pubkey,
    token_mint: Pubkey,
    amount: u64,
) -> Instruction {
    burn_ix_raw(
        operator,
        burn_counters_pda(),
        vault,
        vault_authority,
        token_mint,
        amount,
    )
}

fn burn_ix_raw(
    operator: Pubkey,
    burn_counters: Pubkey,
    vault: Pubkey,
    vault_authority: Pubkey,
    token_mint: Pubkey,
    amount: u64,
) -> Instruction {
    let accounts = burn::accounts::Burn {
        burn_state: burn_state_pda(),
        operator,
        burn_counters,
        vault,
        vault_authority,
        token_mint,
        token_program: spl_token_interface::id(),
        event_authority: burn_event_authority_pda(),
        program: burn::id(),
    };
    let data = burn::instruction::Burn { amount };
    Instruction {
        program_id: burn::id(),
        accounts: accounts.to_account_metas(None),
        data: data.data(),
    }
}

// ---------- The init fixture and the Burn state readers ----------

/// Extends [`TestEnv`] with the Burn initialization fixture and readers of its state.
/// A trait rather than an inherent impl: [`TestEnv`] lives in a foreign crate
/// (`sale-test-common`), so methods can only be attached to it through a trait.
pub trait BurnTestEnv {
    /// Initializes Burn with valid parameters: authority = the `owner` actor (the
    /// environment's upgrade authority), the roles are the owner/admin/operator actors,
    /// the mint is `win_mint` (dec 8) and the limits come from the arguments. It panics
    /// on rejection - a failing init here means a broken fixture, not a guard under test.
    fn init_burn_with_limits(&mut self, daily_limit: u64, weekly_limit: u64);

    /// [`Self::init_burn_with_limits`] with the default limits
    /// ([`DEFAULT_DAILY_LIMIT`] / [`DEFAULT_WEEKLY_LIMIT`]).
    fn init_default_burn(&mut self);

    /// Deserializes the `BurnState` singleton from the chain (stripping the 8-byte
    /// discriminator). Panics if the account is absent - a missing singleton must never
    /// be read silently (fail-closed).
    fn burn_state(&self) -> burn::state::BurnState;

    /// Whether the `BurnState` singleton exists on the chain.
    fn burn_state_exists(&self) -> bool;

    /// Deserializes the `BurnCounters` singleton accumulator from the chain (stripping
    /// the 8-byte discriminator). Panics if the account is absent - a missing singleton
    /// must never be read silently as zeroes (fail-closed).
    fn burn_counters(&self) -> burn::state::BurnCounters;

    /// The SPL mint's current `supply` - for the "exactly N units were burned"
    /// assertions. It lives here rather than in the shared engine's token.rs: only the
    /// burn tests need it.
    fn mint_supply(&self, mint: &Pubkey) -> u64;
}

impl BurnTestEnv for TestEnv {
    fn init_burn_with_limits(&mut self, daily_limit: u64, weekly_limit: u64) {
        let owner = self.actors.owner.insecure_clone();
        let ix = burn_initialize_ix(
            owner.pubkey(),
            self.win_mint,
            owner.pubkey(),
            self.actors.admin.pubkey(),
            self.actors.operator.pubkey(),
            daily_limit,
            weekly_limit,
        );
        self.send_ixs(&[ix], &owner.pubkey(), &[&owner])
            .expect("init_burn: initialize must succeed");
    }

    fn init_default_burn(&mut self) {
        self.init_burn_with_limits(DEFAULT_DAILY_LIMIT, DEFAULT_WEEKLY_LIMIT);
    }

    fn burn_state(&self) -> burn::state::BurnState {
        let acc = self
            .svm
            .get_account(&burn_state_pda())
            .expect("burn_state does not exist");
        let mut data: &[u8] = &acc.data[8..];
        anchor_lang::AnchorDeserialize::deserialize(&mut data).expect("BurnState deserializes")
    }

    fn burn_state_exists(&self) -> bool {
        self.svm.get_account(&burn_state_pda()).is_some()
    }

    fn burn_counters(&self) -> burn::state::BurnCounters {
        let acc = self
            .svm
            .get_account(&burn_counters_pda())
            .expect("burn_counters does not exist");
        let mut data: &[u8] = &acc.data[8..];
        anchor_lang::AnchorDeserialize::deserialize(&mut data).expect("BurnCounters deserializes")
    }

    fn mint_supply(&self, mint: &Pubkey) -> u64 {
        use solana_program_pack::Pack;
        let acc = self
            .svm
            .get_account(mint)
            .unwrap_or_else(|| panic!("mint {mint} does not exist"));
        spl_token_interface::state::Mint::unpack(&acc.data)
            .unwrap_or_else(|e| panic!("{mint} is not an SPL mint: {e}"))
            .supply
    }
}

// ---------- Raw SPL Token builders (the token layer around the program) ----------

/// A direct SPL `Burn` from `account`, signed by `authority` - the "around the
/// program" path: it never enters the Burn program, so its acceptance depends on
/// the token layer alone.
pub fn spl_burn_ix(account: Pubkey, mint: Pubkey, authority: Pubkey, amount: u64) -> Instruction {
    spl_token_interface::instruction::burn(
        &spl_token_interface::id(),
        &account,
        &mint,
        &authority,
        &[],
        amount,
    )
    .expect("the Burn instruction")
}

/// A direct SPL `Transfer` from `account` to `destination`, signed by `authority`:
/// funding the vault the way a client does, or the other "around the program" path.
pub fn spl_transfer_ix(
    account: Pubkey,
    destination: Pubkey,
    authority: Pubkey,
    amount: u64,
) -> Instruction {
    spl_token_interface::instruction::transfer(
        &spl_token_interface::id(),
        &account,
        &destination,
        &authority,
        &[],
        amount,
    )
    .expect("the Transfer instruction")
}
