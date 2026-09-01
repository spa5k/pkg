//! macOS launchd asset definitions.

/// Exact launchd definitions.
///
/// The daemon socket is created by Nix inside its traversal-restricted parent.
/// Product sockets are bound by their fixed jobs; both transports still
/// authenticate every accepted connection in-kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacOsLaunchdAssets;

impl MacOsLaunchdAssets {
    /// Mounts/unlocks the receipt-owned encrypted APFS store before daemon use.
    /// The helper reads the dynamic UUID/keychain handle from root-only state;
    /// neither appears in this plist or process arguments.
    pub const STORE_VOLUME: &'static str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>org.pkg.store-volume</string>
<key>ProgramArguments</key><array><string>/opt/pkg/bin/pkg-root-helper</string><string>--mount-store-volume</string></array>
<key>RunAtLoad</key><true/><key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
<key>ProcessType</key><string>Standard</string>
<key>UserName</key><string>root</string><key>GroupName</key><string>wheel</string>
<key>Umask</key><integer>63</integer>
<key>StandardOutPath</key><string>/Library/Application Support/pkg/log/store-volume.log</string>
<key>StandardErrorPath</key><string>/Library/Application Support/pkg/log/store-volume.log</string>
</dict></plist>
"#;

    /// Root managed Nix daemon. No launchd resource-limit claim is made: those
    /// are inherited per-process RLIMITs, not a per-build or subtree cap.
    pub const NIX_DAEMON: &'static str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>org.pkg.nix-daemon</string>
<key>ProgramArguments</key><array><string>/bin/sh</string><string>-c</string><string>/bin/wait4path /nix/var/nix/daemon-socket &amp;&amp; exec /opt/pkg/nix/current/bin/nix-daemon</string></array>
<key>EnvironmentVariables</key><dict><key>NIX_CONF_DIR</key><string>/opt/pkg/etc/pkg</string><key>NIX_DAEMON_SOCKET_PATH</key><string>/nix/var/nix/daemon-socket/socket</string><key>NIX_STATE_DIR</key><string>/nix/var/nix</string></dict>
<key>RunAtLoad</key><true/><key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
<key>UserName</key><string>root</string><key>GroupName</key><string>wheel</string>
<key>ProcessType</key><string>Standard</string><key>Umask</key><integer>63</integer>
<key>StandardOutPath</key><string>/Library/Application Support/pkg/log/nix-daemon.log</string>
<key>StandardErrorPath</key><string>/Library/Application Support/pkg/log/nix-daemon.log</string>
</dict></plist>
"#;

    /// Root helper with only the private broker-traversable socket path.
    pub const ROOT_HELPER: &'static str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>org.pkg.root-helper</string>
<key>ProgramArguments</key><array><string>/opt/pkg/bin/pkg-root-helper</string><string>--serve-macos</string></array>
<key>EnvironmentVariables</key><dict><key>HOME</key><string>/Library/Application Support/pkg/helper-home</string><key>TMPDIR</key><string>/Library/Application Support/pkg/helper-home/tmp</string></dict>
<key>WorkingDirectory</key><string>/Library/Application Support/pkg/helper-home</string>
<key>RunAtLoad</key><true/><key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
<key>UserName</key><string>root</string><key>GroupName</key><string>pkg-nix-broker</string>
<key>ProcessType</key><string>Standard</string><key>Umask</key><integer>63</integer>
<key>StandardOutPath</key><string>/Library/Application Support/pkg/log/helper/root-helper.log</string>
<key>StandardErrorPath</key><string>/Library/Application Support/pkg/log/helper/root-helper.log</string>
</dict></plist>
"#;

    /// Singleton unprivileged broker. Its only writable service-tree leaf is
    /// the private broker log directory; the socket directory is root-owned
    /// and group-writable solely by the singleton broker group.
    pub const BROKER: &'static str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>org.pkg.nix-broker</string>
<key>ProgramArguments</key><array><string>/opt/pkg/bin/pkg-nix-broker</string><string>--serve-macos</string></array>
<key>EnvironmentVariables</key><dict><key>HOME</key><string>/Library/Application Support/pkg/broker-home</string><key>TMPDIR</key><string>/Library/Application Support/pkg/broker-home/tmp</string></dict>
<key>WorkingDirectory</key><string>/Library/Application Support/pkg/broker-home</string>
<key>RunAtLoad</key><true/><key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
<key>UserName</key><string>pkg-nix-broker</string><key>GroupName</key><string>pkg-nix-broker</string>
<key>ProcessType</key><string>Standard</string><key>Umask</key><integer>63</integer>
<key>StandardOutPath</key><string>/Library/Application Support/pkg/log/broker/broker.log</string>
<key>StandardErrorPath</key><string>/Library/Application Support/pkg/log/broker/broker.log</string>
</dict></plist>
"#;

    /// Deterministic label/text set.
    #[must_use]
    pub const fn all() -> [(&'static str, &'static str); 4] {
        [
            ("org.pkg.store-volume", Self::STORE_VOLUME),
            ("org.pkg.nix-daemon", Self::NIX_DAEMON),
            ("org.pkg.root-helper", Self::ROOT_HELPER),
            ("org.pkg.nix-broker", Self::BROKER),
        ]
    }
}
