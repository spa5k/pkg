# Linux product install checkpoint

Run this destructive proof only on a native x86-64 (`amd64`) Docker server:

```sh
tests/linux-clean-host/run.sh
```

The staging image builds the actual x86-64 `pkg-install` binary and an ephemeral
signed release. A separate final image receives only the staged, versioned
artifact, its checksum-pinned bootstrap, and the proof release. That clean host
has no source tree or compiler.

The proof uses pinned Determinate Nix Installer 3.22.1 and the public `pkg` CLI.
It runs the vendor install and uninstall lifecycle twice in fresh containers.
Each run also proves the product Package Repair and channel lifecycle. It proves
bootstrap verification, repeat product install, the authenticated installed
vendor helper, opaque receipt metadata, functional vendor Nix and systemd state,
product Broker and Root Helper isolation, package operations, product cleanup,
and vendor uninstall postconditions. Determinate owns its supported native
update. `pkg` exposes no Base Nix update action in this alpha. This proof does
not invoke or validate `determinate-nixd upgrade`. General Base Nix repair has
no supported vendor command or product action. The proof records vendor residue.
It does not require exact `/etc/nix` or `nixbld` cleanup. Separate clean hosts
prove foreign-Nix refusal before mutation and product-asset ownership-drift
refusal.

The install process contract is one authenticated vendor start. One supervisor
drains bounded output, waits, and reaps. After vendor start, the product has no
safe cancellation, signal, hard timeout, or parent-death guarantee. A persisted
`Started` Base Nix Handoff means an Unknown Base Nix Outcome. It fails closed
and does not authorize retry. Only vendor exit status `0` plus installed-state
validation can become `Accepted`.

For uninstall, `pkg` first completes and verifies product-owned cleanup. It then
revalidates the exact installed vendor helper and opaque receipt. Live uninstall
requires plain output. It consumes the Accepted Base Nix Handoff immediately
before `exec`. After `exec` starts the vendor program, no `pkg` uninstall
process remains. The vendor command owns signals and the status
returned to the calling shell. Determinate also owns its temporary directory,
self-copy behavior, and native residue.

A synchronous `exec` error restores the exact Accepted Base Nix Handoff in
production tests. A `SIGKILL` between Base Nix Handoff consumption and `exec`
leaves Base Nix unmarked and the Base Nix Handoff absent. Alpha does not infer
success, adopt that Nix, retry uninstall, or repair this state. Recovery is
unsupported. A vendor failure
after `exec` is an Unknown Base Nix Outcome. It has no `pkg` recovery path.

`/run/pkg-install-handoff.lock` is a deliberate volatile coordination exception.
It is root-owned with mode `0600`. It is not lifecycle state. A reboot normally
clears it.

This is a privileged Docker and systemd proof. The Docker server can be local or
on a disposable native x86-64 GitHub-hosted runner. A GitHub-hosted result is
accepted only for the exact signed product commit. Its complete logs, results
matrix, and retained artifacts must receive independent review.

This proof does not cover host boot, reboot, SELinux behavior, foreign-host
coexistence, or a complete Linux distribution matrix. The retained artifacts
use test keys. They are not a production release. The harness refuses ARM
servers instead of using x86-64 emulation. Linux and Docker evidence cannot
satisfy any macOS proof gate.
