//! Typed Sale instruction builders, PDA helpers and the `SaleState` reader.
//!
//! Every builder assembles an `Instruction` out of the Anchor-generated
//! `sale::accounts::*` (account metadata via `ToAccountMetas`) and
//! `sale::instruction::*` (data via `InstructionData`) - that is, the test calls the
//! REAL instruction through exactly the interface a production client uses
//! (tests-bind-to-real-code: we do not duplicate the body, we invoke the real
//! handler).
//!
//! The PDA helpers derive the addresses from the same seeds declared in the
//! program's contexts (`initialize.rs` / `params.rs` / `roles.rs`).

use anchor_lang::{InstructionData, ToAccountMetas};
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;
use solana_signer::Signer;

use crate::env::TestEnv;

// ---------- PDA helpers (the seeds are copied from the program's contexts) ----------

/// The `SaleState` singleton, seeds `[b"sale_state"]` (initialize.rs).
pub fn sale_state_pda() -> Pubkey {
    Pubkey::find_program_address(&[b"sale_state"], &sale::id()).0
}

/// The PDA that owns every vault, seeds `[b"vault_authority"]` (initialize.rs).
pub fn vault_authority_pda() -> Pubkey {
    Pubkey::find_program_address(&[b"vault_authority"], &sale::id()).0
}

/// The USDC intake vault, seeds `[b"deposit_vault"]` (initialize.rs).
pub fn deposit_vault_pda() -> Pubkey {
    Pubkey::find_program_address(&[b"deposit_vault"], &sale::id()).0
}

/// The token emission vault, seeds `[b"token_vault"]` (initialize.rs).
pub fn token_vault_pda() -> Pubkey {
    Pubkey::find_program_address(&[b"token_vault"], &sale::id()).0
}

/// `#[event_cpi]`-authority, seeds `[b"__event_authority"]` (Anchor).
pub fn event_authority_pda() -> Pubkey {
    Pubkey::find_program_address(&[b"__event_authority"], &sale::id()).0
}

/// The `Blacklist` marker of a wallet, seeds `[b"blacklist", wallet]` (blacklist.rs).
pub fn blacklist_pda(wallet: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"blacklist", wallet.as_ref()], &sale::id()).0
}

/// The `Graylist` record of a wallet, seeds `[b"graylist", wallet]` (graylist.rs).
pub fn graylist_pda(wallet: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"graylist", wallet.as_ref()], &sale::id()).0
}

/// A wallet's `WalletAggregate`, seeds `[b"aggregate", wallet]` (purchase.rs).
pub fn aggregate_pda(wallet: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"aggregate", wallet.as_ref()], &sale::id()).0
}

/// A `Deposit`, seeds `[b"deposit", wallet, index_le]` (purchase.rs, §6.2).
pub fn deposit_pda(wallet: &Pubkey, index: u64) -> Pubkey {
    Pubkey::find_program_address(
        &[b"deposit", wallet.as_ref(), &index.to_le_bytes()],
        &sale::id(),
    )
    .0
}

/// The proceeds vault, seeds `[b"proceeds_vault"]` (activate_claim.rs, §9.9).
pub fn proceeds_vault_pda() -> Pubkey {
    Pubkey::find_program_address(&[b"proceeds_vault"], &sale::id()).0
}

/// The refund vault, seeds `[b"refund_vault"]` (activate_claim.rs, §9.9).
pub fn refund_vault_pda() -> Pubkey {
    Pubkey::find_program_address(&[b"refund_vault"], &sale::id()).0
}

/// A wallet's ATA for the USDC mint - the `refund` recipient (§9.8, §9.10).
pub fn wallet_usdc_ata(wallet: &Pubkey, usdc_mint: &Pubkey) -> Pubkey {
    spl_associated_token_account_interface::address::get_associated_token_address(wallet, usdc_mint)
}

/// A wallet's ATA for the $1WIN mint - the recipient of the released tokens (§9.10).
pub fn wallet_token_ata(wallet: &Pubkey, token_mint: &Pubkey) -> Pubkey {
    spl_associated_token_account_interface::address::get_associated_token_address(
        wallet, token_mint,
    )
}

// ---------- The initialize arguments (a self-consistent set) ----------

/// A self-consistent set of `initialize` arguments relative to `now`.
///
/// The economic reference values come from `expected` (the single source) and the
/// dates are derived from `now`: sale_start_date = now+1000,
/// sale_end_date = now+2000, claim_start_date = sale_end_date+1000,
/// claim_duration = 90 days. Every initialize invariant holds, and the tests build
/// their deviations from exactly this set.
#[derive(Clone, Copy, Debug)]
pub struct InitParams {
    pub owner: Pubkey,
    pub admin: Pubkey,
    pub operator: Pubkey,
    pub price_usdc: u64,
    pub emission: u64,
    pub min_per_tx: u64,
    pub max_per_tx: u64,
    pub sale_start_date: i64,
    pub sale_end_date: i64,
    pub claim_duration: i64,
    pub claim_start_date: i64,
}

