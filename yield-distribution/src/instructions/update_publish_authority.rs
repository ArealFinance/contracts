//! update_publish_authority — authority rotates the server wallet that can publish roots.

use arlex_lang::prelude::*;
use pinocchio::sysvars::{clock::Clock, Sysvar};

use crate::error::YdError;
use crate::events::PublishAuthorityUpdated;
use crate::state::DistributionConfig;

#[derive(Accounts)]
pub struct UpdatePublishAuthority<'info> {
    #[account(signer)]
    pub authority: &'info AccountView,

    #[account(
        mut, has_one = authority, account_type = "DistributionConfig",
        seeds = [b"dist_config"], bump
    )]
    pub config: &'info AccountView,
}

pub fn handler(
    ctx: Context<UpdatePublishAuthority>,
    new_publish_authority: [u8; 32],
) -> Result<()> {
    if new_publish_authority == [0u8; 32] {
        return Err(ProgramError::from(YdError::ZeroDestination));
    }

    let config = DistributionConfig::load_mut(ctx.accounts.config, ctx.program_id)?;
    let old_publish_authority = config.publish_authority;
    config.publish_authority = new_publish_authority;

    let clock = Clock::get()?;
    emit!(PublishAuthorityUpdated {
        old_publish_authority,
        new_publish_authority,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}
