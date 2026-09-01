//! `propose_owner` / `cancel_owner_proposal` / `accept_owner` / `set_admin` /
//! `set_operator`

use anchor_lang::prelude::*;

use crate::errors::SaleError;
use crate::events::{OwnerProposalCancelled, OwnerProposed, RoleKind, RoleUpdated};
use crate::seeds::SALE_STATE_SEED;
use crate::state::SaleState;

#[derive(Accounts)]
#[event_cpi]
pub struct OwnerUpdate<'info> {
    #[account(
        mut,
        seeds = [SALE_STATE_SEED],
        bump,
        has_one = owner @ SaleError::Unauthorized,
    )]
    pub sale_state: Account<'info, SaleState>,
    pub owner: Signer<'info>,
}

#[derive(Accounts)]
#[event_cpi]
pub struct AcceptOwner<'info> {
    #[account(mut, seeds = [SALE_STATE_SEED], bump)]
    pub sale_state: Account<'info, SaleState>,
    pub new_owner: Signer<'info>,
}

pub fn propose_owner(ctx: Context<OwnerUpdate>, new_authority: Pubkey) -> Result<()> {
    require_keys_neq!(
        new_authority,
        Pubkey::default(),
        SaleError::InvalidAuthority
    );
    let state = &mut ctx.accounts.sale_state;

    state.pending_owner = Some(new_authority);

    emit_cpi!(OwnerProposed {
        pending_owner: new_authority
    });
    Ok(())
}

pub fn cancel_owner_proposal(ctx: Context<OwnerUpdate>) -> Result<()> {
    let state = &mut ctx.accounts.sale_state;

    require!(state.pending_owner.is_some(), SaleError::NoPendingOwner);

    state.pending_owner = None;

    emit_cpi!(OwnerProposalCancelled {});
    Ok(())
}

pub fn accept_owner(ctx: Context<AcceptOwner>) -> Result<()> {
    let signer = ctx.accounts.new_owner.key();
    let state = &mut ctx.accounts.sale_state;

    let pending = state.pending_owner.ok_or(SaleError::NoPendingOwner)?;
    require_keys_eq!(pending, signer, SaleError::Unauthorized);

    state.owner = pending;
    state.pending_owner = None;

    emit_cpi!(RoleUpdated {
        role: RoleKind::Owner,
        new_pubkey: pending,
    });
    Ok(())
}

pub fn set_admin(ctx: Context<OwnerUpdate>, new_authority: Pubkey) -> Result<()> {
    require_keys_neq!(
        new_authority,
        Pubkey::default(),
        SaleError::InvalidAuthority
    );
    let state = &mut ctx.accounts.sale_state;

    state.admin = new_authority;

    emit_cpi!(RoleUpdated {
        role: RoleKind::Admin,
        new_pubkey: new_authority,
    });
    Ok(())
}

pub fn set_operator(ctx: Context<OwnerUpdate>, new_authority: Pubkey) -> Result<()> {
    require_keys_neq!(
        new_authority,
        Pubkey::default(),
        SaleError::InvalidAuthority
    );
    let state = &mut ctx.accounts.sale_state;

    state.operator = new_authority;

    emit_cpi!(RoleUpdated {
        role: RoleKind::Operator,
        new_pubkey: new_authority,
    });
    Ok(())
}
