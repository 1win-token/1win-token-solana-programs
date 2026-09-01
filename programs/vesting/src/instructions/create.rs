//! `create_vesting` - the Owner creates an activity and its empty vault before
//! TGE.

use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};

use crate::errors::VestingError;
use crate::events::VestingCreated;
use crate::instructions::require_before_tge;
use crate::math;
use crate::state::{Vesting, VestingConfig, MAX_LABEL_LEN};

#[derive(Accounts)]
#[event_cpi]
pub struct CreateVesting<'info> {
    // Data accounts are boxed: the try_accounts frame then fits the stack limit
    // for any number of accounts.
    #[account(
        mut,
        seeds = [b"config"],
        bump,
        has_one = owner @ VestingError::NotOwner,
    )]
    pub config: Box<Account<'info, VestingConfig>>,

    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        init,
        seeds = [b"vesting", config.activity_count.to_le_bytes().as_ref()],
        bump,
        payer = owner,
        space = 8 + Vesting::INIT_SPACE,
    )]
    pub vesting: Box<Account<'info, Vesting>>,

    #[account(
        init,
        seeds = [b"vault", config.activity_count.to_le_bytes().as_ref()],
        bump,
        payer = owner,
        token::mint = token_mint,
        token::authority = vault_authority,
    )]
    pub vault: Box<Account<'info, TokenAccount>>,

    pub recipient: SystemAccount<'info>,

    /// CHECK: the PDA that owns the vaults, no data; the address comes from the
    /// seeds and the bump from the config.
    #[account(seeds = [b"vault_authority"], bump = config.vault_authority_bump)]
    pub vault_authority: UncheckedAccount<'info>,

    #[account(address = config.token_mint)]
    pub token_mint: Box<Account<'info, Mint>>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

pub fn create_vesting(
    ctx: Context<CreateVesting>,
    reference_amount: u64,
    tge_bps: u16,
    monthly_bps: u16,
    cliff_months: u16,
    label: String,
) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    require_before_tge(&ctx.accounts.config, now)?;

    let recipient = ctx.accounts.recipient.key();

    VestingConfig::validate_payee(recipient, ctx.accounts.vault_authority.key())?;
    require!(reference_amount > 0, VestingError::InvalidReferenceAmount);
    require!(label.len() <= MAX_LABEL_LEN, VestingError::InvalidParams);
    math::validate_schedule(tge_bps, monthly_bps, cliff_months)?;

    let activity_id = ctx.accounts.config.activity_count;

    let vesting = &mut ctx.accounts.vesting;
    vesting.recipient = recipient;
    vesting.reference_amount = reference_amount;
    vesting.claimed = 0;
    vesting.tge_bps = tge_bps;
    vesting.monthly_bps = monthly_bps;
    vesting.cliff_months = cliff_months;

    let config = &mut ctx.accounts.config;
    config.activity_count = activity_id
        .checked_add(1)
        .ok_or(VestingError::MathOverflow)?;

    emit_cpi!(VestingCreated {
        activity_id,
        recipient,
        reference_amount,
        tge_bps,
        monthly_bps,
        cliff_months,
        label,
    });
    Ok(())
}
