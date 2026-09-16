//! The program's single #[error_code] enum.

use anchor_lang::prelude::*;

#[error_code]
#[derive(PartialEq, Eq)]
pub enum SaleError {
    #[msg("Arithmetic overflow or invalid numeric downcast")]
    MathOverflow,
    #[msg("Mint decimals out of representable range (10^decimals must fit in u64)")]
    InvalidMintDecimals,
    #[msg("Signer is not the authority required for this instruction")]
    Unauthorized,
    #[msg("Numeric parameter out of its allowed range")]
    InvalidParams,
    #[msg("usdc_mint and token_mint must be distinct mints")]
    InvalidMint,
    #[msg("Sale schedule violates now < sale_start_date < sale_end_date")]
    InvalidDeadline,
    #[msg("Economy and sale schedule are frozen once the Sale phase has started")]
    SaleAlreadyStarted,
    #[msg("Pool is already fixed; the Claim schedule can no longer change")]
    PoolAlreadyFixed,
    #[msg("claim_start_date must lie ahead of both sale_end_date and now")]
    ClaimStartInPast,
    #[msg("claim_start_date exceeds max(sale_end_date, now) + MAX_CLAIM_START_DELAY")]
    ClaimStartTooFar,
    #[msg("Instruction is not allowed in the current phase")]
    WrongPhase,
    #[msg("Purchases are paused")]
    SalePaused,
    #[msg("Wallet is blacklisted; purchase is rejected")]
    WalletBlacklisted,
    #[msg("Purchase amount is below the minimum per transaction")]
    BelowMin,
    #[msg("Purchase amount is above the maximum per transaction")]
    AboveMax,
    #[msg("Wallet total contribution would exceed its graylist max_total")]
    WalletTotalExceeded,
    #[msg("token_vault is underfunded: activate_claim requires the distributable amount, assert_funded requires the full emission")]
    VaultNotFunded,
    #[msg("deposit_vault holds less USDC than the accounted pool")]
    DepositVaultUnderfunded,
    #[msg("refund_vault presence does not match the oversubscription branch")]
    RefundVaultMismatch,
    #[msg("Claim deadline has passed; claiming is closed")]
    ClaimDeadlinePassed,
    #[msg("Wallet has no net contribution to claim")]
    NothingToClaim,
    #[msg("refund_vault account is required when refund is positive")]
    RefundVaultMissing,
    #[msg("wallet USDC associated token account is required when refund is positive")]
    RefundAtaMissing,
    #[msg("Pool is not fixed yet; claim has not been activated")]
    PoolNotFixed,
    #[msg("Claim has not started yet (now < claim_start_date)")]
    ClaimNotStarted,
    #[msg("Claim deadline has not passed yet")]
    DeadlineNotPassed,
    #[msg("An account in the close batch failed validation")]
    InvalidAccountInBatch,
    #[msg("Close batch exceeds the maximum accounts per call")]
    BatchTooLarge,
    #[msg("Authority pubkey must not be the default (zero) pubkey")]
    InvalidAuthority,
    #[msg("sale_start_date exceeds now + MAX_SALE_START_DELAY")]
    StartDateTooFar,
    #[msg("Sale window sale_end_date - sale_start_date exceeds MAX_SALE_DURATION")]
    SaleDurationTooLong,
    #[msg("No owner handover is pending; propose_owner must run first")]
    NoPendingOwner,
    #[msg("Buyer address is off-curve; only a wallet with its own private key can take part in the sale")]
    WalletNotOnCurve,
}
