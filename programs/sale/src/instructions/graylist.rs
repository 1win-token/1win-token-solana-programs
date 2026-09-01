//! `graylist_set` (= `graylist_create` + `graylist_update`) / `graylist_remove` - Admin, at any time.

use anchor_lang::prelude::*;

use crate::errors::SaleError;
use crate::events::{GraylistRemoved, GraylistSet};
use crate::seeds::{GRAYLIST_SEED, SALE_STATE_SEED};
use crate::state::{Graylist, SaleState};

#[derive(Accounts)]
#[instruction(wallet: Pubkey)]
#[event_cpi]
pub struct GraylistCreate<'info> {
    #[account(
        seeds = [SALE_STATE_SEED],
        bump,
        has_one = admin @ SaleError::Unauthorized,
    )]
    pub sale_state: Account<'info, SaleState>,

    #[account(mut)]
    pub admin: Signer<'info>,

    #[account(
        init,
        seeds = [GRAYLIST_SEED, wallet.as_ref()],
        bump,
        payer = admin,
        space = 8 + Graylist::INIT_SPACE,
    )]
    pub graylist: Account<'info, Graylist>,

    pub system_program: Program<'info, System>,
}

pub fn graylist_create(
    ctx: Context<GraylistCreate>,
    wallet: Pubkey,
    min_per_tx: u64,
    max_per_tx: u64,
    max_total: u64,
) -> Result<()> {
    Graylist::validate_limits(min_per_tx, max_per_tx, max_total)?;

    let entry = &mut ctx.accounts.graylist;

    entry.wallet = wallet;
    entry.min_per_tx = min_per_tx;
    entry.max_per_tx = max_per_tx;
    entry.max_total = max_total;

    emit_cpi!(GraylistSet {
        wallet,
        min_per_tx,
        max_per_tx,
        max_total,
    });
    Ok(())
}

#[derive(Accounts)]
#[instruction(wallet: Pubkey)]
#[event_cpi]
pub struct GraylistUpdate<'info> {
    #[account(
        seeds = [SALE_STATE_SEED],
        bump,
        has_one = admin @ SaleError::Unauthorized,
    )]
    pub sale_state: Account<'info, SaleState>,

    pub admin: Signer<'info>,

    #[account(
        mut,
        seeds = [GRAYLIST_SEED, wallet.as_ref()],
        bump,
    )]
    pub graylist: Account<'info, Graylist>,
}

pub fn graylist_update(
    ctx: Context<GraylistUpdate>,
    wallet: Pubkey,
    min_per_tx: u64,
    max_per_tx: u64,
    max_total: u64,
) -> Result<()> {
    Graylist::validate_limits(min_per_tx, max_per_tx, max_total)?;

    let entry = &mut ctx.accounts.graylist;

    entry.min_per_tx = min_per_tx;
    entry.max_per_tx = max_per_tx;
    entry.max_total = max_total;

    emit_cpi!(GraylistSet {
        wallet,
        min_per_tx,
        max_per_tx,
        max_total,
    });
    Ok(())
}

#[derive(Accounts)]
#[instruction(wallet: Pubkey)]
#[event_cpi]
pub struct GraylistRemove<'info> {
    #[account(
        seeds = [SALE_STATE_SEED],
        bump,
        has_one = admin @ SaleError::Unauthorized,
    )]
    pub sale_state: Account<'info, SaleState>,

    #[account(mut)]
    pub admin: Signer<'info>,

    #[account(
        mut,
        close = admin,
        seeds = [GRAYLIST_SEED, wallet.as_ref()],
        bump,
    )]
    pub graylist: Account<'info, Graylist>,
}

pub fn graylist_remove(ctx: Context<GraylistRemove>, wallet: Pubkey) -> Result<()> {
    emit_cpi!(GraylistRemoved { wallet });
    Ok(())
}
