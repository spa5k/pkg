# macOS technical preview package

This recipe builds a script-only macOS component package.

The package contains the shipping `pkg-install` artifact in its script area.
It does not compile code on the install host.
It does not install product files before `pkg-install` authenticates the product bundle.

Run this command on macOS:

```sh
packaging/macos/build-preview.sh \
  /absolute/path/to/pkg-install \
  /absolute/path/to/pkg-0.1.0-alpha.7-preview.pkg \
  0.1.0-alpha.7
```

The recipe ad-hoc signs a temporary copy of `pkg-install`.
The result is for local technical-preview tests only.

Use a shipping `pkg-install` binary that was compiled with the signed TUF root and the fixed HTTPS metadata and target URLs.
The manual macOS proof workflow is the canonical preview build recipe.

For an interactive preview install, run the verified package payload from a terminal:

```sh
bash packaging/macos/install-preview.sh /absolute/path/to/preview.pkg EXPECTED_SHA256
```

Use the checksum from the release publication. The runner verifies a private copy,
checks the embedded executable signature, and runs it with `sudo` in the foreground.
It prints the log path and returns the actual setup status. It does not run the
package's scripts or reset an incomplete installation.

On macOS, administrator privileges do not grant access to all system configuration
files. Allow the system configuration prompt if macOS shows it. If macOS denies
access, allow the terminal in System Settings > Privacy & Security > Full Disk
Access, then restart that terminal. See [Apple's system configuration access
guide](https://support.apple.com/guide/mac-help/mchlccb25729/mac).

The previous background worker lost the interactive user's privacy authorization.
Its PackageKit success result only meant the job was queued. The current package
script waits for setup and returns its exit status. The interactive runner is the
preview path for hosts that deny access from PackageKit's background session.

The package is not Developer ID signed.
The package is not notarized.
The package is not proven Gatekeeper-clean.

TODO: Add Developer ID signing and notarization after the required credentials are available.
