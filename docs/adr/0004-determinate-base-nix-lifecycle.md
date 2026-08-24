---
Status: Accepted
---

# Determinate owns the Base Nix lifecycle

## Context

DN-03 is **EVIDENCE COMPLETE; PRODUCT DELIVERY NO-GO**. It proves enough of the pinned Determinate Nix Installer contract to select the ownership model. It does not approve product cutover, clean uninstall, or successful crash recovery.

The old design made `pkg` own both the machine-wide Nix installation and the Package Lifecycle. This duplicated mature vendor work and mixed two different ownership domains. [ADR 0002](./0002-per-user-authoritative-state.md) remains accepted for per-user package state. [ADR 0003](./0003-broker-root-helper-privilege-split.md) remains accepted for Package Lifecycle privilege. This ADR partially supersedes only their Base Nix Lifecycle ownership claims.

## Decision

Determinate owns the Base Nix Lifecycle. This includes Base Nix install, the specific proved Base Nix repair and update routes, service control, and explicit Base Nix uninstall. `pkg` authenticates and orchestrates the vendor programs. It does not reimplement their Base Nix mutations.

`pkg` continues to own the Package Lifecycle. This includes package selection, builds, state, Generations, Activation Forests, package roots, package garbage collection, Package Repair, product policy, and the user experience. The Broker and Root Helper remain valid authorities for this work. Their later removal needs separate proof.

### Vendor executable seam

The planned integration uses Determinate Nix Installer v3.22.1 at full revision `4132ad07a15ee7d88c096ac7172b7afb2672866b`. The product install flow invokes it. Users are not required to install Nix first.

- DN-05 records each supported target asset and its exact digest.
- `pkg` verifies the selected asset digest before privilege or execution.
- One concrete Adapter must execute the authenticated vendor program by absolute path.
- The Adapter is the only Base Nix mutation path after a platform completes its cutover gates.
- The Adapter uses only observed arguments and explicit environment values.
- The product does not use PATH lookup, `curl | sh`, installer plan JSON, the experimental Rust library interface, copied vendor source, or a provider framework.

This seam does not hide raw Nix as a security boundary. It only keeps vendor implementation details out of the normal product interface.

### Diagnostics

Diagnostics control must be explicit. The integration must set `DETSYS_IDS_TELEMETRY=disabled` and use the proved explicit loopback endpoint control. An empty diagnostic endpoint alone is not an accepted disable control because source inspection showed that it can fall back to the public default transport. The DN-03 canary result is not proof that no network traffic can occur.

### Vendor Receipt and Handoff

`/nix/receipt.json` is the Vendor Receipt. It is opaque, private, and coupled to the installer version. `pkg` does not parse or copy its action list. It does not print, log, or publish its contents. It does not create a second receipt or vendor action journal.

DN-07 must implement this minimum Handoff state machine:

| Durable state | Meaning | Allowed transition |
|---|---|---|
| no Handoff record (`NotStarted`) | No product-owned vendor attempt is recorded. | Durably write `Started` before vendor execution. |
| `Started` | A vendor attempt can be incomplete or its result is not accepted. | Stay `Started` and fail closed after interruption, vendor failure, or failed validation. Move atomically to `Accepted` only after installed-state validation passes. |
| `Accepted` | The minimum private, no-follow identity of the observed vendor executable and Vendor Receipt passed validation. | Stay `Accepted` while identity matches. After a proved repair or update changes identity, validate it and atomically replace `Accepted`. Missing or changed identity fails closed. |

Vendor exit status `0` is only one input to validation. It is not acceptance. Crash R1 returned `0` from the recovery install, but installed-state validation failed because `_nixbld1` was missing. Product code must not use `SIGKILL` for vendor process control. Successful and functional crash recovery remain unproved.

### Repair and update limits

The installer does not provide general Base Nix recovery. Its default repair updates hooks. The macOS-only `repair sequoia` route has separate, optional proof in DN-12. It is not a cutover requirement by itself.

Only a pinned same-version `/usr/local/bin/determinate-nixd upgrade --version v3.22.1` was observed. N-to-N+1 behavior, rollback, and the complete set of files changed by upgrade remain unproved. DN-12 can implement only a route that later proof supports.

