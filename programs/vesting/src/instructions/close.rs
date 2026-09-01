//! `cancel_vesting` (the Owner cancels a mistaken activity before TGE) and
//! `close_vesting` (closes a fully drawn-down one): a shared mechanism for
//! withdrawing the remainder and closing the account.

use anchor_lang::prelude::*;
use anchor_spl::token::{self, CloseAccount, Mint, Token, TokenAccount, Transfer};

use crate::errors::VestingError;
use crate::events::{VestingCancelled, VestingClosed};
use crate::instructions::require_before_tge;
use crate::state::{Vesting, VestingConfig};

#[derive(Accounts)]
#[instruction(activity_id: u64)]
#[event_cpi]
pub struct CloseActivity<'info> {
    // Data accounts are boxed: the try_accounts frame then fits the stack limit
    // for any number of accounts.
    #[account(
        seeds = [b"config"],
        bump,
        has_one = owner @ VestingError::NotOwner,
    )]
    pub config: Box<Account<'info, VestingConfig>>,

    pub owner: Signer<'info>,

    #[account(
        mut,
        close = rent_collector,
        seeds = [b"vesting", activity_id.to_le_bytes().as_ref()],
        bump,
    )]
    pub vesting: Box<Account<'info, Vesting>>,

    #[account(
        mut,
        seeds = [b"vault", activity_id.to_le_bytes().as_ref()],
        bump,
        token::mint = token_mint,
        token::authority = vault_authority,
    )]
    pub vault: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = beneficiary_token.mint == config.token_mint @ VestingError::InvalidMint,
        constraint = beneficiary_token.owner == config.withdrawal_authority @ VestingError::InvalidBeneficiary,
        constraint = beneficiary_token.key() != vault.key() @ VestingError::InvalidBeneficiary,
    )]
    pub beneficiary_token: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        address = config.withdrawal_authority @ VestingError::InvalidRentCollector,
    )]
    pub rent_collector: SystemAccount<'info>,

    /// CHECK: the PDA that owns the vaults, no data; the address comes from the
    /// seeds and the bump from the config.
    #[account(seeds = [b"vault_authority"], bump = config.vault_authority_bump)]
    pub vault_authority: UncheckedAccount<'info>,

    #[account(address = config.token_mint)]
    pub token_mint: Box<Account<'info, Mint>>,

    pub token_program: Program<'info, Token>,
}

/// The order is mandatory: SPL Token closes a token account only at a zero
/// balance.
fn drain_and_close(ctx: &Context<CloseActivity>) -> Result<()> {
    let bump = ctx.accounts.config.vault_authority_bump;
    let signer: &[&[&[u8]]] = &[&[b"vault_authority", &[bump]]];
    let amount = ctx.accounts.vault.amount;
    if amount > 0 {
        token::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.key(),
                Transfer {
                    from: ctx.accounts.vault.to_account_info(),
                    to: ctx.accounts.beneficiary_token.to_account_info(),
                    authority: ctx.accounts.vault_authority.to_account_info(),
                },
                signer,
            ),
            amount,
        )?;
    }
    token::close_account(CpiContext::new_with_signer(
        ctx.accounts.token_program.key(),
        CloseAccount {
            account: ctx.accounts.vault.to_account_info(),
            destination: ctx.accounts.rent_collector.to_account_info(),
            authority: ctx.accounts.vault_authority.to_account_info(),
        },
        signer,
    ))?;
    Ok(())
}

pub fn cancel_vesting(ctx: Context<CloseActivity>, activity_id: u64) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    require_before_tge(&ctx.accounts.config, now)?;
    // Before TGE claimed is always 0 (claiming is possible only after TGE). The
    // check guards against that ceasing to hold after future code changes.
    require!(
        ctx.accounts.vesting.claimed == 0,
        VestingError::ScheduleStarted
    );

    let amount = ctx.accounts.vault.amount;
    let beneficiary_token = ctx.accounts.beneficiary_token.key();
    let rent_collector = ctx.accounts.rent_collector.key();

    drain_and_close(&ctx)?;

    emit_cpi!(VestingCancelled {
        activity_id,
        amount,
        beneficiary_token,
        rent_collector,
    });
    Ok(())
}

pub fn close_vesting(ctx: Context<CloseActivity>, activity_id: u64) -> Result<()> {
    // The absence of a time gate is deliberate: `claimed >= reference_amount` is
    // reachable only after TGE, and a pre-TGE condition would shut off the
    // regular close path entirely.
    require!(
        ctx.accounts.vesting.claimed >= ctx.accounts.vesting.reference_amount,
        VestingError::NotFullyClaimed
    );

    let amount = ctx.accounts.vault.amount;
    let beneficiary_token = ctx.accounts.beneficiary_token.key();
    let rent_collector = ctx.accounts.rent_collector.key();

    drain_and_close(&ctx)?;

    emit_cpi!(VestingClosed {
        activity_id,
        amount,
        beneficiary_token,
        rent_collector,
    });
    Ok(())
}