/// 90 days in seconds - a valid Claim duration (< MAX_CLAIM_DURATION = 12 months).
pub const CLAIM_DURATION_90D: i64 = 90 * 24 * 60 * 60;

impl TestEnv {
    /// A valid, self-consistent set of `initialize` arguments relative to the current
    /// `now()`. The roles are the environment's actors, the mints are the environment's
    /// (USDC dec 6, $1WIN dec 8) and the economics are the `expected` defaults.
    pub fn valid_init_params(&self) -> InitParams {
        let now = self.now();
        let sale_start_date = now + 1_000;
        let sale_end_date = now + 2_000;
        let claim_start_date = sale_end_date + 1_000;
        InitParams {
            owner: self.actors.owner.pubkey(),
            admin: self.actors.admin.pubkey(),
            operator: self.actors.operator.pubkey(),
            price_usdc: expected::DEFAULT_PRICE_USDC,
            emission: expected::DEFAULT_EMISSION,
            min_per_tx: expected::P3_NET, // $10 - a valid minimum (0 < min ≤ max)
            max_per_tx: expected::P1_NET, // $10 000
            sale_start_date,
            sale_end_date,
            claim_duration: CLAIM_DURATION_90D,
            claim_start_date,
        }
    }
}

// ---------- Instruction builders ----------

/// `initialize`: `authority` signs; the mints are the `usdc_mint` / `token_mint`
/// that were passed in (the tests swap them for the decimals / identical-mint
/// negatives).
#[allow(clippy::too_many_arguments)]
pub fn initialize_ix(
    authority: Pubkey,
    usdc_mint: Pubkey,
    token_mint: Pubkey,
    p: &InitParams,
) -> Instruction {
    let accounts = sale::accounts::Initialize {
        authority,
        sale_program: sale::id(),
        program_data: TestEnv::programdata_address(),
        sale_state: sale_state_pda(),
        vault_authority: vault_authority_pda(),
        deposit_vault: deposit_vault_pda(),
        token_vault: token_vault_pda(),
        usdc_mint,
        token_mint,
        token_program: spl_token_interface::id(),
        system_program: solana_sdk_ids::system_program::id(),
        event_authority: event_authority_pda(),
        program: sale::id(),
    };
    let data = sale::instruction::Initialize {
        owner: p.owner,
        admin: p.admin,
        operator: p.operator,
        price_usdc: p.price_usdc,
        emission: p.emission,
        min_per_tx: p.min_per_tx,
        max_per_tx: p.max_per_tx,
        sale_start_date: p.sale_start_date,
        sale_end_date: p.sale_end_date,
        claim_start_date: p.claim_start_date,
        claim_duration: p.claim_duration,
    };
    Instruction {
        program_id: sale::id(),
        accounts: accounts.to_account_metas(None),
        data: data.data(),
    }
}

/// The metadata of the `AdminUpdate` context (set_price/emission/tx_limits/dates/
/// claim_schedule, pause/unpause). `admin` is the signer.
fn admin_update_metas(admin: Pubkey) -> Vec<solana_instruction::AccountMeta> {
    sale::accounts::AdminUpdate {
        sale_state: sale_state_pda(),
        admin,
        event_authority: event_authority_pda(),
        program: sale::id(),
    }
    .to_account_metas(None)
}

/// The metadata of the `OwnerUpdate` context (set_owner/admin/operator). `owner` is
/// the signer.
fn owner_update_metas(owner: Pubkey) -> Vec<solana_instruction::AccountMeta> {
    sale::accounts::OwnerUpdate {
        sale_state: sale_state_pda(),
        owner,
        event_authority: event_authority_pda(),
        program: sale::id(),
    }
    .to_account_metas(None)
}

fn admin_ix(admin: Pubkey, data: Vec<u8>) -> Instruction {
    Instruction {
        program_id: sale::id(),
        accounts: admin_update_metas(admin),
        data,
    }
}

fn owner_ix(owner: Pubkey, data: Vec<u8>) -> Instruction {
    Instruction {
        program_id: sale::id(),
        accounts: owner_update_metas(owner),
        data,
    }
}

pub fn set_price_ix(admin: Pubkey, price_usdc: u64) -> Instruction {
    admin_ix(admin, sale::instruction::SetPrice { price_usdc }.data())
}

pub fn set_emission_ix(admin: Pubkey, emission: u64) -> Instruction {
    admin_ix(admin, sale::instruction::SetEmission { emission }.data())
}

pub fn set_tx_limits_ix(admin: Pubkey, min_per_tx: u64, max_per_tx: u64) -> Instruction {
    admin_ix(
        admin,
        sale::instruction::SetTxLimits {
            min_per_tx,
            max_per_tx,
        }
        .data(),
    )
}

