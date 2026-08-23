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

`run-aarch64-container.sh` is the host runner for the narrow ARM64 Asset
proof. It uses the pinned image, `--platform linux/arm64`, `--network none`,
and `--rm`. It mounts the pinned `aarch64-linux` asset and guest probe as
read-only files. The evidence path must be absolute and absent. The exact
container name must not exist.

Invoke the host runner with the exact destructive approval:

```sh
./linux-vm/run-aarch64-container.sh --approve-destructive-container \
  /absolute/path/nix-installer-aarch64-linux \
  /absolute/path/new-private-evidence \
  pkg-s6-dn03b-arm64-probe-REV-RUN
```

The runner records the exact Docker argv. The argv includes `--rm`. After
Docker returns, the runner immediately queries `docker ps -a`. It compares
names by exact equality. It records the exact name, matching-name list, zero
count, UTC time, and provenance. It hashes this cleanup record. A nonzero
count fails the runner. It also writes `evidence.sha256` for the complete
private bundle. The runner never removes any other container.

`inside-aarch64-container.sh` is the guest side. The host runner invokes it
with the exact `--approve-destructive-container` argument.

Before it writes evidence or runs the installer, the script requires the
exact one-argument approval, root, and Linux. It also refuses if `/nix`,
`/etc/nix`, or `/usr/local/bin/determinate-nixd` exists or is a symlink.

The script records exact argv, the telemetry-disable environment, loopback
canary counts, receipt metadata and a private receipt SHA-256, installed-copy
identity, Nix execution, sentry metadata and private identity hashes, and the
strict residue result. It does not copy or print receipt or sentry contents.
The expected vendor result still exits nonzero when `sentry-endpoint` remains.
