//! `claim` - the wallet owner receives $1WIN and the USDC refund; the aggregate
//! is closed.

use anchor_lang::prelude::*;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::{Mint, Token, TokenAccount};

use crate::errors::SaleError;
use crate::events::Claimed;
use crate::math::{allocate, Allocation, TOKEN_UNITS};
use crate::phase::Phase;
use crate::seeds::{AGGREGATE_SEED, REFUND_VAULT_SEED, SALE_STATE_SEED, VAULT_AUTHORITY_SEED};
use crate::state::{SaleState, WalletAggregate};
use crate::vault::transfer_from_vault;

// Data accounts are boxed: otherwise try_accounts overflows the SBF stack
// (>4096 bytes).
#[derive(Accounts)]
#[event_cpi]
pub struct Claim<'info> {
    #[account(seeds = [SALE_STATE_SEED], bump)]
    pub sale_state: Box<Account<'info, SaleState>>,

    /// The contribution owner: the signer, and the recipient of the tokens, the
    /// refund and the aggregate's rent.
    #[account(mut)]
    pub wallet: Signer<'info>,

    #[account(
        mut,
        close = wallet,
        seeds = [AGGREGATE_SEED, wallet.key().as_ref()],
        bump,
        has_one = wallet,
    )]
    pub aggregate: Box<Account<'info, WalletAggregate>>,

    #[account(mut, address = sale_state.token_vault)]
    pub token_vault: Box<Account<'info, TokenAccount>>,

    #[account(
        init_if_needed,
        payer = wallet,
        associated_token::mint = token_mint,
        associated_token::authority = wallet,
    )]
    pub wallet_token_ata: Box<Account<'info, TokenAccount>>,

    /// The refund vault: it exists only under oversubscription and is passed if
    /// and only if the refund is > 0. Not created here (it was created on
    /// activation); bound by seeds plus `token::mint` / `token::authority`.
    #[account(
        mut,
        seeds = [REFUND_VAULT_SEED],
        bump,
        token::mint = usdc_mint,
        token::authority = vault_authority,
    )]
    pub refund_vault: Option<Box<Account<'info, TokenAccount>>>,

    /// The wallet's USDC ATA - the refund recipient; passed when the refund is > 0.
    #[account(
        init_if_needed,
        payer = wallet,
        associated_token::mint = usdc_mint,
        associated_token::authority = wallet,
    )]
    pub wallet_usdc_ata: Option<Box<Account<'info, TokenAccount>>>,

    /// CHECK: a data-less PDA authority; the address comes from the seeds, the
    /// bump from sale_state.
    #[account(seeds = [VAULT_AUTHORITY_SEED], bump = sale_state.vault_authority_bump)]
    pub vault_authority: UncheckedAccount<'info>,

    #[account(address = sale_state.token_mint)]
    pub token_mint: Box<Account<'info, Mint>>,

    #[account(address = sale_state.usdc_mint)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

pub fn claim(ctx: Context<Claim>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    // A single source of the phase: ClaimClosed (the deadline has passed) is
    // separated from the other non-Claim phases (pool not fixed / claim not
    // started yet) by its own error code.
    match ctx.accounts.sale_state.phase(now) {
        Phase::Claim => {}
        Phase::ClaimClosed => return Err(SaleError::ClaimDeadlinePassed.into()),
        _ => return Err(SaleError::WrongPhase.into()),
    }

    let net = ctx.accounts.aggregate.net_usdc_contributed;
    require!(net > 0, SaleError::NothingToClaim);

    let wallet = ctx.accounts.wallet.key();
    let emission = ctx.accounts.sale_state.emission;
    let price_usdc = ctx.accounts.sale_state.price_usdc;
    let pool = ctx.accounts.sale_state.pool_fixed_amount;
    let vault_authority_bump = ctx.accounts.sale_state.vault_authority_bump;

    // Allocation out of the fixed pool (§10). The decision is made on the
    // pre-CPI state.
    let Allocation { tokens, refund } = allocate(net, pool, emission, price_usdc, TOKEN_UNITS)?;

    // 1. Tokens (rounded down; a transfer of 0 is allowed under extreme
    //    oversubscription).
    transfer_from_vault(
        &ctx.accounts.token_program.to_account_info(),
        &ctx.accounts.token_vault.to_account_info(),
        &ctx.accounts.wallet_token_ata.to_account_info(),
        &ctx.accounts.vault_authority.to_account_info(),
        vault_authority_bump,
        tokens,
    )?;

    // 2. The USDC refund: both accounts are mandatory when the refund is > 0
    //    (fail-closed).
    if refund > 0 {
        let refund_vault = ctx
            .accounts
            .refund_vault
            .as_ref()
            .ok_or(SaleError::RefundVaultMissing)?;
        let wallet_usdc_ata = ctx
            .accounts
            .wallet_usdc_ata
            .as_ref()
            .ok_or(SaleError::RefundAtaMissing)?;
        transfer_from_vault(
            &ctx.accounts.token_program.to_account_info(),
            &refund_vault.to_account_info(),
            &wallet_usdc_ata.to_account_info(),
            &ctx.accounts.vault_authority.to_account_info(),
            vault_authority_bump,
            refund,
        )?;
    }

    // 3. The aggregate is closed unconditionally (`close = wallet`) - the only
    //    replay guard (I5): a repeated claim will not find the aggregate and
    //    fails with AccountNotInitialized.
    emit_cpi!(Claimed {
        wallet,
        net,
        tokens,
        refund,
    });
    Ok(())
}
