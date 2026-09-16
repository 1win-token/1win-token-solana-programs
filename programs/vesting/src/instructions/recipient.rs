//! `set_recipient` - Admin reassigns an activity's recipient. It works at any
//! point in the activity's life: the next claim goes to the new address, while
//! what was already handed out stays with the previous one.

use anchor_lang::prelude::*;

use crate::errors::VestingError;
use crate::events::RecipientUpdated;
use crate::state::{Vesting, VestingConfig};

#[derive(Accounts)]
#[instruction(activity_id: u64)]
#[event_cpi]
pub struct SetRecipient<'info> {
    #[account(
        seeds = [b"config"],
        bump,
        has_one = admin @ VestingError::NotAdmin,
    )]
    pub config: Account<'info, VestingConfig>,

    #[account(
        mut,
        seeds = [b"vesting", activity_id.to_le_bytes().as_ref()],
        bump,
    )]
    pub vesting: Account<'info, Vesting>,

    /// The new payout wallet: the same type and the same `validate_payee` gates
    /// as in `create_vesting`, so both write paths admit one class of address.
    pub new_recipient: SystemAccount<'info>,

    /// CHECK: the PDA that owns the vaults, no data; the address comes from the
    /// seeds and the bump from the config.
    #[account(seeds = [b"vault_authority"], bump = config.vault_authority_bump)]
    pub vault_authority: UncheckedAccount<'info>,

    pub admin: Signer<'info>,
}

pub fn set_recipient(ctx: Context<SetRecipient>, activity_id: u64) -> Result<()> {
    let new_recipient = ctx.accounts.new_recipient.key();
    VestingConfig::validate_payee(new_recipient, ctx.accounts.vault_authority.key())?;

    let vesting = &mut ctx.accounts.vesting;
    vesting.recipient = new_recipient;

    emit_cpi!(RecipientUpdated {
        activity_id,
        new_recipient,
    });
    Ok(())
}
