//! `assert_funded` - the on-chain solvency check of an activity vault.

use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, TokenAccount};

use crate::errors::VestingError;
use crate::state::{Vesting, VestingConfig};

#[derive(Accounts)]
#[instruction(activity_id: u64)]
pub struct AssertFunded<'info> {
    // Data accounts are boxed: the try_accounts frame then fits the stack limit
    // for any number of accounts.
    #[account(seeds = [b"config"], bump)]
    pub config: Box<Account<'info, VestingConfig>>,

    #[account(
        seeds = [b"vesting", activity_id.to_le_bytes().as_ref()],
        bump,
    )]
    pub vesting: Box<Account<'info, Vesting>>,

    #[account(
        seeds = [b"vault", activity_id.to_le_bytes().as_ref()],
        bump,
        token::mint = token_mint,
        token::authority = vault_authority,
    )]
    pub vault: Box<Account<'info, TokenAccount>>,

    /// CHECK: the PDA that owns the vaults, no data; the address comes from the
    /// seeds and the bump from the config.
    #[account(seeds = [b"vault_authority"], bump = config.vault_authority_bump)]
    pub vault_authority: UncheckedAccount<'info>,

    #[account(address = config.token_mint)]
    pub token_mint: Box<Account<'info, Mint>>,
}

pub fn assert_funded(ctx: Context<AssertFunded>, _activity_id: u64) -> Result<()> {
    let vesting = &ctx.accounts.vesting;

    let covered = u128::from(ctx.accounts.vault.amount)
        .checked_add(u128::from(vesting.claimed))
        .ok_or(VestingError::MathOverflow)?;
    require!(
        covered >= u128::from(vesting.reference_amount),
        VestingError::InsufficientVaultBalance
    );
    Ok(())
}
