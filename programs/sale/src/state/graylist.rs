//! The `Graylist` record - one PDA per wallet, seeds `[b"graylist", wallet]`.

use anchor_lang::prelude::*;

use crate::errors::SaleError;
use crate::state::SaleState;

#[account]
#[derive(InitSpace)]
pub struct Graylist {
    pub wallet: Pubkey,
    pub min_per_tx: u64,
    pub max_per_tx: u64,
    pub max_total: u64,
}

impl Graylist {
    pub fn validate_limits(min_per_tx: u64, max_per_tx: u64, max_total: u64) -> Result<()> {
        SaleState::validate_tx_limits(min_per_tx, max_per_tx)?;
        require!(max_per_tx <= max_total, SaleError::InvalidParams);
        Ok(())
    }
}
