//! `set_withdrawal_authority` - Admin reassigns the address that receives the
//! remaining $1WIN and the rent when an activity is cancelled or closed.

use anchor_lang::prelude::*;

use crate::errors::VestingError;
use crate::events::WithdrawalAuthorityUpdated;
use crate::state::VestingConfig;

#[derive(Accounts)]
#[event_cpi]
pub struct SetWithdrawalAuthority<'info> {
    #[account(
        mut,
        seeds = [b"config"],
        bump,
        has_one = admin @ VestingError::NotAdmin,
    )]
    pub config: Account<'info, VestingConfig>,

    pub withdrawal_authority: SystemAccount<'info>,

    /// CHECK: the PDA that owns the vaults, no data; the address comes from the
    /// seeds and the bump from the config.
    #[account(seeds = [b"vault_authority"], bump = config.vault_authority_bump)]
    pub vault_authority: UncheckedAccount<'info>,

    pub admin: Signer<'info>,
}

pub fn set_withdrawal_authority(ctx: Context<SetWithdrawalAuthority>) -> Result<()> {
    let withdrawal_authority = ctx.accounts.withdrawal_authority.key();
    VestingConfig::validate_payee(withdrawal_authority, ctx.accounts.vault_authority.key())?;

    let config = &mut ctx.accounts.config;
    config.withdrawal_authority = withdrawal_authority;

    emit_cpi!(WithdrawalAuthorityUpdated {
        withdrawal_authority,
    });
    Ok(())
}