pub fn set_sale_schedule_ix(
    admin: Pubkey,
    sale_start_date: i64,
    sale_end_date: i64,
) -> Instruction {
    admin_ix(
        admin,
        sale::instruction::SetSaleSchedule {
            sale_start_date,
            sale_end_date,
        }
        .data(),
    )
}

pub fn set_claim_schedule_ix(
    admin: Pubkey,
    claim_start_date: i64,
    claim_duration: i64,
) -> Instruction {
    admin_ix(
        admin,
        sale::instruction::SetClaimSchedule {
            claim_start_date,
            claim_duration,
        }
        .data(),
    )
}

pub fn pause_ix(admin: Pubkey) -> Instruction {
    admin_ix(admin, sale::instruction::Pause {}.data())
}

pub fn unpause_ix(admin: Pubkey) -> Instruction {
    admin_ix(admin, sale::instruction::Unpause {}.data())
}

/// `propose_owner`: the sitting `owner` signs and `new` is the candidate.
pub fn propose_owner_ix(owner: Pubkey, new: Pubkey) -> Instruction {
    owner_ix(
        owner,
        sale::instruction::ProposeOwner { new_authority: new }.data(),
    )
}

/// `cancel_owner_proposal`: the sitting `owner` withdraws the nominated candidate.
pub fn cancel_owner_proposal_ix(owner: Pubkey) -> Instruction {
    owner_ix(owner, sale::instruction::CancelOwnerProposal {}.data())
}

/// `accept_owner`: the candidate signs; the sitting owner takes no part in the
/// context.
pub fn accept_owner_ix(new_owner: Pubkey) -> Instruction {
    let accounts = sale::accounts::AcceptOwner {
        sale_state: sale_state_pda(),
        new_owner,
        event_authority: event_authority_pda(),
        program: sale::id(),
    };
    Instruction {
        program_id: sale::id(),
        accounts: accounts.to_account_metas(None),
        data: sale::instruction::AcceptOwner {}.data(),
    }
}

pub fn set_admin_ix(owner: Pubkey, new: Pubkey) -> Instruction {
    owner_ix(
        owner,
        sale::instruction::SetAdmin { new_authority: new }.data(),
    )
}

pub fn set_operator_ix(owner: Pubkey, new: Pubkey) -> Instruction {
    owner_ix(
        owner,
        sale::instruction::SetOperator { new_authority: new }.data(),
    )
}

/// `blacklist_add`: `operator` signs and pays the marker's rent; `wallet` is the
/// wallet being blocked - it is passed as an instruction argument and the
/// `blacklist` slot is derived from it.
pub fn blacklist_add_ix(operator: Pubkey, wallet: Pubkey) -> Instruction {
    blacklist_add_ix_with_slot(operator, wallet, blacklist_pda(&wallet))
}

/// Same as [`blacklist_add_ix`], but the `blacklist` slot is passed explicitly - for
/// the "the PDA does not match the `wallet` argument" negative (a seed mismatch →
/// ConstraintSeeds).
pub fn blacklist_add_ix_with_slot(
    operator: Pubkey,
    wallet: Pubkey,
    blacklist: Pubkey,
) -> Instruction {
    let accounts = sale::accounts::BlacklistAdd {
        sale_state: sale_state_pda(),
        operator,
        blacklist,
        system_program: solana_sdk_ids::system_program::id(),
        event_authority: event_authority_pda(),
        program: sale::id(),
    };
    Instruction {
        program_id: sale::id(),
        accounts: accounts.to_account_metas(None),
        data: sale::instruction::BlacklistAdd { wallet }.data(),
    }
}

/// `blacklist_remove`: `operator` signs and receives the closed marker's rent; the
/// wallet is an instruction argument.
pub fn blacklist_remove_ix(operator: Pubkey, wallet: Pubkey) -> Instruction {
    blacklist_remove_ix_with_slot(operator, wallet, blacklist_pda(&wallet))
}

/// Same as [`blacklist_remove_ix`], but the `blacklist` slot is passed explicitly.
pub fn blacklist_remove_ix_with_slot(
    operator: Pubkey,
    wallet: Pubkey,
    blacklist: Pubkey,
) -> Instruction {
    let accounts = sale::accounts::BlacklistRemove {
        sale_state: sale_state_pda(),
        operator,
        blacklist,
        event_authority: event_authority_pda(),
        program: sale::id(),
    };
    Instruction {
        program_id: sale::id(),
        accounts: accounts.to_account_metas(None),
        data: sale::instruction::BlacklistRemove { wallet }.data(),
    }
}

