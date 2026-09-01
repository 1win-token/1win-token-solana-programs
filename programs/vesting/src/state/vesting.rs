//! The `Vesting` activity account - one PDA per emission bucket, seeds
//! `[b"vesting", activity_id_le]`.

use anchor_lang::prelude::*;

#[derive(Debug, Clone, Copy)]
pub struct Schedule {
    pub reference_amount: u64,
    pub tge_bps: u16,
    pub monthly_bps: u16,
    pub cliff_months: u16,
}

#[account]
#[derive(InitSpace)]
pub struct Vesting {
    pub recipient: Pubkey,
    pub reference_amount: u64,
    pub claimed: u64,
    pub tge_bps: u16,
    pub monthly_bps: u16,
    pub cliff_months: u16,
}

impl Vesting {
    pub fn schedule(&self) -> Schedule {
        Schedule {
            reference_amount: self.reference_amount,
            tge_bps: self.tge_bps,
            monthly_bps: self.monthly_bps,
            cliff_months: self.cliff_months,
        }
    }
}
