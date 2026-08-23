# Linux destructive VM proof

This harness runs destructive Determinate installer checks in a disposable
Ubuntu 24.04 amd64 QEMU guest. It never mounts a host directory in the guest.
It never writes the base image.

Use the dated Ubuntu image at
`https://cloud-images.ubuntu.com/releases/noble/release-20260814/ubuntu-24.04-server-cloudimg-amd64.img`.
Its SHA-256 is `6e40c07ae715f744f84af0bec76415cc1987dd115b4b8de437818561f01a3733`.
Use the pinned `x86_64-linux` Determinate v3.22.1 asset from the parent
`assets.sha256` file.

The output directory must not exist. The base image must not be writable.
Port `127.0.0.1:22222` must be free. The host needs at least 16 GiB of free
disk. The 30G guest disk is sparse.

```sh
chmod 0444 /absolute/path/ubuntu-24.04-server-cloudimg-amd64.img
./linux-vm/run.sh --approve-destructive-vm \
  /absolute/path/ubuntu-24.04-server-cloudimg-amd64.img \
  /absolute/path/nix-installer-x86_64-linux \
  lifecycle \
  /absolute/path/new-evidence-directory
```

Valid lanes are `lifecycle`, `diagnostics-disabled`, `crash-recovery`,
`foreign-nix`, and `upstream-input`. Each lane uses a fresh 30G overlay.

The `lifecycle` lane always does a clean reboot after install. It compares the
kernel boot IDs before and after the reboot. It also runs the pinned
same-version `determinate-nixd upgrade --version v3.22.1` probe. This can use
the network and can update the Nix profile inside the disposable guest.

The runner requires a clean Git worktree. It records the product Git revision
and the pinned vendor full revision in the private host output. Each guest
phase has a two-hour timeout and a 60-second forced-stop grace period.

The runner reports `PASS`, `FAIL`, or `UNPROVED`. `FAIL` and `UNPROVED` exit
nonzero. Run `./linux-vm/test-static.sh` for checks that need no VM or network.

The lifecycle residue proof allows `/etc/nix` to remain only as an empty,
root-owned, non-symlink directory with mode `0755`. A repeat uninstall must
return the pinned missing-receipt refusal for `/nix/receipt.json`.

The lifecycle lane records no-follow identity evidence for
`/etc/nix/sentry-endpoint` at four stable lifecycle stages.

## Linux ARM64 Asset proof

`inside-aarch64-container.sh` is the guest side of the narrow ARM64 Asset
proof. Run it only in a fresh `linux/arm64` container. Mount the pinned
`aarch64-linux` asset at `/input` as read-only. Mount the script at
`/probe.sh` as read-only. Mount a new private evidence directory at
`/evidence`. Use Docker `--network none`.

The script records exact argv, the telemetry-disable environment, loopback
canary counts, receipt metadata and a private receipt SHA-256, installed-copy
identity, Nix execution, sentry metadata and private identity hashes, and the
strict residue result. It does not copy or print receipt or sentry contents.
The expected vendor result still exits nonzero when `sentry-endpoint` remains.