/// `graylist_create`: `admin` signs and pays the record's rent; `wallet` is the
/// wallet under individual limits - it is passed as an instruction argument and the
/// `graylist` slot is derived from it.
pub fn graylist_create_ix(
    admin: Pubkey,
    wallet: Pubkey,
    min_per_tx: u64,
    max_per_tx: u64,
    max_total: u64,
) -> Instruction {
    graylist_create_ix_with_slot(
        admin,
        wallet,
        graylist_pda(&wallet),
        min_per_tx,
        max_per_tx,
        max_total,
    )
}

/// Same as [`graylist_create_ix`], but the `graylist` slot is passed explicitly - for
/// the "the PDA does not match the `wallet` argument" negative (a seed mismatch →
/// ConstraintSeeds).
pub fn graylist_create_ix_with_slot(
    admin: Pubkey,
    wallet: Pubkey,
    graylist: Pubkey,
    min_per_tx: u64,
    max_per_tx: u64,
    max_total: u64,
) -> Instruction {
    let accounts = sale::accounts::GraylistCreate {
        sale_state: sale_state_pda(),
        admin,
        graylist,
        system_program: solana_sdk_ids::system_program::id(),
        event_authority: event_authority_pda(),
        program: sale::id(),
    };
    Instruction {
        program_id: sale::id(),
        accounts: accounts.to_account_metas(None),
        data: sale::instruction::GraylistCreate {
            wallet,
            min_per_tx,
            max_per_tx,
            max_total,
        }
        .data(),
    }
}

/// `graylist_update`: `admin` signs; it overwrites the limits of an existing record,
/// and the wallet is an instruction argument.
pub fn graylist_update_ix(
    admin: Pubkey,
    wallet: Pubkey,
    min_per_tx: u64,
    max_per_tx: u64,
    max_total: u64,
) -> Instruction {
    graylist_update_ix_with_slot(
        admin,
        wallet,
        graylist_pda(&wallet),
        min_per_tx,
        max_per_tx,
        max_total,
    )
}

/// Same as [`graylist_update_ix`], but the `graylist` slot is passed explicitly.
pub fn graylist_update_ix_with_slot(
    admin: Pubkey,
    wallet: Pubkey,
    graylist: Pubkey,
    min_per_tx: u64,
    max_per_tx: u64,
    max_total: u64,
) -> Instruction {
    let accounts = sale::accounts::GraylistUpdate {
        sale_state: sale_state_pda(),
        admin,
        graylist,
        event_authority: event_authority_pda(),
        program: sale::id(),
    };
    Instruction {
        program_id: sale::id(),
        accounts: accounts.to_account_metas(None),
        data: sale::instruction::GraylistUpdate {
            wallet,
            min_per_tx,
            max_per_tx,
            max_total,
        }
        .data(),
    }
}

/// `graylist_remove`: `admin` signs and receives the closed record's rent; the wallet
/// is an instruction argument.
pub fn graylist_remove_ix(admin: Pubkey, wallet: Pubkey) -> Instruction {
    graylist_remove_ix_with_slot(admin, wallet, graylist_pda(&wallet))
}

/// Same as [`graylist_remove_ix`], but the `graylist` slot is passed explicitly.
pub fn graylist_remove_ix_with_slot(
    admin: Pubkey,
    wallet: Pubkey,
    graylist: Pubkey,
) -> Instruction {
    let accounts = sale::accounts::GraylistRemove {
        sale_state: sale_state_pda(),
        admin,
        graylist,
        event_authority: event_authority_pda(),
        program: sale::id(),
    };
    Instruction {
        program_id: sale::id(),
        accounts: accounts.to_account_metas(None),
        data: sale::instruction::GraylistRemove { wallet }.data(),
    }
}

/// `init_aggregate`: `user` signs and pays the aggregate's rent; the blacklist slot
/// is the canonical PDA.
pub fn init_aggregate_ix(user: Pubkey) -> Instruction {
    init_aggregate_ix_with_slots(user, blacklist_pda(&user))
}

/// Same as [`init_aggregate_ix`], but the blacklist slot is passed explicitly - for
/// the slot-substitution negative.
pub fn init_aggregate_ix_with_slots(user: Pubkey, blacklist: Pubkey) -> Instruction {
    let accounts = sale::accounts::InitAggregate {
        sale_state: sale_state_pda(),
        user,
        aggregate: aggregate_pda(&user),
        blacklist,
        system_program: solana_sdk_ids::system_program::id(),
        event_authority: event_authority_pda(),
        program: sale::id(),
    };
    Instruction {
        program_id: sale::id(),
        accounts: accounts.to_account_metas(None),
        data: sale::instruction::InitAggregate {}.data(),
    }
}

