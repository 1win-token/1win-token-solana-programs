//! `withdraw_proceeds` - the Owner withdraws all proceeds and closes
//! `proceeds_vault`.

use anchor_lang::prelude::*;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::{Mint, Token, TokenAccount};

use crate::errors::SaleError;
use crate::events::ProceedsWithdrawn;
use crate::seeds::{PROCEEDS_VAULT_SEED, SALE_STATE_SEED, VAULT_AUTHORITY_SEED};
use crate::state::SaleState;
use crate::vault::{close_vault, transfer_from_vault};

// Data accounts are boxed: otherwise try_accounts overflows the SBF stack
// (>4096 bytes).
#[derive(Accounts)]
#[event_cpi]
pub struct WithdrawProceeds<'info> {
    #[account(
        seeds = [SALE_STATE_SEED],
        bump,
        has_one = owner @ SaleError::Unauthorized,
    )]
    pub sale_state: Box<Account<'info, SaleState>>,

    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        mut,
        seeds = [PROCEEDS_VAULT_SEED],
        bump,
        token::mint = usdc_mint,
        token::authority = vault_authority,
    )]
    pub proceeds_vault: Box<Account<'info, TokenAccount>>,

    #[account(
        init_if_needed,
        payer = owner,
        associated_token::mint = usdc_mint,
        associated_token::authority = owner,
    )]
    pub owner_usdc_ata: Box<Account<'info, TokenAccount>>,

    /// CHECK: the rent recipient of the closed vault; the address is supplied by
    /// the trusted Owner (§5.5).
    #[account(mut)]
    pub rent_collector: UncheckedAccount<'info>,

    /// CHECK: a data-less PDA authority; the address comes from the seeds, the
    /// bump from sale_state.
    #[account(seeds = [VAULT_AUTHORITY_SEED], bump = sale_state.vault_authority_bump)]
    pub vault_authority: UncheckedAccount<'info>,

    #[account(address = sale_state.usdc_mint)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

pub fn withdraw_proceeds(ctx: Context<WithdrawProceeds>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;

    require!(ctx.accounts.sale_state.pool_fixed, SaleError::PoolNotFixed);
    require!(
        now >= ctx.accounts.sale_state.claim_start_date,
        SaleError::ClaimNotStarted
    );

    let amount = ctx.accounts.proceeds_vault.amount;
    let vault_authority_bump = ctx.accounts.sale_state.vault_authority_bump;

    if amount > 0 {
        transfer_from_vault(
            &ctx.accounts.token_program.to_account_info(),
            &ctx.accounts.proceeds_vault.to_account_info(),
            &ctx.accounts.owner_usdc_ata.to_account_info(),
            &ctx.accounts.vault_authority.to_account_info(),
            vault_authority_bump,
            amount,
        )?;
    }

    close_vault(
        &ctx.accounts.token_program.to_account_info(),
        &ctx.accounts.proceeds_vault.to_account_info(),
        &ctx.accounts.rent_collector.to_account_info(),
        &ctx.accounts.vault_authority.to_account_info(),
        vault_authority_bump,
    )?;

    emit_cpi!(ProceedsWithdrawn { amount });
    Ok(())
}
