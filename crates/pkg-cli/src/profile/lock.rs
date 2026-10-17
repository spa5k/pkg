//! A native profile lease held through mutation, observation, and launcher refresh.

use crate::profile_lock::ProfileLock;

pub struct LockedProfile<'a> {
    nix: &'a crate::nix::Nix,
    paths: &'a crate::config::Paths,
    _lock: ProfileLock,
}

impl<'a> LockedProfile<'a> {
    pub fn acquire(
        nix: &'a crate::nix::Nix,
        paths: &'a crate::config::Paths,
    ) -> Result<Self, String> {
        let lock = ProfileLock::acquire(&paths.profile)?;
        Ok(Self {
            nix,
            paths,
            _lock: lock,
        })
    }

    pub const fn nix(&self) -> &crate::nix::Nix {
        self.nix
    }

    pub const fn paths(&self) -> &crate::config::Paths {
        self.paths
    }
}
