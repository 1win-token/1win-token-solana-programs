//! Program events (`#[event_cpi]` + `emit_cpi!`).

use anchor_lang::prelude::*;

#[event]
pub struct Initialized {
    pub owner: Pubkey,
    pub admin: Pubkey,
    pub operator: Pubkey,
    pub token_mint: Pubkey,
    pub withdrawal_authority: Pubkey,
    pub tge_ts: i64,
}

#[event]
pub struct TgeUpdated {
    pub tge_ts: i64,
}

#[event]
pub struct AdminUpdated {
    pub admin: Pubkey,
}

#[event]
pub struct OperatorUpdated {
    pub operator: Pubkey,
}

#[event]
pub struct OwnerProposed {
    pub pending_owner: Pubkey,
}

#[event]
pub struct OwnerProposalCancelled {}

#[event]
pub struct OwnerUpdated {
    pub new_owner: Pubkey,
}

#[event]
pub struct WithdrawalAuthorityUpdated {
    pub withdrawal_authority: Pubkey,
}

#[event]
pub struct VestingCreated {
    pub activity_id: u64,
    pub recipient: Pubkey,
    pub reference_amount: u64,
    pub tge_bps: u16,
    pub monthly_bps: u16,
    pub cliff_months: u16,
    pub label: String,
}

#[event]
pub struct RecipientUpdated {
    pub activity_id: u64,
    pub new_recipient: Pubkey,
}

#[event]
pub struct VestingCancelled {
    pub activity_id: u64,
    pub amount: u64,
    pub beneficiary_token: Pubkey,
    pub rent_collector: Pubkey,
}

#[event]
pub struct Claimed {
    pub activity_id: u64,
    pub recipient: Pubkey,
    pub amount: u64,
    pub claimed: u64,
}

#[event]
pub struct VestingClosed {
    pub activity_id: u64,
    pub amount: u64,
    pub beneficiary_token: Pubkey,
    pub rent_collector: Pubkey,
}
