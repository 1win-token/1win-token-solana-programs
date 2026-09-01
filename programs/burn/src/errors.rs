//! The program's single #[error_code] enum.

use anchor_lang::prelude::*;

#[error_code]
#[derive(PartialEq, Eq)]
pub enum BurnError {
    #[msg("Arithmetic overflow or invalid numeric downcast")]
    MathOverflow,
    #[msg("token_mint decimals must equal the $1WIN token decimals (8)")]
    InvalidMintDecimals,
    #[msg("Signer is not the authority required for this instruction")]
    Unauthorized,
    #[msg("Numeric parameter out of its allowed range")]
    InvalidParams,
    #[msg("Authority pubkey must not be the default (zero) pubkey")]
    InvalidAuthority,
    #[msg("token_mint does not match the mint fixed in burn_state")]
    InvalidMint,
    #[msg("On-chain clock returned a negative unix timestamp")]
    InvalidTimestamp,
    #[msg("Burn amount must be greater than zero")]
    ZeroBurnAmount,
    #[msg("Daily burned total would exceed daily_limit")]
    DailyLimitExceeded,
    #[msg("Weekly burned total would exceed weekly_limit")]
    WeeklyLimitExceeded,
    #[msg("No owner handover is pending")]
    NoPendingOwner,
}
