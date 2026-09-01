//! `sweep_*` - the Owner withdraws the remainder of a single vault and closes it
//! (ClaimClosed).

use anchor_lang::prelude::*;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::{Mint, Token, TokenAccount};

use crate::errors::SaleError;
use crate::events::Swept;
use crate::seeds::{REFUND_VAULT_SEED, SALE_STATE_SEED, VAULT_AUTHORITY_SEED};
use crate::state::SaleState;
use crate::vault::{close_vault, transfer_from_vault};

// Data accounts are boxed: otherwise try_accounts overflows the SBF stack
// (>4096 bytes).
#[derive(Accounts)]
#[event_cpi]
pub struct SweepRefundVault<'info> {
    #[account(
        seeds = [SALE_STATE_SEED],
        bump,
        has_one = owner @ SaleError::Unauthorized,
    )]
    pub sale_state: Box<Account<'info, SaleState>>,

    #[account(mut)]
    pub owner: Signer<'info>,

    /// The refund vault: it exists only under oversubscription; when
    /// undersubscribed it was never created → `AccountNotInitialized`.
    #[account(
        mut,
        seeds = [REFUND_VAULT_SEED],
        bump,
        token::mint = usdc_mint,
        token::authority = vault_authority,
    )]
    pub refund_vault: Box<Account<'info, TokenAccount>>,

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

#[derive(Accounts)]
#[event_cpi]
pub struct SweepDepositVault<'info> {
    #[account(
        seeds = [SALE_STATE_SEED],
        bump,
        has_one = owner @ SaleError::Unauthorized,
    )]
    pub sale_state: Box<Account<'info, SaleState>>,

    #[account(mut)]
    pub owner: Signer<'info>,

    /// Stray transfers and donations: bookkeeping runs on
    /// `deposits_usdc_contributed`, not on the balance.
    #[account(mut, address = sale_state.deposit_vault)]
    pub deposit_vault: Box<Account<'info, TokenAccount>>,

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

#[derive(Accounts)]
#[event_cpi]
pub struct SweepTokenVault<'info> {
    #[account(
        seeds = [SALE_STATE_SEED],
        bump,
        has_one = owner @ SaleError::Unauthorized,
    )]
    pub sale_state: Box<Account<'info, SaleState>>,

    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(mut, address = sale_state.token_vault)]
    pub token_vault: Box<Account<'info, TokenAccount>>,

    #[account(
        init_if_needed,
        payer = owner,
        associated_token::mint = token_mint,
        associated_token::authority = owner,
    )]
    pub owner_token_ata: Box<Account<'info, TokenAccount>>,

    /// CHECK: the rent recipient of the closed vault; the address is supplied by
    /// the trusted Owner (§5.5).
    #[account(mut)]
    pub rent_collector: UncheckedAccount<'info>,

    /// CHECK: a data-less PDA authority; the address comes from the seeds, the
    /// bump from sale_state.
    #[account(seeds = [VAULT_AUTHORITY_SEED], bump = sale_state.vault_authority_bump)]
    pub vault_authority: UncheckedAccount<'info>,

    #[account(address = sale_state.token_mint)]
    pub token_mint: Box<Account<'info, Mint>>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

fn sweep_one<'info>(
    vault: &Account<'info, TokenAccount>,
    destination: &AccountInfo<'info>,
    authority: &AccountInfo<'info>,
    rent_collector: &AccountInfo<'info>,
    token_program: &AccountInfo<'info>,
    vault_authority_bump: u8,
) -> Result<u64> {
    let amount = vault.amount;
    if amount > 0 {
        transfer_from_vault(
            token_program,
            &vault.to_account_info(),
            destination,
            authority,
            vault_authority_bump,
            amount,
        )?;
    }
    close_vault(
        token_program,
        &vault.to_account_info(),
        rent_collector,
        authority,
        vault_authority_bump,
    )?;
    Ok(amount)
}

pub fn sweep_refund_vault(ctx: Context<SweepRefundVault>) -> Result<()> {
    ctx.accounts.sale_state.require_claim_closed()?;
    let bump = ctx.accounts.sale_state.vault_authority_bump;
    let vault = ctx.accounts.refund_vault.key();
    let amount = sweep_one(
        &ctx.accounts.refund_vault,
        &ctx.accounts.owner_usdc_ata.to_account_info(),
        &ctx.accounts.vault_authority.to_account_info(),
        &ctx.accounts.rent_collector.to_account_info(),
        &ctx.accounts.token_program.to_account_info(),
        bump,
    )?;
    emit_cpi!(Swept { vault, amount });
    Ok(())
}

pub fn sweep_deposit_vault(ctx: Context<SweepDepositVault>) -> Result<()> {
    ctx.accounts.sale_state.require_claim_closed()?;
    let bump = ctx.accounts.sale_state.vault_authority_bump;
    let vault = ctx.accounts.deposit_vault.key();
    let amount = sweep_one(
        &ctx.accounts.deposit_vault,
        &ctx.accounts.owner_usdc_ata.to_account_info(),
        &ctx.accounts.vault_authority.to_account_info(),
        &ctx.accounts.rent_collector.to_account_info(),
        &ctx.accounts.token_program.to_account_info(),
        bump,
    )?;
    emit_cpi!(Swept { vault, amount });
    Ok(())
}

pub fn sweep_token_vault(ctx: Context<SweepTokenVault>) -> Result<()> {
    ctx.accounts.sale_state.require_claim_closed()?;
    let bump = ctx.accounts.sale_state.vault_authority_bump;
    let vault = ctx.accounts.token_vault.key();
    let amount = sweep_one(
        &ctx.accounts.token_vault,
        &ctx.accounts.owner_token_ata.to_account_info(),
        &ctx.accounts.vault_authority.to_account_info(),
        &ctx.accounts.rent_collector.to_account_info(),
        &ctx.accounts.token_program.to_account_info(),
        bump,
    )?;
    emit_cpi!(Swept { vault, amount });
    Ok(())
}
