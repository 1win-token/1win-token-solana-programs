#![allow(clippy::diverging_sub_expression)]

use anchor_lang::prelude::*;

pub mod errors;
pub mod events;
pub mod instructions;
pub mod state;

pub(crate) mod period;
pub(crate) mod seeds;

use instructions::*;

declare_id!("HuzMDQVv3TA4wWFW1m4pBQ4q5Sz7aHoPwLLGHEzpP8Cb");

#[program]
pub mod burn {
    use super::*;

    pub fn initialize(
        ctx: Context<Initialize>,
        owner: Pubkey,
        admin: Pubkey,
        operator: Pubkey,
        daily_limit: u64,
        weekly_limit: u64,
    ) -> Result<()> {
        instructions::initialize::handler(ctx, owner, admin, operator, daily_limit, weekly_limit)
    }

    pub fn set_limits(
        ctx: Context<AdminUpdate>,
        daily_limit: u64,
        weekly_limit: u64,
    ) -> Result<()> {
        instructions::params::set_limits(ctx, daily_limit, weekly_limit)
    }

    pub fn propose_owner(ctx: Context<OwnerUpdate>, new: Pubkey) -> Result<()> {
        instructions::roles::propose_owner(ctx, new)
    }

    pub fn cancel_owner_proposal(ctx: Context<OwnerUpdate>) -> Result<()> {
        instructions::roles::cancel_owner_proposal(ctx)
    }

    pub fn accept_owner(ctx: Context<AcceptOwner>) -> Result<()> {
        instructions::roles::accept_owner(ctx)
    }

    pub fn set_admin(ctx: Context<OwnerUpdate>, new: Pubkey) -> Result<()> {
        instructions::roles::set_admin(ctx, new)
    }

    pub fn set_operator(ctx: Context<OwnerUpdate>, new: Pubkey) -> Result<()> {
        instructions::roles::set_operator(ctx, new)
    }

    pub fn burn(ctx: Context<Burn>, amount: u64) -> Result<()> {
        instructions::burn::burn(ctx, amount)
    }
}
