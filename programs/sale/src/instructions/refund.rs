//! `refund(index)` - the Operator refunds a single deposit before the pool is
//! fixed.

use anchor_lang::prelude::*;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::{Mint, Token, TokenAccount};

use crate::errors::SaleError;
use crate::events::Refunded;
use crate::phase::Phase;
use crate::seeds::{AGGREGATE_SEED, DEPOSIT_SEED, SALE_STATE_SEED, VAULT_AUTHORITY_SEED};
use crate::state::{Deposit, SaleState, WalletAggregate};
use crate::vault::transfer_from_vault;

// Data accounts are boxed: otherwise try_accounts overflows the SBF stack
// (>4096 bytes).
#[derive(Accounts)]
#[instruction(index: u64)]
#[event_cpi]
pub struct Refund<'info> {
    #[account(
        mut,
        seeds = [SALE_STATE_SEED],
        bump,
        has_one = operator @ SaleError::Unauthorized,
    )]
    pub sale_state: Box<Account<'info, SaleState>>,

    #[account(mut)]
    pub operator: Signer<'info>,

    /// CHECK: the address is bound by the deposit and aggregate seeds plus
    /// `has_one = wallet`; the wallet's owning program is irrelevant - it only
    /// receives the rent and owns its own USDC ATA.
    #[account(mut)]
    pub wallet: UncheckedAccount<'info>,

    #[account(
        mut,
        close = wallet,
        seeds = [DEPOSIT_SEED, wallet.key().as_ref(), index.to_le_bytes().as_ref()],
        bump,
        has_one = wallet,
    )]
    pub deposit: Box<Account<'info, Deposit>>,

    #[account(
        mut,
        seeds = [AGGREGATE_SEED, wallet.key().as_ref()],
        bump,
    )]
    pub aggregate: Box<Account<'info, WalletAggregate>>,

    #[account(
        init_if_needed,
        payer = operator,
        associated_token::mint = usdc_mint,
        associated_token::authority = wallet,
    )]
    pub wallet_usdc_ata: Box<Account<'info, TokenAccount>>,

    #[account(mut, address = sale_state.deposit_vault)]
    pub deposit_vault: Box<Account<'info, TokenAccount>>,

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

pub fn refund(ctx: Context<Refund>, index: u64) -> Result<()> {
    require!(
        !ctx.accounts.sale_state.pool_fixed,
        SaleError::PoolAlreadyFixed
    );
    let now = Clock::get()?.unix_timestamp;
    require!(
        matches!(
            ctx.accounts.sale_state.phase(now),
            Phase::Sale | Phase::SaleEnded
        ),
        SaleError::WrongPhase
    );

    let wallet = ctx.accounts.wallet.key();
    let amount = ctx.accounts.deposit.amount;
    let vault_authority_bump = ctx.accounts.sale_state.vault_authority_bump;

    let aggregate = &mut ctx.accounts.aggregate;
    aggregate.net_usdc_contributed = aggregate
        .net_usdc_contributed
        .checked_sub(amount)
        .ok_or(SaleError::MathOverflow)?;
    let new_net = aggregate.net_usdc_contributed;

    let state = &mut ctx.accounts.sale_state;
    state.deposits_usdc_contributed = state
        .deposits_usdc_contributed
        .checked_sub(amount)
        .ok_or(SaleError::MathOverflow)?;
    let new_total = state.deposits_usdc_contributed;

    transfer_from_vault(
        &ctx.accounts.token_program.to_account_info(),
        &ctx.accounts.deposit_vault.to_account_info(),
        &ctx.accounts.wallet_usdc_ata.to_account_info(),
        &ctx.accounts.vault_authority.to_account_info(),
        vault_authority_bump,
        amount,
    )?;

    emit_cpi!(Refunded {
        wallet,
        index,
        amount,
        net: new_net,
        total: new_total,
    });
    Ok(())
}
