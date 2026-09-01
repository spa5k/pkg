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

- authentication of the pinned Determinate Nix Installer 3.22.1 executable;
- invocation through an absolute path and a fixed environment;
- one vendor-process start;
- bounded output capture;
- one supervisor that waits for and reaps the vendor process;
- installed-state validation and Base Nix Handoff; and
- redacted user-facing error reporting.

Before vendor start, `pkg` can refuse or stop the operation. After vendor start,
the pinned executable provides no proved safe cancellation, signal, hard
timeout, or parent-death guarantee. `pkg` therefore waits for the vendor process.
It does not send a signal or start a second installer.

Vendor stdout and stderr are diagnostic output only. They are not a stable
progress or completion protocol. Only the terminal result and installed-state
validation can support acceptance.

For live Base Nix uninstall, `pkg` owns only the work before terminal `exec`:

- rejection of structured JSON or JSONL output before mutation;
- complete and verified product-owned cleanup;
- revalidation of the exact installed vendor executable and opaque Vendor Receipt;
- consumption of Accepted Base Nix Handoff and product state immediately before `exec`; and
- replacement of the `pkg` process with the fixed vendor uninstall invocation.

Live Base Nix uninstall requires plain output.
Dry-run uninstall can remain structured because it does not mutate the machine.

Across both paths, `pkg` also owns:

- product health and support policy;
- product-owned file, service, package, and state cleanup.

Linux product installation has three modes. Only a Fresh Install can activate
product services. An ordinary product upgrade and a same-release Product Asset
Repair require every fixed product unit to be inactive and disabled. They only
query systemd state. They change product files only. They do not stop, disable,
start, restart, or reload services. They leave all product services inactive and
disabled. The operator activates the authenticated result after the operation.

This offline boundary prevents an upgrade or repair from running a changed
service unit or binary. It also removes the need for an active-upgrade service
recovery protocol. Determinate remains the only owner of Base Nix. Product
upgrade and Product Asset Repair cannot install, repair, update, or remove Base
Nix.

`pkg` does not implement a second Base Nix install, repair, update, uninstall, or residue-cleanup engine. If the vendor has no supported operation, `pkg` reports that capability as unsupported. It does not fill the gap with custom Base Nix mutation code.

Vendor-owned residue after vendor uninstall is accepted for the alpha product.
The proof records relevant residue. `pkg` does not delete it.

Package Lifecycle remains product-owned. This includes package selection, builds, state, Generations, Activation Forests, package roots, package garbage collection, Package Repair, and package-level Root Helper Nix operations. Base Nix repair and Package Repair are different operations.

### Vendor executable seam

The integration uses the pinned Determinate Nix Installer 3.22.1 executable.
Users install `pkg`. The product installs Base Nix through that vendor executable.

- Release metadata fixes the target, size, digest, license, and source inventory.
- `pkg` authenticates the executable before privilege or execution.
- `pkg` invokes it by absolute path with only observed arguments.
- Live uninstall first finishes and verifies all product-owned cleanup.
- It then holds the stable Base Nix Handoff lock and revalidates exact `/nix/nix-installer` and opaque `/nix/receipt.json`.
- It consumes Accepted Base Nix Handoff and product state immediately before vendor execution.
- It uses `exec` to replace the `pkg` process with the fixed vendor uninstall invocation.
- Determinate owns vendor-phase signals, exit status, self-copy, native cleanup, temporary files, and residue.
- `pkg` does not supervise, cancel, resume, or retry the vendor phase.
- `pkg` treats `/nix/receipt.json` as opaque.
- `pkg` does not use PATH lookup, `curl | sh`, installer plan JSON, copied vendor source, the experimental Rust library, or a provider framework.

### Future update trust rule

`pkg` authenticates the pinned outer Determinate installer and invokes each vendor program through its fixed command path.

No Base Nix repair or update action is exposed on any alpha platform. General Base Nix repair remains unsupported. A future post-alpha update route needs separate approval.

If that route uses `determinate-nixd upgrade`, `pkg` accepts Determinate's inner download and update trust chain. `pkg` does not pre-bind or re-authenticate the downloaded daemon or profile payload.

After update, `pkg` runs functional installed-state health validation. It reports validation failure. It does not create a second update ledger or extend Base Nix Handoff only to mirror vendor update state.

This security trade-off avoids a second update engine and ledger, but it trusts Determinate to authenticate and apply the inner update payload correctly.