/// `purchase`: the canonical account set. `user` signs; `deposit` is derived from
/// `deposit_index` (= the current `aggregate.deposit_count`); the blacklist/graylist
/// slots and `deposit_vault` are the canonical PDAs.
pub fn purchase_ix(
    user: Pubkey,
    user_usdc_ata: Pubkey,
    deposit_index: u64,
    amount: u64,
) -> Instruction {
    purchase_ix_with_slots(
        user,
        aggregate_pda(&user),
        deposit_pda(&user, deposit_index),
        blacklist_pda(&user),
        graylist_pda(&user),
        user_usdc_ata,
        deposit_vault_pda(),
        amount,
    )
}

/// `purchase` with a fully explicit account set - for the negatives: substituting the
/// blacklist or graylist slot, the vault, the aggregate or the deposit.
#[allow(clippy::too_many_arguments)]
pub fn purchase_ix_with_slots(
    user: Pubkey,
    aggregate: Pubkey,
    deposit: Pubkey,
    blacklist: Pubkey,
    graylist: Pubkey,
    user_usdc_ata: Pubkey,
    deposit_vault: Pubkey,
    amount: u64,
) -> Instruction {
    let accounts = sale::accounts::Purchase {
        sale_state: sale_state_pda(),
        user,
        aggregate,
        deposit,
        blacklist,
        graylist,
        user_usdc_ata,
        deposit_vault,
        token_program: spl_token_interface::id(),
        system_program: solana_sdk_ids::system_program::id(),
        event_authority: event_authority_pda(),
        program: sale::id(),
    };
    Instruction {
        program_id: sale::id(),
        accounts: accounts.to_account_metas(None),
        data: sale::instruction::Purchase { amount }.data(),
    }
}

/// `refund(index)`: the canonical set. `operator` signs and pays the rent of the ATA
/// being (re)created; `wallet` is the deposit's owner (the recipient of the refund
/// and of the closing rent) and its owning program is not checked; the deposit,
/// aggregate, ATA and vault are the canonical PDAs.
pub fn refund_ix(operator: Pubkey, wallet: Pubkey, index: u64, usdc_mint: Pubkey) -> Instruction {
    refund_ix_with_vault(operator, wallet, index, usdc_mint, deposit_vault_pda())
}

/// Same as [`refund_ix`], but `deposit_vault` is passed explicitly - for the vault
/// substitution negative (`address = sale_state.deposit_vault` → ConstraintAddress).
pub fn refund_ix_with_vault(
    operator: Pubkey,
    wallet: Pubkey,
    index: u64,
    usdc_mint: Pubkey,
    deposit_vault: Pubkey,
) -> Instruction {
    let accounts = sale::accounts::Refund {
        sale_state: sale_state_pda(),
        operator,
        wallet,
        deposit: deposit_pda(&wallet, index),
        aggregate: aggregate_pda(&wallet),
        wallet_usdc_ata: wallet_usdc_ata(&wallet, &usdc_mint),
        deposit_vault,
        vault_authority: vault_authority_pda(),
        usdc_mint,
        token_program: spl_token_interface::id(),
        associated_token_program: spl_associated_token_account_interface::program::ID,
        system_program: solana_sdk_ids::system_program::id(),
        event_authority: event_authority_pda(),
        program: sale::id(),
    };
    Instruction {
        program_id: sale::id(),
        accounts: accounts.to_account_metas(None),
        data: sale::instruction::Refund { index }.data(),
    }
}

/// `assert_funded`: every slot is canonical.
///
/// The instruction has no signer at all, so the builder takes no signer parameter:
/// the transaction is signed only by its fee payer, and in a test that is any key -
/// which is exactly how permissionless access is verified.
pub fn assert_funded_ix() -> Instruction {
    assert_funded_ix_with_slots(sale_state_pda(), token_vault_pda())
}

/// Same as [`assert_funded_ix`], but every slot is passed explicitly - for a decoy
/// `SaleState` and for substituting the token vault.
pub fn assert_funded_ix_with_slots(sale_state: Pubkey, token_vault: Pubkey) -> Instruction {
    let accounts = sale::accounts::AssertFunded {
        sale_state,
        token_vault,
    };
    Instruction {
        program_id: sale::id(),
        accounts: accounts.to_account_metas(None),
        data: sale::instruction::AssertFunded {}.data(),
    }
}

/// `activate_claim`: `admin` signs and pays the rent of the vaults being created.
/// `refund_vault` is optional (Some when oversubscribed, None when undersubscribed);
/// for the `RefundVaultMismatch` negative the "wrong" side is passed.
pub fn activate_claim_ix(
    admin: Pubkey,
    usdc_mint: Pubkey,
    refund_vault: Option<Pubkey>,
) -> Instruction {
    let accounts = sale::accounts::ActivateClaim {
        sale_state: sale_state_pda(),
        admin,
        vault_authority: vault_authority_pda(),
        deposit_vault: deposit_vault_pda(),
        token_vault: token_vault_pda(),
        proceeds_vault: proceeds_vault_pda(),
        refund_vault,
        usdc_mint,
        token_program: spl_token_interface::id(),
        system_program: solana_sdk_ids::system_program::id(),
        event_authority: event_authority_pda(),
        program: sale::id(),
    };
    Instruction {
        program_id: sale::id(),
        accounts: accounts.to_account_metas(None),
        data: sale::instruction::ActivateClaim {}.data(),
    }
}

