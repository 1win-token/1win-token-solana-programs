//! `initialize`: creates the `SaleState` singleton, the vaults and
//! `vault_authority`. The right to init is bound to the program's upgrade
//! authority.

use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};

use crate::errors::SaleError;
use crate::events::Initialized;
use crate::seeds::{DEPOSIT_VAULT_SEED, SALE_STATE_SEED, TOKEN_VAULT_SEED, VAULT_AUTHORITY_SEED};
use crate::state::{SaleState, TOKEN_DECIMALS, USDC_DECIMALS};

#[derive(Accounts)]
#[event_cpi]
pub struct Initialize<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(constraint = sale_program.programdata_address()? == Some(program_data.key()) @ SaleError::Unauthorized)]
    pub sale_program: Program<'info, crate::program::Sale>,

    #[account(constraint = program_data.upgrade_authority_address == Some(authority.key()) @ SaleError::Unauthorized)]
    pub program_data: Account<'info, ProgramData>,

    #[account(
        init,
        seeds = [SALE_STATE_SEED],
        bump,
        payer = authority,
        space = 8 + SaleState::INIT_SPACE,
    )]
    pub sale_state: Account<'info, SaleState>,

    /// CHECK: a data-less PDA authority; the address comes from the seeds and
    /// the bump is cached.
    #[account(seeds = [VAULT_AUTHORITY_SEED], bump)]
    pub vault_authority: UncheckedAccount<'info>,

    #[account(
        init,
        seeds = [DEPOSIT_VAULT_SEED],
        bump,
        payer = authority,
        token::mint = usdc_mint,
        token::authority = vault_authority,
    )]
    pub deposit_vault: Account<'info, TokenAccount>,

    #[account(
        init,
        seeds = [TOKEN_VAULT_SEED],
        bump,
        payer = authority,
        token::mint = token_mint,
        token::authority = vault_authority,
    )]
    pub token_vault: Account<'info, TokenAccount>,

    pub usdc_mint: Account<'info, Mint>,
    pub token_mint: Account<'info, Mint>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

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
    let now = Clock::get()?.unix_timestamp;

    SaleState::validate_sale_window(now, sale_start_date, sale_end_date)?;
    SaleState::validate_claim_start(now, sale_end_date, claim_start_date)?;
    SaleState::validate_claim_duration(claim_duration)?;

    require_keys_neq!(
        ctx.accounts.usdc_mint.key(),
        ctx.accounts.token_mint.key(),
        SaleError::InvalidMint
    );
    require!(
        ctx.accounts.token_mint.decimals == TOKEN_DECIMALS,
        SaleError::InvalidMintDecimals
    );
    require!(
        ctx.accounts.usdc_mint.decimals == USDC_DECIMALS,
        SaleError::InvalidMintDecimals
    );

    SaleState::validate_price(price_usdc)?;
    SaleState::validate_emission(emission)?;
    SaleState::validate_tx_limits(min_per_tx, max_per_tx)?;

    require_keys_neq!(owner, Pubkey::default(), SaleError::InvalidAuthority);
    require_keys_neq!(admin, Pubkey::default(), SaleError::InvalidAuthority);
    require_keys_neq!(operator, Pubkey::default(), SaleError::InvalidAuthority);

    let state = &mut ctx.accounts.sale_state;
    state.owner = owner;
    state.pending_owner = None;
    state.admin = admin;
    state.operator = operator;
    state.usdc_mint = ctx.accounts.usdc_mint.key();
    state.token_mint = ctx.accounts.token_mint.key();
    state.deposit_vault = ctx.accounts.deposit_vault.key();
    state.token_vault = ctx.accounts.token_vault.key();
    state.price_usdc = price_usdc;
    state.emission = emission;
    state.min_per_tx = min_per_tx;
    state.max_per_tx = max_per_tx;
    state.deposits_usdc_contributed = 0;
    state.pool_fixed_amount = 0;
    state.participants = 0;
    state.sale_start_date = sale_start_date;
    state.sale_end_date = sale_end_date;
    state.claim_start_date = claim_start_date;
    state.claim_duration = claim_duration;
    state.pool_fixed = false;
    state.paused = false;
    state.vault_authority_bump = ctx.bumps.vault_authority;

    emit_cpi!(Initialized {
        owner,
        admin,
        operator,
        usdc_mint: state.usdc_mint,
        token_mint: state.token_mint,
        price_usdc,
        emission,
        min_per_tx,
        max_per_tx,
        sale_start_date,
        sale_end_date,
        claim_start_date,
        claim_duration,
    });

    Ok(())
}
