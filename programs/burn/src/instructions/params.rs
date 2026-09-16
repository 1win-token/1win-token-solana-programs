//! `set_limits` - Admin changes the daily and weekly limits at any time
//! (BRD §4.2, TRD §6.3).

use anchor_lang::prelude::*;

use crate::errors::BurnError;
use crate::events::LimitsUpdated;
use crate::seeds::BURN_STATE_SEED;
use crate::state::BurnState;

/// The shared context of the Admin instructions.
#[derive(Accounts)]
#[event_cpi]
pub struct AdminUpdate<'info> {
    #[account(
        mut,
        seeds = [BURN_STATE_SEED],
        bump,
        has_one = admin @ BurnError::Unauthorized,
    )]
    pub burn_state: Account<'info, BurnState>,
    pub admin: Signer<'info>,
}

pub fn set_limits(ctx: Context<AdminUpdate>, daily_limit: u64, weekly_limit: u64) -> Result<()> {
    require!(daily_limit > 0, BurnError::InvalidParams);
    require!(weekly_limit > 0, BurnError::InvalidParams);
    let state = &mut ctx.accounts.burn_state;

    state.daily_limit = daily_limit;
    state.weekly_limit = weekly_limit;

    emit_cpi!(LimitsUpdated {
        daily_limit,
        weekly_limit,
    });
    Ok(())
}
