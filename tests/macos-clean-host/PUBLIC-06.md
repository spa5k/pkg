# PUBLIC-06: unprivileged source evaluation

Date: 2026-09-23. Result: local gates and native macOS checks passed.
No public channel, release, or host installation was changed by this proof.

## Source and environment

- Source commit: `344921d0328348c6f684eae0228c04bee125d83c`.
- Disposable Apple Silicon Tart VM, macOS 15.7.7 (24G720).
- Determinate Nix 2.35.2; broker UID and GID 333.
- Nixpkgs revision: `a62e6edd6d5e1fa0329b8653c801147986f8d446`.
- Nixpkgs content hash: `sha256-oamiKNfr2MS6yH64rUn99mIZjc45nGJlj9eGth/3Xuw=`.
- VM-only signed test channel, sequence 49, with the existing alpha test root.

The foreground alpha.47 installer installed the candidate broker and helper
from that local channel. Their installed SHA-256 hashes matched the candidate
files. This is runtime candidate evidence, not a new public release.

| Artifact | SHA-256 |
|---|---|
| Broker | `94f22b8bf7d79378dd9e3f92d153756dac8b4bde01c26ece4f09537022320268` |
| Helper | `803ca677399b772f6e320920b967bb5c12dbd49a276d0334e9a8f6ede42b4bd7` |

## Observed results

| Check | Result |
|---|---|
| Source process identity | Observed `flake metadata`, `derivation show`, and `derivation show --recursive` running as broker UID 333 |
| Cached install | `pkg --yes install jq` installed 1.7.1; a JSON expression returned true |
| Local compilation | `pkg --yes --verbose install figlet` built 2.2.5; `figlet pkg` ran |
| Removal | `pkg --yes remove figlet` removed its active command |
| Rollback | `pkg --yes rollback` restored figlet; the command ran again |
| Same-pin upgrade | `pkg --yes upgrade jq` reported no eligible change |
| Repair verification | `pkg --yes repair --verify-only` reported a clean generation |
| Health | Final `pkg doctor` reported healthy |

The process observer sampled the Nix command lines during the jq install.
Every observed source metadata or evaluation command used UID 333. Cache and
store operations retained their existing root-helper route. Hermetic tests
also verify that metadata and evaluation use the local adapter with no helper
connection, and that retired root methods 21 and 30 reject valid old payloads.

The figlet output was absent before the build. The VM HTTPS proxy returned 404
for that output's exact narinfo and forwarded other cache requests with upstream
TLS verification. Dependencies were partly warm. This does not claim a cold
toolchain test, a changed-revision upgrade, or repair of damaged store content.
The earlier [PUBLIC-04 report](PUBLIC-04.md) records broader lifecycle evidence.

## Validation and limits

Local G-LINT passed on Rust 1.96.1: formatting, Clippy, documentation, workspace
build, dependency policy, and vulnerability audit. G-QUALITY passed with 930
debt sites across 23 lints. The Nix library suite passed 257 tests, with two
ignored; the installer library suite passed 371 tests. Focused source-routing
and retired-method refusal tests passed after the final code change.

Documentation links passed on an export of tracked files. The workspace has
ignored third-party files under `.opencode/node_modules`; those files are not
part of the exported check. F primary review and A security/spec review found
no code blockers at the source commit above. CI remains a separate merge gate.

This proof covers native macOS. Linux retains its existing local evaluation
route and remains subject to normal Linux lifecycle CI. Public flake input,
source locks, and source-preserving upgrades are PUBLIC-07 work. This change
does not enable them or establish a sandbox for arbitrary source definitions.

Evidence is retained outside the repository in `pkg-flake-evaluation-validation`
on the development host. It includes the candidate files, local signed channel,
gate output, lifecycle script and log, source process observations, installed
artifact hashes, and a SHA-256 manifest. No signing or TLS private key was copied
there. The disposable VM and its temporary trust settings were deleted after
evidence collection. Other VMs and the host installation were kept.
