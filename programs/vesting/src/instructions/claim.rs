//! `claim` - hands the unlocked $1WIN to the activity recipient (Arch §9.8).

use anchor_lang::prelude::*;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::{self, Mint, Token, TokenAccount, Transfer};

use crate::errors::VestingError;
use crate::events::Claimed;
use crate::math;
use crate::state::{Vesting, VestingConfig};

#[derive(Accounts)]
#[instruction(activity_id: u64)]
#[event_cpi]
pub struct Claim<'info> {
    // Data accounts are boxed: the try_accounts frame then fits the stack limit
    // for any number of accounts.
    #[account(seeds = [b"config"], bump)]
    pub config: Box<Account<'info, VestingConfig>>,

    #[account(
        mut,
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

    #[account(address = vesting.recipient)]
    pub recipient: SystemAccount<'info>,

    #[account(
        init_if_needed,
        payer = signer,
        associated_token::mint = token_mint,
        associated_token::authority = recipient,
        associated_token::token_program = token_program,
    )]
    pub recipient_ata: Box<Account<'info, TokenAccount>>,

    /// CHECK: the PDA that owns the vaults, no data; the address comes from the
    /// seeds and the bump from the config (Arch §6.3).
    #[account(seeds = [b"vault_authority"], bump = config.vault_authority_bump)]
    pub vault_authority: UncheckedAccount<'info>,

    #[account(address = config.token_mint)]
    pub token_mint: Box<Account<'info, Mint>>,

    #[account(mut)]
    pub signer: Signer<'info>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

pub fn claim(ctx: Context<Claim>, activity_id: u64) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;

    let recipient = ctx.accounts.vesting.recipient;
    let signer = ctx.accounts.signer.key();
    require!(
        signer == recipient || signer == ctx.accounts.config.operator,
        VestingError::NotAuthorized
    );

    let tge_ts = ctx.accounts.config.tge_ts;

    require!(tge_ts > 0, VestingError::ScheduleNotStarted);
    require!(now >= tge_ts, VestingError::ScheduleNotStarted);

    let claimed = ctx.accounts.vesting.claimed;
    let vested = math::vested(now, tge_ts, &ctx.accounts.vesting.schedule())?;
    let amount = vested
        .checked_sub(claimed)
        .ok_or(VestingError::MathOverflow)?;

    require!(amount > 0, VestingError::NothingToClaim);
    require!(
        ctx.accounts.vault.amount >= amount,
        VestingError::InsufficientVaultBalance
    );

    let vault_authority_bump = ctx.accounts.config.vault_authority_bump;
    let signer_seeds: &[&[&[u8]]] = &[&[b"vault_authority", &[vault_authority_bump]]];
    token::transfer(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            Transfer {
                from: ctx.accounts.vault.to_account_info(),
                to: ctx.accounts.recipient_ata.to_account_info(),
                authority: ctx.accounts.vault_authority.to_account_info(),
            },
            signer_seeds,
        ),
        amount,
    )?;

    let new_claimed = claimed
        .checked_add(amount)
        .ok_or(VestingError::MathOverflow)?;
    ctx.accounts.vesting.claimed = new_claimed;

    emit_cpi!(Claimed {
        activity_id,
        recipient,
        amount,
        claimed: new_claimed,
    });
    Ok(())
}