/// `claim`: `wallet` signs, pays the ATA rent, and receives the tokens and the
/// refund. `with_refund` selects the canonical profile: `true` passes
/// `refund_vault` + the USDC ATA (oversubscription with a refund > 0), `false` passes
/// neither (undersubscribed). Pinpoint negatives (one side without the other) go
/// through [`claim_ix_full`].
pub fn claim_ix(
    wallet: Pubkey,
    usdc_mint: Pubkey,
    token_mint: Pubkey,
    with_refund: bool,
) -> Instruction {
    let (refund_vault, ata) = if with_refund {
        (
            Some(refund_vault_pda()),
            Some(wallet_usdc_ata(&wallet, &usdc_mint)),
        )
    } else {
        (None, None)
    };
    claim_ix_full(
        wallet,
        aggregate_pda(&wallet),
        usdc_mint,
        token_mint,
        refund_vault,
        ata,
    )
}

/// A fully explicit `claim` - for the negatives: a foreign `aggregate` (a
/// seeds/`has_one` mismatch), and `refund_vault` / the USDC ATA one at a time
/// (fail-closed `RefundVaultMissing` / `RefundAtaMissing`).
pub fn claim_ix_full(
    wallet: Pubkey,
    aggregate: Pubkey,
    usdc_mint: Pubkey,
    token_mint: Pubkey,
    refund_vault: Option<Pubkey>,
    wallet_usdc_ata: Option<Pubkey>,
) -> Instruction {
    let accounts = sale::accounts::Claim {
        sale_state: sale_state_pda(),
        wallet,
        aggregate,
        token_vault: token_vault_pda(),
        wallet_token_ata: wallet_token_ata(&wallet, &token_mint),
        refund_vault,
        wallet_usdc_ata,
        vault_authority: vault_authority_pda(),
        token_mint,
        usdc_mint,
        token_program: spl_token_interface::id(),
        associated_token_program: spl_associated_token_account_interface::program::ID,
        system_program: solana_sdk_ids::system_program::id(),
        event_authority: event_authority_pda(),
        program: sale::id(),
    };
    Instruction {
        program_id: sale::id(),
        accounts: accounts.to_account_metas(None),
        data: sale::instruction::Claim {}.data(),
    }
}

/// The Owner's ATA for the $1WIN mint - the recipient of the unreleased tokens
/// (`sweep_token_vault`).
pub fn owner_token_ata(owner: &Pubkey, token_mint: &Pubkey) -> Pubkey {
    spl_associated_token_account_interface::address::get_associated_token_address(owner, token_mint)
}

/// `withdraw_proceeds`: `owner` signs and pays the ATA rent; the rent of the closed
/// `proceeds_vault` goes to `rent_collector`.
pub fn withdraw_proceeds_ix(
    owner: Pubkey,
    usdc_mint: Pubkey,
    rent_collector: Pubkey,
) -> Instruction {
    let accounts = sale::accounts::WithdrawProceeds {
        sale_state: sale_state_pda(),
        owner,
        proceeds_vault: proceeds_vault_pda(),
        owner_usdc_ata: wallet_usdc_ata(&owner, &usdc_mint),
        rent_collector,
        vault_authority: vault_authority_pda(),
        usdc_mint,
        token_program: spl_token_interface::id(),
        associated_token_program: spl_associated_token_account_interface::program::ID,
        system_program: solana_sdk_ids::system_program::id(),
        event_authority: event_authority_pda(),
        program: sale::id(),
    };
    Instruction {
        program_id: sale::id(),
        accounts: accounts.to_account_metas(None),
        data: sale::instruction::WithdrawProceeds {}.data(),
    }
}

/// `sweep_refund_vault`: `owner` signs; the remainder goes to the Owner's ATA (USDC)
/// and the closed vault's rent to `rent_collector`.
pub fn sweep_refund_vault_ix(
    owner: Pubkey,
    usdc_mint: Pubkey,
    rent_collector: Pubkey,
) -> Instruction {
    let accounts = sale::accounts::SweepRefundVault {
        sale_state: sale_state_pda(),
        owner,
        refund_vault: refund_vault_pda(),
        owner_usdc_ata: wallet_usdc_ata(&owner, &usdc_mint),
        rent_collector,
        vault_authority: vault_authority_pda(),
        usdc_mint,
        token_program: spl_token_interface::id(),
        associated_token_program: spl_associated_token_account_interface::program::ID,
        system_program: solana_sdk_ids::system_program::id(),
        event_authority: event_authority_pda(),
        program: sale::id(),
    };
    Instruction {
        program_id: sale::id(),
        accounts: accounts.to_account_metas(None),
        data: sale::instruction::SweepRefundVault {}.data(),
    }
}

