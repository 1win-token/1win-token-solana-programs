//! The program's instructions and the shared pre-TGE gate.

use anchor_lang::prelude::*;

use crate::errors::VestingError;
use crate::state::VestingConfig;

pub mod claim;
pub mod close;
pub mod create;
pub mod funded;
pub mod initialize;
pub mod recipient;
pub mod roles;
pub mod tge;
pub mod withdrawal;

pub use claim::*;
pub use close::*;
pub use create::*;
pub use funded::*;
pub use initialize::*;
pub use recipient::*;
pub use roles::*;
pub use tge::*;
pub use withdrawal::*;

pub(crate) fn require_before_tge(config: &VestingConfig, now: i64) -> Result<()> {
    require!(now < config.tge_ts, VestingError::ScheduleStarted);
    Ok(())
}
