//! Exact systemd unit bytes installed by the Linux backend.

/// Exact systemd unit bytes installed by the Linux backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinuxSystemdAssets;

impl LinuxSystemdAssets {
    /// Recreates the private helper socket parent after `/run` is cleared.
    pub const TMPFILES: &'static str =
        "d /run/pkg-helper 0750 root pkg-nix-broker -\nd /run/pkg 0755 root root -\n";

    /// Broker-only privileged-helper socket; peer credentials remain mandatory.
    pub const HELPER_SOCKET: &'static str = "[Unit]\nDescription=pkg privileged root helper socket\n\n[Socket]\nListenStream=/run/pkg-helper/root-helper.sock\nSocketUser=root\nSocketGroup=pkg-nix-broker\nSocketMode=0660\nDirectoryMode=0750\nRemoveOnStop=true\n\n[Install]\nWantedBy=sockets.target\n";

    /// Narrow root helper; it has no shell and no public command grammar.
    pub const HELPER_SERVICE: &'static str = "[Unit]\nDescription=pkg privileged root helper\nRequires=pkg-root-helper.socket\nAfter=pkg-root-helper.socket\n\n[Service]\nType=simple\nExecStart=/opt/pkg/bin/pkg-root-helper\nUser=root\nGroup=root\nEnvironment=HOME=/var/lib/pkg/helper-home\nEnvironment=TMPDIR=/var/lib/pkg/helper-home/tmp\nWorkingDirectory=/var/lib/pkg/helper-home\nUMask=0077\nPrivateTmp=true\nProtectHome=read-only\nProtectSystem=strict\nReadWritePaths=/nix/var/nix/gcroots/pkg /nix/store /nix/var/nix /var/lib/pkg/log/helper /var/lib/pkg/helper-home\nNoNewPrivileges=true\n\n[Install]\nWantedBy=multi-user.target\n";

    /// End-user broker socket. The broker authenticates every uid with peer creds.
    pub const BROKER_SOCKET: &'static str = "[Unit]\nDescription=pkg broker socket\n\n[Socket]\nListenStream=/run/pkg/broker.sock\nSocketUser=root\nSocketGroup=root\nSocketMode=0666\nDirectoryMode=0755\nRemoveOnStop=true\n\n[Install]\nWantedBy=sockets.target\n";

    /// Singleton unprivileged broker service.
    pub const BROKER_SERVICE: &'static str = "[Unit]\nDescription=pkg package broker\nRequires=nix-daemon.socket pkg-root-helper.socket\nAfter=nix-daemon.socket pkg-root-helper.socket\n\n[Service]\nType=simple\nExecStart=/opt/pkg/bin/pkg-nix-broker\nUser=pkg-nix-broker\nGroup=pkg-nix-broker\nEnvironment=HOME=/var/lib/pkg/broker-home\nEnvironment=TMPDIR=/var/lib/pkg/broker-home/tmp\nWorkingDirectory=/var/lib/pkg/broker-home\nUMask=0077\nPrivateTmp=true\nProtectHome=true\nProtectSystem=strict\nReadWritePaths=/var/lib/pkg/log/broker /var/lib/pkg/broker-home\nNoNewPrivileges=true\n\n[Install]\nWantedBy=multi-user.target\n";

    /// Returns all unit names and exact text in deterministic order.
    #[must_use]
    pub const fn all() -> [(&'static str, &'static str); 4] {
        [
            ("pkg-root-helper.socket", Self::HELPER_SOCKET),
            ("pkg-root-helper.service", Self::HELPER_SERVICE),
            ("pkg-nix-broker.socket", Self::BROKER_SOCKET),
            ("pkg-nix-broker.service", Self::BROKER_SERVICE),
        ]
    }
}
