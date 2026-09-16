//! The `SaleState` singleton account and the schedule guard constants.

use anchor_lang::prelude::*;

use crate::errors::SaleError;
use crate::math::TOKEN_UNITS;
use crate::phase::{current_phase, Phase, PhaseSchedule};

pub const MAX_CLAIM_DURATION: i64 = 365 * 24 * 60 * 60;
pub const MAX_CLAIM_START_DELAY: i64 = 30 * 24 * 60 * 60;
pub const MAX_SALE_START_DELAY: i64 = 90 * 24 * 60 * 60;
pub const MAX_SALE_DURATION: i64 = 30 * 24 * 60 * 60;
pub const TOKEN_DECIMALS: u8 = 8;
pub const USDC_DECIMALS: u8 = 6;
pub const MAX_CLOSE_PER_BATCH: usize = 20;

#[account]
#[derive(InitSpace)]
pub struct SaleState {
    pub owner: Pubkey,
    pub pending_owner: Option<Pubkey>,
    pub admin: Pubkey,
    pub operator: Pubkey,
    pub usdc_mint: Pubkey,
    pub token_mint: Pubkey,
    pub deposit_vault: Pubkey,
    pub token_vault: Pubkey,
    pub price_usdc: u64,
    pub emission: u64,
    pub min_per_tx: u64,
    pub max_per_tx: u64,
    /// USDC collected: grows on a purchase, shrinks on a refund made before the
    /// pool is fixed.
    pub deposits_usdc_contributed: u64,
    /// A snapshot of that counter taken when the pool was fixed; a wallet's
    /// share at claim time is computed against it.
    pub pool_fixed_amount: u64,
    /// How many aggregates were created; serves as the rounding headroom when
    /// the money is split across the vaults.
    pub participants: u64,
    pub sale_start_date: i64,
    pub sale_end_date: i64,
    pub claim_duration: i64,
    pub claim_start_date: i64,
    /// The pool is fixed: the sale no longer accepts money.
    pub pool_fixed: bool,
    pub paused: bool,
    /// The bump of the PDA the program signs vault transfers with.
    pub vault_authority_bump: u8,
}

impl SaleState {
    pub fn phase(&self, now: i64) -> Phase {
        current_phase(
            now,
            &PhaseSchedule {
                sale_start_date: self.sale_start_date,
                sale_end_date: self.sale_end_date,
                pool_fixed: self.pool_fixed,
                claim_start_date: self.claim_start_date,
                claim_duration: self.claim_duration,
            },
        )
    }

    /// Requires the PreSale phase: the sale has not started, parameters are
    /// still editable. Expected by the admin setters for price, emission,
    /// limits and the sale window.
    pub fn require_pre_sale(&self) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        require!(
            self.phase(now) == Phase::PreSale,
            SaleError::SaleAlreadyStarted
        );
        Ok(())
    }

    /// Requires the terminal phase: the pool is fixed and the claim deadline has
    /// passed. Expected by the sweep instructions and by the batched account
    /// closing.
    pub fn require_claim_closed(&self) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        require!(
            self.phase(now) == Phase::ClaimClosed,
            SaleError::DeadlineNotPassed
        );
        Ok(())
    }

    pub fn validate_price(price_usdc: u64) -> Result<()> {
        require!(price_usdc > 0, SaleError::InvalidParams);
        // The price is USDC per whole token. Above TOKEN_UNITS the smallest USDC
        // unit buys zero tokens; the exact boundary is cut off along with it.
        require!(price_usdc < TOKEN_UNITS, SaleError::InvalidParams);
        Ok(())
    }

    pub fn validate_emission(emission: u64) -> Result<()> {
        require!(emission > 0, SaleError::InvalidParams);
        Ok(())
    }

    pub fn validate_tx_limits(min_per_tx: u64, max_per_tx: u64) -> Result<()> {
        require!(min_per_tx > 0, SaleError::InvalidParams);
        require!(min_per_tx <= max_per_tx, SaleError::InvalidParams);
        Ok(())
    }

    pub fn validate_sale_window(now: i64, sale_start_date: i64, sale_end_date: i64) -> Result<()> {
        require!(now < sale_start_date, SaleError::InvalidDeadline);
        require!(sale_start_date < sale_end_date, SaleError::InvalidDeadline);
        let max_start_date = now
            .checked_add(MAX_SALE_START_DELAY)
            .ok_or(SaleError::MathOverflow)?;
        require!(
            sale_start_date <= max_start_date,
            SaleError::StartDateTooFar
        );
        let sale_duration = sale_end_date
            .checked_sub(sale_start_date)
            .ok_or(SaleError::MathOverflow)?;
        require!(
            sale_duration <= MAX_SALE_DURATION,
            SaleError::SaleDurationTooLong
        );
        Ok(())
    }

    /// Both bounds are measured from the later of the sale's end and `now`:
    /// before the sale ends that is `sale_end_date`, after it the moment of the
    /// edit - so a future start can always be scheduled while the pool is not
    /// fixed, and `activate_claim` (`now < claim_start_date`) is never locked out.
    pub fn validate_claim_start(now: i64, sale_end_date: i64, claim_start_date: i64) -> Result<()> {
        let anchor = sale_end_date.max(now);
        require!(anchor < claim_start_date, SaleError::ClaimStartInPast);
        let max_claim_start = anchor
            .checked_add(MAX_CLAIM_START_DELAY)
            .ok_or(SaleError::MathOverflow)?;
        require!(
            claim_start_date <= max_claim_start,
            SaleError::ClaimStartTooFar
        );
        Ok(())
    }

    pub fn validate_claim_duration(claim_duration: i64) -> Result<()> {
        require!(claim_duration > 0, SaleError::InvalidParams);
        require!(
            claim_duration <= MAX_CLAIM_DURATION,
            SaleError::InvalidParams
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests;