### Base Nix Handoff and pre-uninstall health

For install, `pkg` records only the minimum private Base Nix Handoff state:
`NotStarted`, `Started`, and `Accepted`. It writes `Started` before vendor start.
A persisted `Started` state means an Unknown Base Nix Outcome. The product fails
closed and does not automatically retry, resume, adopt, or reconstruct that
operation.

Only vendor exit status `0` followed by successful installed-state validation
can become `Accepted`. A nonzero exit, signal, wait failure, lost supervisor, or
failed validation cannot become `Accepted`. Exit status `0` alone is not proof.

Before terminal uninstall, `pkg` requires Accepted Base Nix Handoff. It consumes that exact state immediately before `exec`. It does not record or reconstruct the later vendor outcome.

Linux uses root-owned mode-`0600` `/run/pkg-install-handoff.lock` as its one
volatile coordination exception. macOS uses the persistent, zero-byte,
root-owned mode-`0600` `/private/var/db/pkg-install-handoff.lock`. The macOS
lock is coordination, not lifecycle state. Its safe parent avoids the native
group-writable `/private/var/run` directory.

The product does not replay vendor actions. It does not keep a second Vendor Receipt. It does not promise recovery behavior that the vendor does not support.

### Terminal uninstall failure

If `exec` returns synchronously, the vendor did not start. Under the same held
platform handoff lock, `pkg` restores the exact Accepted Base Nix Handoff and
revalidates the executable and receipt identities. Restore or
identity-validation failure fails closed.

`SIGKILL` or a crash between Accepted-state consumption and `exec` leaves Base Nix unmarked and Base Nix Handoff absent. The vendor did not start. `pkg` refuses the unmarked state. It does not infer success, retry, adopt, resume, repair, or reconstruct it. Alpha recovery is unsupported.

After `exec` starts the vendor program, Determinate owns signals and exit status. A later crash or loss of vendor outcome creates an Unknown Base Nix Outcome. `pkg` does not promise exactly-once vendor execution. It does not automatically retry, reconstruct vendor state, or clean vendor temporary files. Recovery requires reinstall or vendor support.

`pkg` must never infer vendor uninstall success from later absence of `/nix`, the installed helper, the Vendor Receipt, a vendor temporary file, a service, or any other vendor-owned path. It can observe and report absence. After crash or loss of `exec` outcome, the result remains an Unknown Base Nix Outcome.

This is a deliberate alpha limit. Product-owned cleanup happens before vendor cleanup. Vendor cleanup never runs before product-owned cleanup.

### Existing installations

Clean hosts can use the new lifecycle.

Foreign Nix, upstream Nix, unmarked Determinate Nix, and damaged accepted state remain fail-closed classification cases. Old private-alpha installations are a separate migration case. They do not block clean-host install work. The product must not invent or display an old-alpha reset command that it cannot authenticate and run.

### Platform proof boundary

Linux alpha proof can use a disposable native x86-64 GitHub-hosted runner.
The runner can execute the checked-in privileged Docker and systemd harness.
The proof is accepted only for the exact signed product commit. Complete logs,
the results matrix, and retained artifacts must be available and independently
reviewed. This proof covers only that runner and container environment. It does
not prove host boot, reboot, SELinux, foreign-host coexistence, or a complete
distribution matrix.

A local native x86-64 host can provide the same Linux evidence. An emulated
x86-64 Docker server cannot satisfy the proof.

macOS proof needs an Apple Silicon macOS VM or another disposable Mac. Docker cannot prove launchd, APFS, or `diskutil` behavior.

The macOS store-preserving uninstall action remains distinct until PR 4. PR 4 adopts terminal vendor uninstall only after real macOS proof passes.

Intel macOS is unsupported until an authenticated asset and complete lifecycle proof exist.

## Consequences

- There is one Base Nix lifecycle engine: Determinate.
- Only a Fresh Install activates Linux product services.
- Linux product upgrade and same-release Product Asset Repair are offline,
  systemd-query-only, and product-file-only operations.
- The operator activates product services after an offline upgrade or repair.
- Product code becomes smaller because it does not duplicate vendor repair, update, uninstall, or residue cleanup.
- Vendor capability limits become health and support results.
- Vendor-owned residue is an accepted alpha limitation.
- A started install has no safe product cancellation, signal, hard timeout, or parent-death guarantee.
- A persisted `Started` state or lost terminal uninstall result is an Unknown Base Nix Outcome.
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
