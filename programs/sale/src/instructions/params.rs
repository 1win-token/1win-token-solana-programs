//! Parameter instructions - available to Admin.

use anchor_lang::prelude::*;

use crate::errors::SaleError;
use crate::events::{
    ClaimScheduleUpdated, EmissionUpdated, PriceUpdated, SaleScheduleUpdated, TxLimitsUpdated,
};
use crate::phase::Phase;
use crate::seeds::SALE_STATE_SEED;
use crate::state::SaleState;

/// The shared context of every Admin instruction.
#[derive(Accounts)]
#[event_cpi]
pub struct AdminUpdate<'info> {
    #[account(
        mut,
        seeds = [SALE_STATE_SEED],
        bump,
        has_one = admin @ SaleError::Unauthorized,
    )]
    pub sale_state: Account<'info, SaleState>,
    pub admin: Signer<'info>,
}

pub fn set_price(ctx: Context<AdminUpdate>, price_usdc: u64) -> Result<()> {
    let state = &mut ctx.accounts.sale_state;

    state.require_pre_sale()?;
    SaleState::validate_price(price_usdc)?;

    state.price_usdc = price_usdc;

    emit_cpi!(PriceUpdated { price_usdc });
    Ok(())
}

pub fn set_emission(ctx: Context<AdminUpdate>, emission: u64) -> Result<()> {
    let state = &mut ctx.accounts.sale_state;

    state.require_pre_sale()?;
    SaleState::validate_emission(emission)?;

    state.emission = emission;

    emit_cpi!(EmissionUpdated { emission });
    Ok(())
}

pub fn set_tx_limits(ctx: Context<AdminUpdate>, min_per_tx: u64, max_per_tx: u64) -> Result<()> {
    let state = &mut ctx.accounts.sale_state;

    state.require_pre_sale()?;
    SaleState::validate_tx_limits(min_per_tx, max_per_tx)?;

    state.min_per_tx = min_per_tx;
    state.max_per_tx = max_per_tx;

    emit_cpi!(TxLimitsUpdated {
        min_per_tx,
        max_per_tx,
    });
    Ok(())
}

pub fn set_sale_schedule(
    ctx: Context<AdminUpdate>,
    sale_start_date: i64,
    sale_end_date: i64,
) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let state = &mut ctx.accounts.sale_state;

    state.require_pre_sale()?;
    SaleState::validate_sale_window(now, sale_start_date, sale_end_date)?;
    SaleState::validate_claim_start(now, sale_end_date, state.claim_start_date)?;

    state.sale_start_date = sale_start_date;
    state.sale_end_date = sale_end_date;

    emit_cpi!(SaleScheduleUpdated {
        sale_start_date,
        sale_end_date,
    });
    Ok(())
}

pub fn set_claim_schedule(
    ctx: Context<AdminUpdate>,
    claim_start_date: i64,
    claim_duration: i64,
) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let state = &mut ctx.accounts.sale_state;

    require!(
        matches!(
            state.phase(now),
            Phase::PreSale | Phase::Sale | Phase::SaleEnded
        ),
        SaleError::WrongPhase
    );
    require!(!state.pool_fixed, SaleError::PoolAlreadyFixed);
    SaleState::validate_claim_start(now, state.sale_end_date, claim_start_date)?;
    SaleState::validate_claim_duration(claim_duration)?;

    state.claim_start_date = claim_start_date;
    state.claim_duration = claim_duration;

    emit_cpi!(ClaimScheduleUpdated {
        claim_start_date,
        claim_duration,
    });
    Ok(())
}
