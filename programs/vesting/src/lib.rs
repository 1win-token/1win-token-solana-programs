#![allow(clippy::diverging_sub_expression)]
//! The $1WIN Vesting program: gradual unlock of the emission buckets outside the
//! sale. Fourteen instructions (Arch §9); the delegates live in
//! `instructions::*`.

use anchor_lang::prelude::*;

pub mod errors;
pub mod events;
pub mod instructions;
pub mod state;

pub mod math;

use instructions::*;

declare_id!("9AxqkB7qfMxQrbUwfhkyBTuG4DDEVa8t5utEeSM7b1hQ");

#[program]
pub mod vesting {
    use super::*;

    pub fn initialize(
        ctx: Context<Initialize>,
        owner: Pubkey,
        admin: Pubkey,
        operator: Pubkey,
        tge_ts: i64,
    ) -> Result<()> {
        instructions::initialize::initialize(ctx, owner, admin, operator, tge_ts)
    }

    pub fn set_tge(ctx: Context<SetTge>, tge_ts: i64) -> Result<()> {
        instructions::tge::set_tge(ctx, tge_ts)
    }

    pub fn set_admin(ctx: Context<OwnerUpdate>, admin: Pubkey) -> Result<()> {
        instructions::roles::set_admin(ctx, admin)
    }

    pub fn set_operator(ctx: Context<OwnerUpdate>, operator: Pubkey) -> Result<()> {
        instructions::roles::set_operator(ctx, operator)
    }

    pub fn propose_owner(ctx: Context<OwnerUpdate>, new_owner: Pubkey) -> Result<()> {
        instructions::roles::propose_owner(ctx, new_owner)
    }

    pub fn cancel_owner_proposal(ctx: Context<OwnerUpdate>) -> Result<()> {
        instructions::roles::cancel_owner_proposal(ctx)
    }

    pub fn accept_owner(ctx: Context<AcceptOwner>) -> Result<()> {
        instructions::roles::accept_owner(ctx)
    }

    pub fn set_withdrawal_authority(ctx: Context<SetWithdrawalAuthority>) -> Result<()> {
        instructions::withdrawal::set_withdrawal_authority(ctx)
    }

    pub fn create_vesting(
        ctx: Context<CreateVesting>,
        reference_amount: u64,
        tge_bps: u16,
        monthly_bps: u16,
        cliff_months: u16,
        label: String,
    ) -> Result<()> {
        instructions::create::create_vesting(
            ctx,
            reference_amount,
            tge_bps,
            monthly_bps,
            cliff_months,
            label,
        )
    }

    pub fn set_recipient(ctx: Context<SetRecipient>, activity_id: u64) -> Result<()> {
        instructions::recipient::set_recipient(ctx, activity_id)
    }

    pub fn cancel_vesting(ctx: Context<CloseActivity>, activity_id: u64) -> Result<()> {
        instructions::close::cancel_vesting(ctx, activity_id)
    }

    pub fn close_vesting(ctx: Context<CloseActivity>, activity_id: u64) -> Result<()> {
        instructions::close::close_vesting(ctx, activity_id)
    }

    pub fn claim(ctx: Context<Claim>, activity_id: u64) -> Result<()> {
        instructions::claim::claim(ctx, activity_id)
    }

    pub fn assert_funded(ctx: Context<AssertFunded>, activity_id: u64) -> Result<()> {
        instructions::funded::assert_funded(ctx, activity_id)
    }
}
