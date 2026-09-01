//! The `Blacklist` marker - one PDA per wallet, seeds `[b"blacklist", wallet]`.

use anchor_lang::prelude::*;

#[account]
#[derive(InitSpace)]
pub struct Blacklist {
    pub wallet: Pubkey,
}
