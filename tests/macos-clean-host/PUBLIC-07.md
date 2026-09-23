# PUBLIC-07: locked public flake packages

Date: 2026-09-24. Result: local gates and native macOS checks passed.
No public channel, release, or host installation was changed by this proof.

## Source and environment

- Native proof source commit: `bda2debc42c15b4e3c2755ae3d0b2596a9def577`.
- Disposable Apple Silicon Tart VM, macOS 15.7.7 (24G720).
- Determinate Nix 2.35.2; broker UID and GID 333.
- VM-only signed test channel, sequence 49, with the existing alpha test root.
- Public flake: `github:Mic92/nix-update#default`.
- Locked revision: `d23a574edb7560505af9ccb472d0baeec5aae130`.
- Resolved package: `nix-update` 1.16.0.

The earlier alpha.47 foreground install supplied the product runtime. The final
broker build was then installed in that disposable VM with its production owner,
group, and mode. Its installed SHA-256 matched the local candidate.

| Artifact | SHA-256 |
|---|---|
| Broker | `ca6bfa733ef309748c78b9ad868b742a0971b1015aa47f3da382193c78761a03` |

The final installer manifest also owns `/private/var/db/pkg-source` as a private
broker directory. The VM directory used for this proof had the same path, owner,
group, and `0700` mode. This run did not repeat a clean full product install after
the final manifest-only change.

## Observed results

| Check | Result |
|---|---|
| Public flake install | `pkg --yes --verbose install 'github:Mic92/nix-update#default'` resolved, evaluated, built, saved, and activated the package |
| Exact state | `pkg --json list` retained the public selector, locked revision, name, and version |
| Catalog compatibility | `pkg --yes install jq` installed jq 1.7.1 from the signed cache under the same final broker policy |
| Outdated status | The public flake appeared in `uncheckedSources`; catalog packages kept normal channel checks |
| Pin and unpin | Both commands accepted the original public selector; `upgrade --all` preserved the exact lock when no change was eligible |
| Removal | Removing the public selector removed its active command |
| Rollback | Rollback restored the public flake package and its command |
| Repair | `pkg --yes repair --verify-only` reported a clean generation |
| Health | Final `pkg doctor` reported healthy |

The first public flake run performed the native local build. The exact final
broker repeated the same install after removal. Nix reused its local store data
for the repeated build, so the second run proves final-binary evaluation and
activation, not another cold build.

## Source isolation and refusals

On macOS, every source metadata and derivation evaluation child runs through a
Seatbelt profile. The profile denies local file data by default. It allows the
Nix store, required macOS system paths, the exact managed Nix executable, and one
private source workspace. The reference lock file stays inside that workspace.

Native probes evaluated cached and previously unseen GitHub flakes through this
profile. The fresh probes fetched `NixOS/templates`, `hercules-ci/flake-parts`,
and `edolstra/flake-compat`. A focused macOS test proved that a source child can
read its private workspace and cannot read a marker outside it. Linux refuses
public flake references before any Nix source operation until it has an
equivalent per-process local-file read boundary. Ordinary Nixpkgs packages keep
their existing Linux behavior. A Linux-only direct-adapter regression used a
forged Darwin target and proved that locking and evaluation both refuse without
starting a subprocess.

Separate native refusal probes confirmed these results:

- untrusted flake configuration was ignored;
- direct local file reads failed under pure evaluation;
- import from derivation failed because it was disabled;
- a missing dependency lock failed because lock changes were disabled;
- local path and SSH selectors failed at the public CLI boundary.

The implementation also validates the complete dependency lock graph after
metadata resolution. It accepts locked public GitHub, GitLab, HTTPS Git, and
HTTPS archive inputs. It rejects local, mutable, credential-bearing, incomplete,
or unsupported inputs before a package can enter product state.

## Validation and limits

Local G-LINT passed on Rust 1.96.1: formatting, Clippy, documentation, workspace
build, dependency policy, and vulnerability audit. The complete workspace unit,
contract, integration, benchmark-target, and doctest commands passed. G-QUALITY
passed with 930 debt sites across 23 lints. The final `pkg-nix` source also passed
an all-target Linux compile in the Rust 1.96.1 Bookworm image.

The native proof used one public GitHub flake and one macOS system. It does not
claim private repository support, arbitrary flake URL support, a changed-revision
upgrade, Linux public flake support, or a cold rebuild of every dependency.
Public flake upgrades deliberately resolve the original public reference again.

Evidence is retained outside the repository in `pkg-public07-validation` on the
development host. It includes gate logs, native lifecycle logs, refusal probes,
sandbox probes, exact candidate hashes, and the VM scripts. No public release or
channel was published.
