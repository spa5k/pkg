---
Status: Accepted
---

# Determinate owns the Base Nix lifecycle

## Context

The old design made `pkg` own the machine-wide Nix installation and the Package Lifecycle. This duplicated vendor lifecycle work and mixed two ownership domains.

[ADR 0002](./0002-per-user-authoritative-state.md) remains accepted for per-user package state. [ADR 0003](./0003-broker-root-helper-privilege-split.md) remains accepted for Package Lifecycle privilege. This ADR changes only Base Nix Lifecycle ownership.

The linked evidence records important limits. The Determinate tools do not provide a general Base Nix repair command. Vendor update interruption behavior is not a product contract. Vendor uninstall leaves residue. These limits define product health and support policy. They do not transfer Base Nix mutations back to `pkg`.

## Decision

Determinate owns Base Nix install, supported repair, update, Base Nix service setup and initialization, and uninstall.

For Base Nix install, `pkg` owns:

- authentication of the pinned vendor executable;
- invocation through an absolute path and a fixed environment;
- bounded progress and process supervision;
- cancellation before the install process commits an unsafe state;
- installed-state validation and Handoff; and
- redacted user-facing error reporting.

For live Base Nix uninstall, `pkg` owns only the work before terminal `exec`:

- rejection of structured JSON or JSONL output before mutation;
- complete and verified product-owned cleanup;
- revalidation of the exact installed vendor executable and opaque Vendor Receipt;
- consumption of Accepted Handoff and product state immediately before `exec`; and
- replacement of the `pkg` process with the fixed vendor uninstall invocation.

Dry-run uninstall can remain structured because it does not mutate the machine.

Across both paths, `pkg` also owns:

- product health and support policy;
- product-owned file, service, package, and state cleanup.

`pkg` does not implement a second Base Nix install, repair, update, uninstall, or residue-cleanup engine. If the vendor has no supported operation, `pkg` reports that capability as unsupported. It does not fill the gap with custom Base Nix mutation code.

Vendor-owned residue after vendor uninstall is accepted for the alpha product. `pkg` reports relevant residue when it can do so safely. It does not delete that residue.

Package Lifecycle remains product-owned. This includes package selection, builds, state, Generations, Activation Forests, package roots, package garbage collection, Package Repair, and package-level Root Helper Nix operations. Base Nix repair and Package Repair are different operations.

### Vendor executable seam

The integration uses the pinned Determinate executable. Users install `pkg`; the product installs Base Nix through the vendor executable.

- Release metadata fixes the target, size, digest, license, and source inventory.
- `pkg` authenticates the executable before privilege or execution.
- `pkg` invokes it by absolute path with only observed arguments.
- Live uninstall first finishes and verifies all product-owned cleanup.
- It then holds the stable Handoff lock and revalidates exact `/nix/nix-installer` and opaque `/nix/receipt.json`.
- It consumes Accepted Handoff and product state immediately before vendor execution.
- It uses `exec` to replace the `pkg` process with the fixed vendor uninstall invocation.
- Determinate owns vendor-phase signals, exit status, self-copy, native cleanup, temporary files, and residue.
- `pkg` does not supervise, cancel, resume, or retry the vendor phase.
- `pkg` treats `/nix/receipt.json` as opaque.
- `pkg` does not use PATH lookup, `curl | sh`, installer plan JSON, copied vendor source, the experimental Rust library, or a provider framework.

### Future update trust rule

`pkg` authenticates the pinned outer Determinate installer and invokes each vendor program through its fixed command path.

No Base Nix repair or update action is exposed on any alpha platform. General Base Nix repair remains unsupported. A future post-alpha update route needs separate approval.

If that route uses `determinate-nixd upgrade`, `pkg` accepts Determinate's inner download and update trust chain. `pkg` does not pre-bind or re-authenticate the downloaded daemon or profile payload.

After update, `pkg` runs functional installed-state health validation. It reports validation failure. It does not create a second update ledger or extend Handoff only to mirror vendor update state.

This security trade-off avoids a second update engine and ledger, but it trusts Determinate to authenticate and apply the inner update payload correctly.

### Install Handoff and pre-uninstall health

For install, `pkg` records only the minimum private Handoff state: `NotStarted`, `Started`, and `Accepted`. Install exit status `0` is not enough for acceptance. The product validates the installed state and fails closed when identity is missing, changed, or ambiguous.

Before terminal uninstall, `pkg` requires Accepted Handoff. It consumes that exact state immediately before `exec`. It does not record or reconstruct the later vendor outcome.