/// `sweep_deposit_vault`: stray transfers into `deposit_vault` → the Owner's ATA
/// (USDC).
pub fn sweep_deposit_vault_ix(
    owner: Pubkey,
    usdc_mint: Pubkey,
    rent_collector: Pubkey,
) -> Instruction {
    let accounts = sale::accounts::SweepDepositVault {
        sale_state: sale_state_pda(),
        owner,
        deposit_vault: deposit_vault_pda(),
        owner_usdc_ata: wallet_usdc_ata(&owner, &usdc_mint),
        rent_collector,
        vault_authority: vault_authority_pda(),
        usdc_mint,
        token_program: spl_token_interface::id(),
        associated_token_program: spl_associated_token_account_interface::program::ID,
        system_program: solana_sdk_ids::system_program::id(),
        event_authority: event_authority_pda(),
        program: sale::id(),
    };
    Instruction {
        program_id: sale::id(),
        accounts: accounts.to_account_metas(None),
        data: sale::instruction::SweepDepositVault {}.data(),
    }
}

/// `sweep_token_vault`: the unreleased tokens of `token_vault` → the Owner's ATA
/// ($1WIN).
pub fn sweep_token_vault_ix(
    owner: Pubkey,
    token_mint: Pubkey,
    rent_collector: Pubkey,
) -> Instruction {
    let accounts = sale::accounts::SweepTokenVault {
        sale_state: sale_state_pda(),
        owner,
        token_vault: token_vault_pda(),
        owner_token_ata: owner_token_ata(&owner, &token_mint),
        rent_collector,
        vault_authority: vault_authority_pda(),
        token_mint,
        token_program: spl_token_interface::id(),
        associated_token_program: spl_associated_token_account_interface::program::ID,
        system_program: solana_sdk_ids::system_program::id(),
        event_authority: event_authority_pda(),
        program: sale::id(),
    };
    Instruction {
        program_id: sale::id(),
        accounts: accounts.to_account_metas(None),
        data: sale::instruction::SweepTokenVault {}.data(),
    }
}

/// Lays the `[(account, wallet), …]` pairs out into `remaining_accounts`: every
/// account is writable (it is being closed) and every wallet is writable (it receives
/// the rent).
fn close_pairs_metas(pairs: &[(Pubkey, Pubkey)]) -> Vec<AccountMeta> {
    let mut metas = Vec::with_capacity(pairs.len() * 2);
    for (acc, wallet) in pairs {
        metas.push(AccountMeta::new(*acc, false));
        metas.push(AccountMeta::new(*wallet, false));
    }
    metas
}

/// `close_deposits(pairs)`: `operator` signs; `pairs` = `[(deposit, wallet), …]` in
/// `remaining_accounts` (the tests substitute forges and duplicates for the
/// negatives).
pub fn close_deposits_ix(operator: Pubkey, pairs: &[(Pubkey, Pubkey)]) -> Instruction {
    let mut accounts = sale::accounts::CloseDeposits {
        sale_state: sale_state_pda(),
        operator,
        event_authority: event_authority_pda(),
        program: sale::id(),
    }
    .to_account_metas(None);
    accounts.extend(close_pairs_metas(pairs));
    Instruction {
        program_id: sale::id(),
        accounts,
        data: sale::instruction::CloseDeposits {}.data(),
    }
}

/// `close_aggregates(pairs)`: `operator` signs; `pairs` = `[(aggregate, wallet), …]`.
pub fn close_aggregates_ix(operator: Pubkey, pairs: &[(Pubkey, Pubkey)]) -> Instruction {
    let mut accounts = sale::accounts::CloseAggregates {
        sale_state: sale_state_pda(),
        operator,
        event_authority: event_authority_pda(),
        program: sale::id(),
    }
    .to_account_metas(None);
    accounts.extend(close_pairs_metas(pairs));
    Instruction {
        program_id: sale::id(),
        accounts,
        data: sale::instruction::CloseAggregates {}.data(),
    }
}

// ---------- The SaleState reader (for asserting stored fields) ----------

impl TestEnv {
    /// Initializes the sale with valid parameters (owner = the upgrade authority) and
    /// returns the `InitParams` that were used. A helper for the phase and role tests
    /// that need a ready singleton. It panics on rejection - a failing init here means a
    /// broken fixture, not a guard under test.
    pub fn init_default_sale(&mut self) -> InitParams {
        let owner = self.actors.owner.insecure_clone();
        let (usdc, win) = (self.usdc_mint, self.win_mint);
        let p = self.valid_init_params();
        let ix = initialize_ix(owner.pubkey(), usdc, win, &p);
        self.send_ixs(&[ix], &owner.pubkey(), &[&owner])
            .expect("init_default_sale: initialize must succeed");
        p
    }

