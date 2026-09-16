//! `activate_claim` - Admin fixes the pool and creates the proceeds and refund
//! vaults.

use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};

use crate::errors::SaleError;
use crate::events::ClaimActivated;
use crate::math::{distributable, is_oversubscribed, soft_cap_usdc, TOKEN_UNITS};
use crate::phase::Phase;
use crate::seeds::{PROCEEDS_VAULT_SEED, REFUND_VAULT_SEED, SALE_STATE_SEED, VAULT_AUTHORITY_SEED};
use crate::state::SaleState;
use crate::vault::transfer_from_vault;

// Data accounts are boxed: otherwise try_accounts overflows the SBF stack
// (>4096 bytes).
#[derive(Accounts)]
#[event_cpi]
pub struct ActivateClaim<'info> {
    #[account(
        mut,
        seeds = [SALE_STATE_SEED],
        bump,
        has_one = admin @ SaleError::Unauthorized,
    )]
    pub sale_state: Box<Account<'info, SaleState>>,

    #[account(mut)]
    pub admin: Signer<'info>,

    /// CHECK: a data-less PDA authority; the address comes from the seeds, the
    /// bump from sale_state.
    #[account(seeds = [VAULT_AUTHORITY_SEED], bump = sale_state.vault_authority_bump)]
    pub vault_authority: UncheckedAccount<'info>,

    #[account(mut, address = sale_state.deposit_vault)]
    pub deposit_vault: Box<Account<'info, TokenAccount>>,

    #[account(address = sale_state.token_vault)]
    pub token_vault: Box<Account<'info, TokenAccount>>,

    #[account(
        init,
        seeds = [PROCEEDS_VAULT_SEED],
        bump,
        payer = admin,
        token::mint = usdc_mint,
        token::authority = vault_authority,
    )]
    pub proceeds_vault: Box<Account<'info, TokenAccount>>,

    /// Created only under oversubscription; the match is checked in the body.
    #[account(
        init,
        seeds = [REFUND_VAULT_SEED],
        bump,
        payer = admin,
        token::mint = usdc_mint,
        token::authority = vault_authority,
    )]
    pub refund_vault: Option<Box<Account<'info, TokenAccount>>>,

    #[account(address = sale_state.usdc_mint)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

pub fn activate_claim(ctx: Context<ActivateClaim>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;

    let emission = ctx.accounts.sale_state.emission;
    let price_usdc = ctx.accounts.sale_state.price_usdc;
    let pool = ctx.accounts.sale_state.deposits_usdc_contributed;
    let participants = ctx.accounts.sale_state.participants;
    let claim_start_date = ctx.accounts.sale_state.claim_start_date;
    let claim_duration = ctx.accounts.sale_state.claim_duration;
    let vault_authority_bump = ctx.accounts.sale_state.vault_authority_bump;

    // With pool_fixed && now < claim_start_date the phase is SaleEnded as well -
    // the phase alone is not enough.
    require!(
        ctx.accounts.sale_state.phase(now) == Phase::SaleEnded,
        SaleError::WrongPhase
    );
    require!(
        !ctx.accounts.sale_state.pool_fixed,
        SaleError::PoolAlreadyFixed
    );

    // The setters enforce the same predicate at the moment of the edit; the pool
    // is fixed later, so it is re-asserted against the real moment of the fix.
    require!(now < claim_start_date, SaleError::ClaimStartInPast);

    let oversubscribed = is_oversubscribed(pool, emission, price_usdc, TOKEN_UNITS)?;
    let dist = distributable(oversubscribed, pool, emission, price_usdc, TOKEN_UNITS)?;
    require!(
        ctx.accounts.token_vault.amount >= dist,
        SaleError::VaultNotFunded
    );
    require!(
        ctx.accounts.deposit_vault.amount >= pool,
        SaleError::DepositVaultUnderfunded
    );

    // Rounding at claim time can cost up to 1 minimal USDC unit per participant -
    // we subtract participants as headroom and keep it in refund_vault.
    let (proceeds, refund) = if oversubscribed {
        let proceeds =
            soft_cap_usdc(emission, price_usdc, TOKEN_UNITS)?.saturating_sub(participants);
        let refund = pool.checked_sub(proceeds).ok_or(SaleError::MathOverflow)?;
        (proceeds, refund)
    } else {
        (pool, 0)
    };

    require!(
        oversubscribed == ctx.accounts.refund_vault.is_some(),
        SaleError::RefundVaultMismatch
    );

    {
        let state = &mut ctx.accounts.sale_state;
        state.pool_fixed_amount = pool;
        state.pool_fixed = true;
    }

    transfer_from_vault(
        &ctx.accounts.token_program.to_account_info(),
        &ctx.accounts.deposit_vault.to_account_info(),
        &ctx.accounts.proceeds_vault.to_account_info(),
        &ctx.accounts.vault_authority.to_account_info(),
        vault_authority_bump,
        proceeds,
    )?;
    if let Some(refund_vault) = ctx.accounts.refund_vault.as_ref() {
        transfer_from_vault(
            &ctx.accounts.token_program.to_account_info(),
            &ctx.accounts.deposit_vault.to_account_info(),
            &refund_vault.to_account_info(),
            &ctx.accounts.vault_authority.to_account_info(),
            vault_authority_bump,
            refund,
        )?;
    }

    let claim_deadline = claim_start_date
        .checked_add(claim_duration)
        .ok_or(SaleError::MathOverflow)?;

    emit_cpi!(ClaimActivated {
        pool_fixed_amount: pool,
        distributable: dist,
        proceeds_vault_amount: proceeds,
        refund_vault_amount: refund,
        claim_start_date,
        claim_deadline,
        price_usdc,
        emission,
    });
    Ok(())
}
