//! Pure update policy: what a verified manifest means for the running version.

use crate::verify::VerifiedManifest;
use crate::version::{Platform, Version};
use crate::UpdateError;

/// What the caller knows about itself.
#[derive(Clone, Copy, Debug)]
pub struct Context {
    pub current: Version,
    pub platform: Platform,
    /// Highest manifest serial ever accepted for this channel (0 = none).
    pub highest_serial: u64,
    pub now_unix: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offer {
    UpToDate,
    Available {
        version: Version,
        size: u64,
    },
    /// The running version is revoked and the manifest's version is lower: an explicit,
    /// separately confirmed downgrade is the only way forward. Never offered otherwise.
    DowngradeOnly {
        version: Version,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Advice {
    pub offer: Offer,
    /// The running version is listed as revoked.
    pub current_revoked: bool,
    /// The running version is older than `min_supported`.
    pub below_min_supported: bool,
}

/// Anti-rollback and replay rules. Clock-free (the validity window was checked by `verify`).
///
/// - serial lower than `highest_serial` => `ReplayedManifest`
/// - the manifest's own version revoked => `Revoked`
/// - no artifact for this platform => `NoArtifact`
/// - version lower than current: `DowngradeOnly` if the current version is revoked, else
///   `StaleManifest`
pub fn decide(m: &VerifiedManifest, ctx: &Context) -> Result<Advice, UpdateError> {
    if m.serial() < ctx.highest_serial {
        return Err(UpdateError::ReplayedManifest);
    }
    if m.is_revoked(m.version()) {
        return Err(UpdateError::Revoked);
    }
    let art = m.artifact(ctx.platform).ok_or(UpdateError::NoArtifact)?;
    let current_revoked = m.is_revoked(ctx.current);
    let below_min_supported = ctx.current < m.min_supported();
    let offer = match m.version().cmp(&ctx.current) {
        std::cmp::Ordering::Equal => Offer::UpToDate,
        std::cmp::Ordering::Greater => Offer::Available {
            version: m.version(),
            size: art.size(),
        },
        std::cmp::Ordering::Less if current_revoked => Offer::DowngradeOnly {
            version: m.version(),
        },
        std::cmp::Ordering::Less => return Err(UpdateError::StaleManifest),
    };
    Ok(Advice {
        offer,
        current_revoked,
        below_min_supported,
    })
}
