//! `set_tge` - Admin moves the TGE date while it is still ahead.

use anchor_lang::prelude::*;

use crate::errors::VestingError;
use crate::events::TgeUpdated;
use crate::instructions::require_before_tge;
use crate::state::VestingConfig;

#[derive(Accounts)]
#[event_cpi]
pub struct SetTge<'info> {
    #[account(
        mut,
        seeds = [b"config"],
        bump,
        has_one = admin @ VestingError::NotAdmin,
    )]
    pub config: Account<'info, VestingConfig>,
    pub admin: Signer<'info>,
}

pub fn set_tge(ctx: Context<SetTge>, tge_ts: i64) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;

    require_before_tge(&ctx.accounts.config, now)?;

    VestingConfig::validate_tge(tge_ts, now)?;

    let config = &mut ctx.accounts.config;
    config.tge_ts = tge_ts;

    emit_cpi!(TgeUpdated { tge_ts });
    Ok(())
}
