//! Program events (`#[event_cpi]` + `emit_cpi!`).

use anchor_lang::prelude::*;

#[event]
pub struct Initialized {
    pub owner: Pubkey,
    pub admin: Pubkey,
    pub operator: Pubkey,
    pub token_mint: Pubkey,
    pub daily_limit: u64,
    pub weekly_limit: u64,
}

#[event]
pub struct LimitsUpdated {
    pub daily_limit: u64,
    pub weekly_limit: u64,
}

/// `propose_owner`: a candidate is nominated, the role has not moved yet.
#[event]
pub struct OwnerProposed {
    pub pending_owner: Pubkey,
}

/// `cancel_owner_proposal`: the nomination of `pending_owner` is withdrawn.
#[event]
pub struct OwnerProposalCancelled {
    pub pending_owner: Pubkey,
}

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

/// `amount` - the burned amount (BR-4); `day_total` / `week_total` - the period
/// totals accumulated after this burn.
#[event]
pub struct BurnExecuted {
    pub day_index: u64,
    pub week_index: u64,
    pub amount: u64,
    pub day_total: u64,
    pub week_total: u64,
}
