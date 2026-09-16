//! The program's single #[error_code] enum. The variant order is frozen: it
//! defines the 6000+ codes the tests and the off-chain indexer rely on.

use anchor_lang::prelude::*;

#[error_code]
#[derive(PartialEq, Eq)]
pub enum VestingError {
    #[msg("Arithmetic overflow or invalid numeric downcast")]
    MathOverflow,
    #[msg("Signer is not the Owner: Owner instructions require config.owner, accept_owner requires config.pending_owner")]
    NotOwner,
    #[msg("Signer is not the upgrade authority of the program")]
    NotUpgradeAuthority,
    #[msg("Signer is neither the activity recipient nor the operator")]
    NotAuthorized,
    #[msg("Role, recipient or withdrawal_authority pubkey is the default (zero) one, or the recipient or withdrawal_authority is the vault_authority")]
    InvalidAuthority,
    #[msg("token_mint does not match the mint fixed in the vesting config")]
    InvalidMint,
    #[msg("token_mint decimals do not match the $1WIN decimals")]
    InvalidMintDecimals,
    #[msg("tge_ts must satisfy now < tge_ts <= now + MAX_TGE_DELAY")]
    InvalidTge,
    #[msg("Activity label is longer than MAX_LABEL_LEN bytes")]
    InvalidParams,
    #[msg("reference_amount must be greater than zero")]
    InvalidReferenceAmount,
    #[msg("Basis points value exceeds 10000")]
    InvalidBps,
    #[msg("Schedule does not unlock the reference amount within the allowed months")]
    InvalidSchedule,
    #[msg("beneficiary_token is not owned by config.withdrawal_authority, or it is the activity vault")]
    InvalidBeneficiary,
    #[msg("TGE has already started; the schedule and the activity set are final")]
    ScheduleStarted,
    #[msg("TGE is not set or has not started yet")]
    ScheduleNotStarted,
    #[msg("Nothing is claimable at this time")]
    NothingToClaim,
    #[msg("Activity vault is underfunded: claim requires the claimable amount, assert_funded requires the unclaimed remainder of reference_amount")]
    InsufficientVaultBalance,
    #[msg("Activity is not fully claimed yet")]
    NotFullyClaimed,
    #[msg("rent_collector is not config.withdrawal_authority")]
    InvalidRentCollector,
    #[msg("No owner candidate is proposed")]
    NoPendingOwner,
    #[msg("Signer is not the Admin of the vesting config")]
    NotAdmin,
    #[msg("Recipient or withdrawal address is off-curve; only a wallet with its own private key can receive a payout")]
    PayeeNotOnCurve,
}
