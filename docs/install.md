---
title: Install pkg
---

# Install pkg

No public technical-preview release exists yet. `docs/install.sh` is the release template, not a
working source installer: it exits before network access while any release placeholder remains.
This is intentional—the project will not download an unpinned “latest” build.

For a published release:

1. Download `install.sh` and its checksum from the exact version's signed release notes. Do not pipe
   a network response directly into a shell.
2. Verify the script checksum using `sha256sum` on Linux or `shasum -a 256` on macOS.
3. Read the script, then run `sh install.sh`. The alpha script supports only Linux x86-64. It
   downloads one exact version over HTTPS, verifies the artifact against the embedded SHA-256, and
   only then asks for privilege.
4. Run `pkg doctor` after installation.

The script accepts `--verify-only` to download and verify without installing. It never accepts a
caller-provided URL, checksum, target, install path, or Nix setting.

Linux arm64 remains unavailable until it has the same staged-artifact and clean-host proof. The
macOS release path remains separate from this Linux alpha.

If an existing Nix installation is detected, installation refuses. Remove it only with that
installation's own documented uninstaller, then rerun the installer. `pkg` never removes or adopts
an installation it cannot authenticate as its own.

## Uninstall

Use `pkg uninstall --dry-run` to preview the exact product-owned assets, then `pkg uninstall` to
remove them. The uninstaller refuses unrecorded or unmanaged Nix state.
