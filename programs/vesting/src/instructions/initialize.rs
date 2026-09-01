//! `initialize`: creates the `VestingConfig` singleton. The right to init is
//! bound to the program's upgrade authority.

use anchor_lang::prelude::*;
use anchor_spl::token::Mint;

use crate::errors::VestingError;
use crate::events::Initialized;
use crate::state::{VestingConfig, TOKEN_DECIMALS};

#[derive(Accounts)]
#[event_cpi]
pub struct Initialize<'info> {
    // Data accounts are boxed: the try_accounts frame then fits the stack limit
    // for any number of accounts.
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(constraint = vesting_program.programdata_address()? == Some(program_data.key()) @ VestingError::NotUpgradeAuthority)]
    pub vesting_program: Program<'info, crate::program::Vesting>,

    #[account(constraint = program_data.upgrade_authority_address == Some(authority.key()) @ VestingError::NotUpgradeAuthority)]
    pub program_data: Box<Account<'info, ProgramData>>,

    #[account(
        init,
        seeds = [b"config"],
        bump,
        payer = authority,
        space = 8 + VestingConfig::INIT_SPACE,
    )]
    pub config: Box<Account<'info, VestingConfig>>,

    pub withdrawal_authority: SystemAccount<'info>,

    /// CHECK: the PDA that owns the vaults, no data; declared for the sake of the
    /// canonical bump (Arch §6.3).
    #[account(seeds = [b"vault_authority"], bump)]
    pub vault_authority: UncheckedAccount<'info>,

    pub token_mint: Box<Account<'info, Mint>>,

    pub system_program: Program<'info, System>,
}

pub fn initialize(
    ctx: Context<Initialize>,
    owner: Pubkey,
    admin: Pubkey,
    operator: Pubkey,
    tge_ts: i64,
) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;

    VestingConfig::validate_tge(tge_ts, now)?;

    require!(
        ctx.accounts.token_mint.decimals == TOKEN_DECIMALS,
        VestingError::InvalidMintDecimals
    );

    VestingConfig::validate_role_key(owner)?;
    VestingConfig::validate_role_key(admin)?;
    VestingConfig::validate_role_key(operator)?;

    let withdrawal_authority = ctx.accounts.withdrawal_authority.key();
    VestingConfig::validate_payee(withdrawal_authority, ctx.accounts.vault_authority.key())?;

    let token_mint = ctx.accounts.token_mint.key();

    let config = &mut ctx.accounts.config;
    config.owner = owner;
    config.pending_owner = None;
    config.admin = admin;
    config.operator = operator;
    config.token_mint = token_mint;
    config.withdrawal_authority = withdrawal_authority;
    config.tge_ts = tge_ts;
    config.activity_count = 0;
    config.vault_authority_bump = ctx.bumps.vault_authority;

    emit_cpi!(Initialized {
        owner,
        admin,
        operator,
        token_mint,
        withdrawal_authority,
        tge_ts,
    });

    Ok(())
}
