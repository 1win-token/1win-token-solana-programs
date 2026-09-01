//! The `Deposit` record - one PDA per contribution, seeds
//! `[b"deposit", wallet, index_le]`.

use anchor_lang::prelude::*;

#[account]
#[derive(InitSpace)]
pub struct Deposit {
    pub wallet: Pubkey,
    pub amount: u64,
    pub index: u64,
}
