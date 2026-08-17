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

## Test the retained Linux alpha proof

Use only a disposable Ubuntu 24.04 x86-64 VM. The VM must have Docker, Git, the
`gh` CLI, Python 3, and the `file` utility. Docker must support privileged
containers. Do not run this proof on a workstation or on a host with Nix.

First confirm the host boundary:

```sh
set -eu
test "$(uname -s)" = Linux
test "$(uname -m)" = x86_64
! command -v nix
test ! -e /nix
test ! -e /opt/pkg
docker info >/dev/null
```

The retained artifact is available for seven days from its 2026-08-14 upload.
GitHub reports expiry at 2026-08-21 19:15:39 UTC. Download and verify the exact
artifact:

```sh
set -eu
test "$(gh api repos/spa5k/pkg/actions/artifacts/9231189201 --jq .expired)" = false
preview_root=$(mktemp -d)
mkdir "$preview_root/download" "$preview_root/unpacked"
gh run download 31830773943 \
    --repo spa5k/pkg \
    --name pkg-v0.1.0-alpha.1-x86_64-linux-proof \
    --dir "$preview_root/download"
archive="$preview_root/download/pkg-v0.1.0-alpha.1-x86_64-linux-proof.tar.gz"
printf '%s  %s\n' \
    01cbbe1a175884bfdace159ac4749c72eb6cf1df934dd2afff317064886ee6ec \
    "$archive" \
    | sha256sum --check --strict
tar -tzf "$archive"
tar -xzf "$archive" -C "$preview_root/unpacked"
(
    cd "$preview_root/unpacked"
    sha256sum --check --strict SHA256SUMS
    file v0.1.0-alpha.1/pkg-installer-x86_64-linux \
        | grep -F 'ELF 64-bit LSB' \
        | grep -F 'x86-64'
)
```

The exact tar is inspection-only. It contains the bootstrap, installer, and
checksums. It does not contain the CI-only TUF publications, proof HTTPS server,
or proof certificate authority. Its fixed loopback proof service is not public.
Do not run `install.sh` or `pkg-installer-x86_64-linux` from this tar.

Use the repository proof to run the full preview. It creates fresh test keys,
builds the fixed loopback proof service, stages one artifact, and runs that
artifact in a separate privileged container with no compiler or source tree:

```sh
set -eu
proof_root=$(mktemp -d)
gh repo clone spa5k/pkg "$proof_root/pkg"
cd "$proof_root/pkg"
git checkout --detach 2d902f0f23ec788f764094ca001fc883d02088f1
test "$(git rev-parse HEAD)" = 2d902f0f23ec788f764094ca001fc883d02088f1
tests/linux-clean-host/run.sh --keep-artifacts "$proof_root/rebuilt"
```

The proof stops on the first failed check. It checks:

- foreign-Nix refusal and interrupted-install recovery;
- authenticated ownership-drift refusal;
- install, exact retry, and binary-cache installs of `hello` and `ripgrep`;
- one approved local build of `cxx-prettyprint`;
- signed update, upgrade, rollback, and cache repair;
- ordinary-user isolation from raw Nix, helper, daemon, and trust controls;
- uninstall, idempotent absence, and final service, account, Nix, and pkg-state
  absence.

The proof uses disposable test keys and local proof publications. It is not a
production release. Production key ceremony, hosting, license selection, and
publication remain external gates.

This alpha has no managed installer-replacement flow. To move to a later alpha, run the safe
uninstall below. Then install the exact new version. Do not install a new alpha over an older one.

If an existing Nix installation is detected, installation refuses. Remove it only with that
installation's own documented uninstaller, then rerun the installer. `pkg` never removes or adopts
an installation it cannot authenticate as its own.

## Uninstall

Use `pkg uninstall --dry-run` to preview the exact product-owned assets, then `pkg uninstall` to
remove them. The uninstaller removes authenticated per-user package state under
`~/.local/share/pkg`. It refuses changed, unrecorded, or unmanaged state and preserves that state
for manual review.
