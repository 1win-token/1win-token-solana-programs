//! `set_admin` / `set_operator` / `propose_owner` / `cancel_owner_proposal` /
//! `accept_owner` - the Owner reassigns the roles (Arch §9.3, §9.4).

use anchor_lang::prelude::*;

use crate::errors::VestingError;
use crate::events::{
    AdminUpdated, OperatorUpdated, OwnerProposalCancelled, OwnerProposed, OwnerUpdated,
};
use crate::state::VestingConfig;

#[derive(Accounts)]
#[event_cpi]
pub struct OwnerUpdate<'info> {
    #[account(
        mut,
        seeds = [b"config"],
        bump,
        has_one = owner @ VestingError::NotOwner,
    )]
    pub config: Account<'info, VestingConfig>,
    pub owner: Signer<'info>,
}

/// The `accept_owner` context: the candidate signs, not the sitting owner, so
/// `has_one = owner` does not apply here - the body checks that the signer
/// matches `config.pending_owner`.
#[derive(Accounts)]
#[event_cpi]
pub struct AcceptOwner<'info> {
    #[account(mut, seeds = [b"config"], bump)]
    pub config: Account<'info, VestingConfig>,
    pub new_owner: Signer<'info>,
}

pub fn set_admin(ctx: Context<OwnerUpdate>, admin: Pubkey) -> Result<()> {
    VestingConfig::validate_role_key(admin)?;

    let config = &mut ctx.accounts.config;
    config.admin = admin;

    emit_cpi!(AdminUpdated { admin });
    Ok(())
}

pub fn set_operator(ctx: Context<OwnerUpdate>, operator: Pubkey) -> Result<()> {
    VestingConfig::validate_role_key(operator)?;

    let config = &mut ctx.accounts.config;
    config.operator = operator;

    emit_cpi!(OperatorUpdated { operator });
    Ok(())
}

pub fn propose_owner(ctx: Context<OwnerUpdate>, new_owner: Pubkey) -> Result<()> {
    VestingConfig::validate_role_key(new_owner)?;

    let config = &mut ctx.accounts.config;
    config.pending_owner = Some(new_owner);

    emit_cpi!(OwnerProposed {
        pending_owner: new_owner
    });
    Ok(())
}

pub fn cancel_owner_proposal(ctx: Context<OwnerUpdate>) -> Result<()> {
    let config = &mut ctx.accounts.config;

    require!(config.pending_owner.is_some(), VestingError::NoPendingOwner);

    config.pending_owner = None;

    emit_cpi!(OwnerProposalCancelled {});
    Ok(())
}

pub fn accept_owner(ctx: Context<AcceptOwner>) -> Result<()> {
    let signer = ctx.accounts.new_owner.key();
    let config = &mut ctx.accounts.config;

    let pending = config.pending_owner.ok_or(VestingError::NoPendingOwner)?;
    require_keys_eq!(pending, signer, VestingError::NotOwner);

    config.owner = pending;
    config.pending_owner = None;

    emit_cpi!(OwnerUpdated { new_owner: pending });
    Ok(())
}
