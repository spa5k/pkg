//! Operating-system bindings.

use std::os::unix::net::UnixStream;

pub mod linux;
pub mod macos;

/// Returns the kernel-authenticated effective uid for one connected local
/// socket. Payload identity is never consulted.
pub(crate) fn peer_uid(stream: &UnixStream) -> Result<u32, ()> {
    #[cfg(target_os = "linux")]
    {
        return linux::peer_credentials(stream)
            .map(linux::LinuxPeerCredentials::uid)
            .map_err(|_| ());
    }
    #[cfg(target_os = "macos")]
    {
        return macos::peer_credentials(stream)
            .map(macos::MacOsPeerCredentials::uid)
            .map_err(|_| ());
    }
    #[expect(
        unreachable_code,
        reason = "the cfg-gated return above makes this fallback reachable only on unsupported platforms"
    )]
    Err(())
}

/// Requires the connected peer to be the configured singleton broker.
pub(crate) fn authenticate_broker(stream: &UnixStream, broker_uid: u32) -> Result<(), ()> {
    if peer_uid(stream)? == broker_uid {
        Ok(())
    } else {
        Err(())
    }
}
