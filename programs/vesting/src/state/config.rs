//! The `VestingConfig` singleton, the input guard constants, and the value
//! checks shared by several instructions. The config holds three roles: owner,
//! admin and operator.

use anchor_lang::prelude::*;

use crate::errors::VestingError;

pub const TOKEN_DECIMALS: u8 = 8;

pub const MAX_TGE_DELAY: i64 = 365 * 86400;

pub const MAX_LABEL_LEN: usize = 32;

pub const MAX_SCHEDULE_MONTHS: u32 = 120;

#[account]
#[derive(InitSpace)]
pub struct VestingConfig {
    pub owner: Pubkey,
    /// The owner candidate: `None` means no handover is in flight. The zero key
    /// cannot serve as that marker - `propose_owner` rejects it.
    pub pending_owner: Option<Pubkey>,
    pub admin: Pubkey,
    pub operator: Pubkey,
    pub token_mint: Pubkey,
    pub withdrawal_authority: Pubkey,
    pub tge_ts: i64,
    pub activity_count: u64,
    pub vault_authority_bump: u8,
}

impl VestingConfig {
    /// The TGE date: strictly in the future and no further out than
    /// `MAX_TGE_DELAY` from now. Called by `initialize` and `set_tge`.
    pub fn validate_tge(tge_ts: i64, now: i64) -> Result<()> {
        require!(tge_ts > now, VestingError::InvalidTge);
        let max_tge_ts = now
            .checked_add(MAX_TGE_DELAY)
            .ok_or(VestingError::MathOverflow)?;
        require!(tge_ts <= max_tge_ts, VestingError::InvalidTge);
        Ok(())
    }

    /// A role key is non-zero: nobody can sign with the zero key. Called by
    /// `initialize`, `set_admin`, `set_operator` and `propose_owner`.
    pub fn validate_role_key(key: Pubkey) -> Result<()> {
        require_keys_neq!(key, Pubkey::default(), VestingError::InvalidAuthority);
        Ok(())
    }

    /// An address the program pays out to: an activity recipient or a withdrawal
    /// target. It must be non-zero and must differ from the vault authority -
    /// under that address the funds would end up in the program's own custody.
    /// The zero address is currently rejected earlier by the slot type
    /// (`SystemAccount`); the check is kept in case that type changes. Called by
    /// `initialize`, `create_vesting`, `set_recipient` and
    /// `set_withdrawal_authority`.
    pub fn validate_payee(payee: Pubkey, vault_authority: Pubkey) -> Result<()> {
        require_keys_neq!(payee, Pubkey::default(), VestingError::InvalidAuthority);
        require_keys_neq!(payee, vault_authority, VestingError::InvalidAuthority);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