`/run/pkg-install-handoff.lock` is the one deliberate product coordination exception to product-residue removal. It is root-owned with mode `0600`. It is volatile coordination, not lifecycle state. It normally disappears at reboot.

The product does not replay vendor actions. It does not keep a second Vendor Receipt. It does not promise recovery behavior that the vendor does not support.

### Terminal uninstall failure

If `exec` returns synchronously, the vendor did not start. Under the same held `/run/pkg-install-handoff.lock`, `pkg` restores the exact Accepted Handoff and revalidates the executable and receipt identities. Restore or identity-validation failure fails closed.

`SIGKILL` or a crash between Accepted-state consumption and `exec` leaves Base Nix unmarked and Handoff absent. The vendor did not start. `pkg` refuses the unmarked state. It does not infer success, retry, adopt, resume, repair, or reconstruct it. Alpha recovery is unsupported.

After successful `exec`, Determinate owns signals and exit status. A later crash or loss of vendor outcome can leave the Base Nix outcome `Unknown`. `pkg` does not promise exactly-once vendor execution. It does not automatically retry, reconstruct vendor state, or clean vendor temporary files. Recovery requires reinstall or vendor support.

`pkg` must never infer vendor uninstall success from later absence of `/nix`, the installed helper, the Vendor Receipt, a vendor temporary file, a service, or any other vendor-owned path. It can observe and report absence. After crash or loss of `exec` outcome, the result remains `Unknown`.

This is a deliberate alpha limit. Product-owned cleanup happens before vendor cleanup. Vendor cleanup never runs before product-owned cleanup.

### Existing installations

Clean hosts can use the new lifecycle.

Foreign Nix, upstream Nix, unmarked Determinate Nix, and damaged accepted state remain fail-closed classification cases. Old private-alpha installations are a separate migration case. They do not block clean-host install work. The product must not invent or display an old-alpha reset command that it cannot authenticate and run.

### Platform proof boundary

Linux alpha proof can use a disposable privileged Docker container with systemd. That proof covers the exact container environment only. It does not prove host boot, reboot, SELinux, foreign-host coexistence, or a complete distribution matrix.

macOS proof needs an Apple Silicon macOS VM or another disposable Mac. Docker cannot prove launchd, APFS, or `diskutil` behavior.

The macOS store-preserving uninstall action remains distinct until PR 4. PR 4 adopts terminal vendor uninstall only after real macOS proof passes.

Intel macOS is unsupported until an authenticated asset and complete lifecycle proof exist.

## Consequences

- There is one Base Nix lifecycle engine: Determinate.
- Product code becomes smaller because it does not duplicate vendor repair, update, uninstall, or residue cleanup.
- Vendor capability limits become health and support results.
- Vendor-owned residue is an accepted alpha limitation.
- A terminal uninstall failure after product cleanup can leave Base Nix outcome `Unknown`.
- Clean-host Linux work can continue after the PR 2 foundation lands.
- Old private-alpha migration does not block clean-host work.
- Linux container proof cannot be presented as boot, reboot, SELinux, or foreign-host proof.
- macOS cutover stays blocked until disposable macOS proof passes.
- Package Repair, package builds, roots, garbage collection, and package-level Root Helper operations remain product-owned.

## Evidence

- [DN-03 parent decision](../../spikes/s6-determinate-installer/FINDINGS.md)
- [Linux runtime and Asset findings](../../spikes/s6-determinate-installer/linux-vm/LINUX-FINDINGS.md)
- [Apple Silicon macOS lifecycle, residue, and crash findings](../../spikes/s6-determinate-installer/macos-vm/FSTAB-CONTRACT-RESEARCH.md)
- [DN-08 vendor configuration limits](../../spikes/s6-determinate-installer/VENDOR-EXTENSION-PROOF.md)
- [DN-12 repair and update limits](../../spikes/s6-determinate-installer/DN12-VENDOR-REPAIR-PROOF.md)
- [DN-13 residue ownership limits](../../spikes/s6-determinate-installer/DN13-RESIDUE-OWNERSHIP-RESEARCH.md)

## Rejected alternatives

- **Keep the custom Base Nix implementation.** This duplicates vendor lifecycle code.
- **Add product repair, update, uninstall, or residue cleanup for vendor-owned Base Nix.** This creates a second lifecycle engine.
- **Copy or fork selected vendor Rust modules.** This creates an incomplete fork.
- **Use the experimental Rust library.** The pinned executable is the supported integration seam.
- **Parse or duplicate the Vendor Receipt.** This couples `pkg` to vendor action internals.
- **Add a provider framework.** One vendor does not need a framework.
- **Make old private-alpha migration a clean-host gate.** These are different starting states.
