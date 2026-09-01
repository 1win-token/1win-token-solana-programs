//! The `WalletAggregate` record - one PDA per wallet, seeds
//! `[b"aggregate", wallet]`.

use anchor_lang::prelude::*;

#[account]
#[derive(InitSpace)]
pub struct WalletAggregate {
    pub wallet: Pubkey,
    pub net_usdc_contributed: u64,
    pub deposit_count: u64,
}