Package Repair remains separate and product-owned.

### Uninstall and residue

Vendor uninstall is not a clean uninstall on either tested platform. DN-13 owns only exact residue cleanup. It does not own crash recovery.

DN-13 must validate every live path identity before it removes any path. One missing or changed identity stops all cleanup. Cleanup must not use a recursive delete.

The exact accepted final vendor residue was:

- Linux: `/etc/nix` containing `/etc/nix/sentry-endpoint`.
- Apple Silicon macOS: `/etc/nix`, `/etc/nix/macos-keychain.crt`, `/etc/nix/sentry-endpoint`, an empty `/etc/fstab`, `/var/log/determinate-nix-init.log`, and `/var/log/determinate-nix-daemon.log`.

### Platform boundary

The supported evidence boundary is exact:

- Linux x86_64 has broad behavior evidence.
- Linux x86_64 and aarch64 have target Asset proofs.
- Linux aarch64 does not yet have the complete broad lifecycle proof.
- Apple Silicon macOS has lifecycle, reboot, residue, and negative crash evidence.
- Intel macOS is unsupported because v3.22.1 has no x86_64-darwin asset.

### Remaining owners and gates

| Owner | Required result |
|---|---|
| DN-05 | Authenticate the exact target asset digest before privilege. |
| DN-06 | Provide the thin process Adapter. Do not use `SIGKILL`. Do not orphan a privileged child. |
| DN-07 | Persist Handoff and validate the Vendor Receipt and installed state. Fail closed. |
| DN-12 | Prove and route inactive Base Nix repair and update behavior. The Sequoia repair proof is optional. |
| DN-13 | Perform exact, all-or-nothing residue cleanup after complete identity validation. |
| DN-15 | Pass the complete Linux lifecycle twice before Linux cutover. |
| DN-16 | Pass the complete Apple Silicon macOS crash, reboot, and lifecycle matrix twice before macOS cutover. |

This ADR does not claim any of the following:

- product integration or delivery approval;
- clean vendor uninstall;
- successful or functional crash recovery;
- acceptance from vendor exit status `0` alone;
- safe `SIGKILL` recovery;
- a stable or product-owned Vendor Receipt format;
- a general installer repair or update capability;
- mandatory `repair sequoia` proof;
- that an empty diagnostic endpoint disables diagnostics;
- absence of all network traffic;
- complete Linux aarch64 lifecycle support;
- Intel macOS support;
- an implemented Handoff;
- movement of Package Lifecycle work to Determinate;
- removal of the Broker or Root Helper; or
- hidden raw Nix as a security boundary.

## Consequences

- `pkg` can delete custom Base Nix code only after the matching Linux or macOS cutover proof passes.
- Each Base Nix asset has one vendor owner. Each package and product asset keeps one product owner.
- Product recovery must validate realized state instead of trusting a process exit code.
- Receipt contents stay outside product logs, evidence, state, and interfaces.
- Negative cleanup and crash results remain explicit stop gates. This ownership decision does not weaken them.

## Evidence

- [DN-03 parent decision](../../spikes/s6-determinate-installer/FINDINGS.md)
- [Linux runtime and Asset findings](../../spikes/s6-determinate-installer/linux-vm/LINUX-FINDINGS.md)
- [Apple Silicon macOS lifecycle, residue, and crash findings](../../spikes/s6-determinate-installer/macos-vm/FSTAB-CONTRACT-RESEARCH.md)

## Rejected Alternatives

- **Keep the custom Base Nix implementation.** This keeps duplicate lifecycle code without a product benefit.
- **Use PATH or `curl | sh`.** This does not authenticate one exact executable before privilege.
- **Use the experimental Rust library interface.** Upstream marks it experimental. The shipped executable is the proved interface.
- **Copy selected vendor Rust modules.** This creates a partial fork and loses upstream lifecycle integration.
- **Fork the complete vendor source now.** No proved blocking gap requires ownership of that code.
- **Use installer plan JSON as a product interface.** It exposes version-coupled vendor implementation details.
- **Parse or duplicate the Vendor Receipt.** It creates a second ownership ledger and couples the product to private action types.
- **Add a provider trait or framework.** One concrete integration does not justify an abstraction.
