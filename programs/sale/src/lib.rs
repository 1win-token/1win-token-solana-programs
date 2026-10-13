#![allow(clippy::diverging_sub_expression)]

use anchor_lang::prelude::*;

pub mod errors;
pub mod events;
pub mod instructions;
pub mod state;

pub(crate) mod math;
pub(crate) mod phase;
pub(crate) mod seeds;
pub(crate) mod vault;

use instructions::*;

declare_id!("GWvQinGBRa7ZUzhXY23MrLjdsx3KfRGxH8yXURBwrz1R");

#[program]
pub mod sale {
    use super::*;

    #[allow(clippy::too_many_arguments)]
    pub fn initialize(
        ctx: Context<Initialize>,
        owner: Pubkey,
        admin: Pubkey,
        operator: Pubkey,
        price_usdc: u64,
        emission: u64,
        min_per_tx: u64,
        max_per_tx: u64,
        sale_start_date: i64,
        sale_end_date: i64,
        claim_start_date: i64,
        claim_duration: i64,
    ) -> Result<()> {
        instructions::initialize::initialize(
            ctx,
            owner,
            admin,
            operator,
            price_usdc,
            emission,
            min_per_tx,
            max_per_tx,
            sale_start_date,
            sale_end_date,
            claim_start_date,
            claim_duration,
        )
    }

    pub fn set_price(ctx: Context<AdminUpdate>, price_usdc: u64) -> Result<()> {
        instructions::params::set_price(ctx, price_usdc)
    }

    pub fn set_emission(ctx: Context<AdminUpdate>, emission: u64) -> Result<()> {
        instructions::params::set_emission(ctx, emission)
    }

    pub fn set_tx_limits(
        ctx: Context<AdminUpdate>,
        min_per_tx: u64,
        max_per_tx: u64,
    ) -> Result<()> {
        instructions::params::set_tx_limits(ctx, min_per_tx, max_per_tx)
    }

    pub fn set_sale_schedule(
        ctx: Context<AdminUpdate>,
        sale_start_date: i64,
        sale_end_date: i64,
    ) -> Result<()> {
        instructions::params::set_sale_schedule(ctx, sale_start_date, sale_end_date)
    }

    pub fn set_claim_schedule(
        ctx: Context<AdminUpdate>,
        claim_start_date: i64,
        claim_duration: i64,
    ) -> Result<()> {
        instructions::params::set_claim_schedule(ctx, claim_start_date, claim_duration)
    }

    pub fn pause(ctx: Context<AdminUpdate>) -> Result<()> {
        instructions::pause::pause(ctx)
    }

    pub fn unpause(ctx: Context<AdminUpdate>) -> Result<()> {
        instructions::pause::unpause(ctx)
    }

    pub fn propose_owner(ctx: Context<OwnerUpdate>, new_authority: Pubkey) -> Result<()> {
        instructions::roles::propose_owner(ctx, new_authority)
    }

    pub fn cancel_owner_proposal(ctx: Context<OwnerUpdate>) -> Result<()> {
        instructions::roles::cancel_owner_proposal(ctx)
    }

    pub fn accept_owner(ctx: Context<AcceptOwner>) -> Result<()> {
        instructions::roles::accept_owner(ctx)
    }

    pub fn set_admin(ctx: Context<OwnerUpdate>, new_authority: Pubkey) -> Result<()> {
        instructions::roles::set_admin(ctx, new_authority)
    }

    pub fn set_operator(ctx: Context<OwnerUpdate>, new_authority: Pubkey) -> Result<()> {
        instructions::roles::set_operator(ctx, new_authority)
    }

    pub fn blacklist_add(ctx: Context<BlacklistAdd>, wallet: Pubkey) -> Result<()> {
        instructions::blacklist::blacklist_add(ctx, wallet)
    }

    pub fn blacklist_remove(ctx: Context<BlacklistRemove>, wallet: Pubkey) -> Result<()> {
        instructions::blacklist::blacklist_remove(ctx, wallet)
    }

    pub fn graylist_create(
        ctx: Context<GraylistCreate>,
        wallet: Pubkey,
        min_per_tx: u64,
        max_per_tx: u64,
        max_total: u64,
    ) -> Result<()> {
        instructions::graylist::graylist_create(ctx, wallet, min_per_tx, max_per_tx, max_total)
    }

    pub fn graylist_update(
        ctx: Context<GraylistUpdate>,
        wallet: Pubkey,
        min_per_tx: u64,
        max_per_tx: u64,
        max_total: u64,
    ) -> Result<()> {
        instructions::graylist::graylist_update(ctx, wallet, min_per_tx, max_per_tx, max_total)
    }

    pub fn graylist_remove(ctx: Context<GraylistRemove>, wallet: Pubkey) -> Result<()> {
        instructions::graylist::graylist_remove(ctx, wallet)
    }

    pub fn init_aggregate(ctx: Context<InitAggregate>) -> Result<()> {
        instructions::purchase::init_aggregate(ctx)
    }

    pub fn purchase(ctx: Context<Purchase>, amount: u64) -> Result<()> {
        instructions::purchase::purchase(ctx, amount)
    }

    pub fn refund(ctx: Context<Refund>, index: u64) -> Result<()> {
        instructions::refund::refund(ctx, index)
    }

    pub fn assert_funded(ctx: Context<AssertFunded>) -> Result<()> {
        instructions::funded::assert_funded(ctx)
    }

    pub fn activate_claim(ctx: Context<ActivateClaim>) -> Result<()> {
        instructions::activate_claim::activate_claim(ctx)
    }

    pub fn claim(ctx: Context<Claim>) -> Result<()> {
        instructions::claim::claim(ctx)
    }

    pub fn withdraw_proceeds(ctx: Context<WithdrawProceeds>) -> Result<()> {
        instructions::withdraw_proceeds::withdraw_proceeds(ctx)
    }

    pub fn sweep_refund_vault(ctx: Context<SweepRefundVault>) -> Result<()> {
        instructions::sweep::sweep_refund_vault(ctx)
    }

    pub fn sweep_deposit_vault(ctx: Context<SweepDepositVault>) -> Result<()> {
        instructions::sweep::sweep_deposit_vault(ctx)
    }

    pub fn sweep_token_vault(ctx: Context<SweepTokenVault>) -> Result<()> {
        instructions::sweep::sweep_token_vault(ctx)
    }

    pub fn close_deposits(ctx: Context<CloseDeposits>) -> Result<()> {
        instructions::close_batch::close_deposits(ctx)
    }

    pub fn close_aggregates(ctx: Context<CloseAggregates>) -> Result<()> {
        instructions::close_batch::close_aggregates(ctx)
    }
}
