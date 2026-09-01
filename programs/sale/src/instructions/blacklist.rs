//! `blacklist_add` / `blacklist_remove` - Operator, at any time.

use anchor_lang::prelude::*;

use crate::errors::SaleError;
use crate::events::{BlacklistRemoved, Blacklisted};
use crate::seeds::{BLACKLIST_SEED, SALE_STATE_SEED};
use crate::state::{Blacklist, SaleState};

#[derive(Accounts)]
#[instruction(wallet: Pubkey)]
#[event_cpi]
pub struct BlacklistAdd<'info> {
    #[account(
        seeds = [SALE_STATE_SEED],
        bump,
        has_one = operator @ SaleError::Unauthorized,
    )]
    pub sale_state: Account<'info, SaleState>,

    #[account(mut)]
    pub operator: Signer<'info>,

    #[account(
        init,
        seeds = [BLACKLIST_SEED, wallet.as_ref()],
        bump,
        payer = operator,
        space = 8 + Blacklist::INIT_SPACE,
    )]
    pub blacklist: Account<'info, Blacklist>,

    pub system_program: Program<'info, System>,
}

pub fn blacklist_add(ctx: Context<BlacklistAdd>, wallet: Pubkey) -> Result<()> {
    let entry = &mut ctx.accounts.blacklist;

    entry.wallet = wallet;

    emit_cpi!(Blacklisted { wallet });
    Ok(())
}

#[derive(Accounts)]
#[instruction(wallet: Pubkey)]
#[event_cpi]
pub struct BlacklistRemove<'info> {
    #[account(
        seeds = [SALE_STATE_SEED],
        bump,
        has_one = operator @ SaleError::Unauthorized,
    )]
    pub sale_state: Account<'info, SaleState>,

    #[account(mut)]
    pub operator: Signer<'info>,

    #[account(
        mut,
        close = operator,
        seeds = [BLACKLIST_SEED, wallet.as_ref()],
        bump,
    )]
    pub blacklist: Account<'info, Blacklist>,
}

pub fn blacklist_remove(ctx: Context<BlacklistRemove>, wallet: Pubkey) -> Result<()> {
    emit_cpi!(BlacklistRemoved { wallet });
    Ok(())
}
