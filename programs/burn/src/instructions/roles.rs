//! `propose_owner` / `cancel_owner_proposal` / `accept_owner` / `set_admin` /
//! `set_operator`. The owner handover is two-step; admin and operator are
//! one-step (§9.1).

use anchor_lang::prelude::*;

use crate::errors::BurnError;
use crate::events::{OwnerProposalCancelled, OwnerProposed, RoleKind, RoleUpdated};
use crate::seeds::BURN_STATE_SEED;
use crate::state::BurnState;

#[derive(Accounts)]
#[event_cpi]
pub struct OwnerUpdate<'info> {
    #[account(
        mut,
        seeds = [BURN_STATE_SEED],
        bump,
        has_one = owner @ BurnError::Unauthorized,
    )]
    pub burn_state: Account<'info, BurnState>,
    pub owner: Signer<'info>,
}

/// The second step of the handover is signed by the candidate, so there is no
/// `has_one = owner` here: the right is proven by the signer matching
/// `pending_owner`.
#[derive(Accounts)]
#[event_cpi]
pub struct AcceptOwner<'info> {
    #[account(mut, seeds = [BURN_STATE_SEED], bump)]
    pub burn_state: Account<'info, BurnState>,
    pub new_owner: Signer<'info>,
}

pub fn propose_owner(ctx: Context<OwnerUpdate>, new: Pubkey) -> Result<()> {
    require_keys_neq!(new, Pubkey::default(), BurnError::InvalidAuthority);
    let state = &mut ctx.accounts.burn_state;

    state.pending_owner = Some(new);

    emit_cpi!(OwnerProposed { pending_owner: new });
    Ok(())
}

pub fn cancel_owner_proposal(ctx: Context<OwnerUpdate>) -> Result<()> {
    let state = &mut ctx.accounts.burn_state;

    require!(state.pending_owner.is_some(), BurnError::NoPendingOwner);

    state.pending_owner = None;

    emit_cpi!(OwnerProposalCancelled {});
    Ok(())
}

pub fn accept_owner(ctx: Context<AcceptOwner>) -> Result<()> {
    let signer = ctx.accounts.new_owner.key();
    let state = &mut ctx.accounts.burn_state;

    let pending = state.pending_owner.ok_or(BurnError::NoPendingOwner)?;
    require_keys_eq!(pending, signer, BurnError::Unauthorized);

    state.owner = pending;
    state.pending_owner = None;

    emit_cpi!(RoleUpdated {
        role: RoleKind::Owner,
        new_pubkey: pending,
    });
    Ok(())
}

pub fn set_admin(ctx: Context<OwnerUpdate>, new: Pubkey) -> Result<()> {
    require_keys_neq!(new, Pubkey::default(), BurnError::InvalidAuthority);
    let state = &mut ctx.accounts.burn_state;

    state.admin = new;

    emit_cpi!(RoleUpdated {
        role: RoleKind::Admin,
        new_pubkey: new,
    });
    Ok(())
}

pub fn set_operator(ctx: Context<OwnerUpdate>, new: Pubkey) -> Result<()> {
    require_keys_neq!(new, Pubkey::default(), BurnError::InvalidAuthority);
    let state = &mut ctx.accounts.burn_state;

    state.operator = new;

    emit_cpi!(RoleUpdated {
        role: RoleKind::Operator,
        new_pubkey: new,
    });
    Ok(())
}