    /// Deserializes the `SaleState` singleton from the chain (stripping the 8-byte
    /// discriminator). Panics if the account is absent - a missing singleton must never
    /// be read silently (fail-closed).
    pub fn sale_state(&self) -> sale::state::SaleState {
        let acc = self
            .svm
            .get_account(&sale_state_pda())
            .expect("sale_state does not exist");
        let mut data: &[u8] = &acc.data[8..];
        anchor_lang::AnchorDeserialize::deserialize(&mut data).expect("SaleState deserializes")
    }

    /// Whether the `SaleState` singleton exists on the chain.
    pub fn sale_state_exists(&self) -> bool {
        self.svm.get_account(&sale_state_pda()).is_some()
    }

    /// Whether a wallet's `Blacklist` marker exists (its presence means blocked).
    pub fn is_blacklisted(&self, wallet: &Pubkey) -> bool {
        self.svm.get_account(&blacklist_pda(wallet)).is_some()
    }

    /// Deserializes a wallet's `Blacklist` marker (to assert the stored `wallet`). Panics
    /// if the account is absent.
    pub fn blacklist_entry(&self, wallet: &Pubkey) -> sale::state::Blacklist {
        let acc = self
            .svm
            .get_account(&blacklist_pda(wallet))
            .expect("the blacklist marker does not exist");
        let mut data: &[u8] = &acc.data[8..];
        anchor_lang::AnchorDeserialize::deserialize(&mut data).expect("Blacklist deserializes")
    }

    /// Whether a wallet's `Graylist` record exists (its presence means individual limits
    /// instead of the global ones).
    pub fn is_graylisted(&self, wallet: &Pubkey) -> bool {
        self.svm.get_account(&graylist_pda(wallet)).is_some()
    }

    /// Deserializes a wallet's `Graylist` record (to assert the stored limits). Panics if
    /// the account is absent.
    pub fn graylist_entry(&self, wallet: &Pubkey) -> sale::state::Graylist {
        let acc = self
            .svm
            .get_account(&graylist_pda(wallet))
            .expect("the graylist record does not exist");
        let mut data: &[u8] = &acc.data[8..];
        anchor_lang::AnchorDeserialize::deserialize(&mut data).expect("Graylist deserializes")
    }

    /// Whether a wallet's `WalletAggregate` exists on the chain.
    pub fn aggregate_exists(&self, wallet: &Pubkey) -> bool {
        self.svm.get_account(&aggregate_pda(wallet)).is_some()
    }

    /// Deserializes a wallet's `WalletAggregate`. Panics if it is absent.
    pub fn wallet_aggregate(&self, wallet: &Pubkey) -> sale::state::WalletAggregate {
        let acc = self
            .svm
            .get_account(&aggregate_pda(wallet))
            .expect("the aggregate does not exist");
        let mut data: &[u8] = &acc.data[8..];
        anchor_lang::AnchorDeserialize::deserialize(&mut data)
            .expect("WalletAggregate deserializes")
    }

    /// Whether a `Deposit` with the given index exists on the chain.
    pub fn deposit_exists(&self, wallet: &Pubkey, index: u64) -> bool {
        self.svm.get_account(&deposit_pda(wallet, index)).is_some()
    }

    /// Whether an account exists at an address (for the vault creation/closing
    /// assertions: `proceeds_vault` always, `refund_vault` only under
    /// oversubscription).
    pub fn account_exists(&self, key: &Pubkey) -> bool {
        self.svm.get_account(key).is_some()
    }

    /// Deserializes the `Deposit` at `(wallet, index)`. Panics if it is absent.
    pub fn deposit_entry(&self, wallet: &Pubkey, index: u64) -> sale::state::Deposit {
        let acc = self
            .svm
            .get_account(&deposit_pda(wallet, index))
            .expect("the deposit does not exist");
        let mut data: &[u8] = &acc.data[8..];
        anchor_lang::AnchorDeserialize::deserialize(&mut data).expect("Deposit deserializes")
    }

    /// The `(mint, owner)` of the SPL token account at an address - for asserting a
    /// vault's binding (mint == usdc/token, owner == vault_authority).
    pub fn token_account_mint_owner(&self, token_account: &Pubkey) -> (Pubkey, Pubkey) {
        use solana_program_pack::Pack;
        let acc = self
            .svm
            .get_account(token_account)
            .unwrap_or_else(|| panic!("token account {token_account} does not exist"));
        let parsed = spl_token_interface::state::Account::unpack(&acc.data)
            .unwrap_or_else(|e| panic!("{token_account} is not an SPL token account: {e}"));
        (parsed.mint, parsed.owner)
    }
}
