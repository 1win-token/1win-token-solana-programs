//! `pause` - Admin, only during the Sale phase. `unpause` - Admin, in any phase.

use anchor_lang::prelude::*;

use crate::errors::SaleError;
use crate::events::{Paused, Unpaused};
use crate::instructions::params::AdminUpdate;
use crate::phase::Phase;

pub fn pause(ctx: Context<AdminUpdate>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let state = &mut ctx.accounts.sale_state;

    require!(state.phase(now) == Phase::Sale, SaleError::WrongPhase);
    state.paused = true;

    emit_cpi!(Paused {});
    Ok(())
}

pub fn unpause(ctx: Context<AdminUpdate>) -> Result<()> {
    let state = &mut ctx.accounts.sale_state;

    state.paused = false;

    emit_cpi!(Unpaused {});
    Ok(())
}
