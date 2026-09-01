//! Vault CPIs signed by `vault_authority`. The signature seed bytes are built
//! here from the bump that is passed in - callers never assemble them.

use anchor_lang::prelude::*;
use anchor_spl::token::{self, CloseAccount, Transfer};

use crate::seeds::VAULT_AUTHORITY_SEED;

/// Transfers tokens out of a vault; `amount` is passed through as is, zero is
/// allowed.
pub(crate) fn transfer_from_vault<'info>(
    token_program: &AccountInfo<'info>,
    from: &AccountInfo<'info>,
    to: &AccountInfo<'info>,
    vault_authority: &AccountInfo<'info>,
    vault_authority_bump: u8,
    amount: u64,
) -> Result<()> {
    let signer_seeds: &[&[&[u8]]] = &[&[VAULT_AUTHORITY_SEED, &[vault_authority_bump]]];
    token::transfer(
        CpiContext::new_with_signer(
            token_program.key(),
            Transfer {
                from: from.clone(),
                to: to.clone(),
                authority: vault_authority.clone(),
            },
            signer_seeds,
        ),
        amount,
    )
}

/// Closes a vault, returning the rent to `rent_collector`.
pub(crate) fn close_vault<'info>(
    token_program: &AccountInfo<'info>,
    vault: &AccountInfo<'info>,
    rent_collector: &AccountInfo<'info>,
    vault_authority: &AccountInfo<'info>,
    vault_authority_bump: u8,
) -> Result<()> {
    let signer_seeds: &[&[&[u8]]] = &[&[VAULT_AUTHORITY_SEED, &[vault_authority_bump]]];
    token::close_account(CpiContext::new_with_signer(
        token_program.key(),
        CloseAccount {
            account: vault.clone(),
            destination: rent_collector.clone(),
            authority: vault_authority.clone(),
        },
        signer_seeds,
    ))
}
