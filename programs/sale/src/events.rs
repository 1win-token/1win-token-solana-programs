//! Program events (`#[event_cpi]` + `emit_cpi!`).

use anchor_lang::prelude::*;

#[event]
pub struct Initialized {
    pub owner: Pubkey,
    pub admin: Pubkey,
    pub operator: Pubkey,
    pub usdc_mint: Pubkey,
    pub token_mint: Pubkey,
    pub price_usdc: u64,
    pub emission: u64,
    pub min_per_tx: u64,
    pub max_per_tx: u64,
    pub sale_start_date: i64,
    pub sale_end_date: i64,
    pub claim_duration: i64,
    pub claim_start_date: i64,
}

#[event]
pub struct PriceUpdated {
    pub price_usdc: u64,
}

#[event]
pub struct EmissionUpdated {
    pub emission: u64,
}

#[event]
pub struct TxLimitsUpdated {
    pub min_per_tx: u64,
    pub max_per_tx: u64,
}

#[event]
pub struct SaleScheduleUpdated {
    pub sale_start_date: i64,
    pub sale_end_date: i64,
}

#[event]
pub struct ClaimScheduleUpdated {
    pub claim_start_date: i64,
    pub claim_duration: i64,
}

#[event]
pub struct OwnerProposed {
    pub pending_owner: Pubkey,
}

#[event]
pub struct OwnerProposalCancelled {}

#[event]
pub struct RoleUpdated {
    pub role: RoleKind,
    pub new_pubkey: Pubkey,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub enum RoleKind {
    Owner,
    Admin,
    Operator,
}

#[event]
pub struct Paused {}

#[event]
pub struct Unpaused {}

#[event]
pub struct Blacklisted {
    pub wallet: Pubkey,
}

#[event]
pub struct BlacklistRemoved {
    pub wallet: Pubkey,
}

#[event]
pub struct GraylistSet {
    pub wallet: Pubkey,
    pub min_per_tx: u64,
    pub max_per_tx: u64,
    pub max_total: u64,
}

#[event]
pub struct GraylistRemoved {
    pub wallet: Pubkey,
}

#[event]
pub struct AggregateInitialized {
    pub wallet: Pubkey,
}

#[event]
pub struct Purchased {
    pub wallet: Pubkey,
    pub index: u64,
    pub amount: u64,
    pub net: u64,
    pub total: u64,
}

#[event]
pub struct Refunded {
    pub wallet: Pubkey,
    pub index: u64,
    pub amount: u64,
    pub net: u64,
    pub total: u64,
}

#[event]
pub struct ClaimActivated {
    pub pool_fixed_amount: u64,
    pub distributable: u64,
    pub proceeds_vault_amount: u64,
    pub refund_vault_amount: u64,
    pub claim_start_date: i64,
    pub claim_deadline: i64,
    pub price_usdc: u64,
    pub emission: u64,
}

#[event]
pub struct Claimed {
    pub wallet: Pubkey,
    pub net: u64,
    pub tokens: u64,
    pub refund: u64,
}

#[event]
pub struct ProceedsWithdrawn {
    pub amount: u64,
}

/// `vault` - the address of the swept vault (`refund` / `deposit` / `token`).
#[event]
pub struct Swept {
    pub vault: Pubkey,
    pub amount: u64,
}

#[event]
pub struct AccountsClosed {
    pub count: u32,
    pub rent_returned: u64,
}
