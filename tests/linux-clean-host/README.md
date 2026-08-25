# Linux product install checkpoint

Run this destructive host test only in Docker:

```sh
tests/linux-clean-host/run.sh
```

The staging image builds the actual x86-64 `pkg-install` binary and an ephemeral
signed release. A separate final image receives only the staged, versioned
artifact, its checksum-pinned bootstrap, and the proof release. That clean host
has no source tree or compiler.

The proof uses the pinned Determinate Nix Installer and the public `pkg` CLI.
It runs the vendor install and uninstall lifecycle twice in fresh containers.
Each run also proves the product Package Repair and channel lifecycle. It proves
bootstrap verification, install retry, the authenticated installed vendor
helper, opaque receipt metadata, functional vendor Nix and systemd state,
product Broker and Root Helper isolation, package operations, product cleanup,
and vendor uninstall postconditions. Determinate owns its supported native
update. `pkg` exposes no Base Nix update action in this alpha. This proof does
not invoke or validate `determinate-nixd upgrade`. General Base Nix repair has
no supported vendor command or product action. The proof records vendor residue.
It does not require exact `/etc/nix` or `nixbld` cleanup. Separate clean hosts
prove foreign-Nix refusal before mutation and product-asset ownership-drift
refusal.

For uninstall, `pkg` first completes and verifies product-owned cleanup. It then
revalidates the exact installed vendor helper and opaque receipt. It consumes
the Accepted Handoff immediately before `exec`. After a successful `exec`, no
`pkg` uninstall process remains. The vendor command owns signals and the status
returned to the calling shell. Determinate also owns its temporary directory,
self-copy behavior, and native residue.

A synchronous `exec` error restores the exact Accepted Handoff in production
tests. A `SIGKILL` between Handoff consumption and `exec` leaves Base Nix
unmarked and the Handoff absent. Alpha does not infer success, adopt that Nix,
retry uninstall, or repair this state. Recovery is unsupported. A vendor failure
after `exec` also has no `pkg` recovery path.

`/run/pkg-install-handoff.lock` is a deliberate volatile coordination exception.
It is root-owned with mode `0600`. It is not lifecycle state. A reboot normally
clears it.

This is a privileged Docker and systemd proof. It does not prove host boot or
reboot, SELinux behavior, foreign-host coexistence, or a complete Linux
distribution matrix. The retained artifacts use test keys. They are not a
production release.
