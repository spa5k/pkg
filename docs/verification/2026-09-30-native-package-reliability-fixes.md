# Native package reliability fixes — 2026-09-30

Six confirmed defects are fixed on `feat/cask-nix-reliability`. GLM-5.3
implemented the repairs in disposable E2B sandboxes. The parent agent reviewed
the changes, corrected edge cases, and verified the final code with real Nix
on Linux and macOS. The native Nix profile remains the package-state authority.

The earlier Stats launcher error had a separate cause: SBCL 2.6.4 could not
start on macOS 27. The helper fix is already in this branch's base. See the
[helper verification](2026-09-30-macos27-app-helper.md) and
[PR #97](https://github.com/spa5k/pkg/pull/97).

## Changes

| Defect | Result after repair |
| --- | --- |
| Filtered stderr reading returned early on an interrupted system call. | Interrupted reads retry. Fatal read errors terminate and reap the child. Native cancellation exits 130 in both output modes. |
| A 40-character hexadecimal directory name was treated as a commit pin. | Revision rules use source semantics. Local paths remain mutable. |
| An unrelated damaged saved tap blocked a qualified request. | Qualified requests load their selected generation. Bare resolution stays strict. Doctor reports damaged saved generations without changing them. |
| Short names collided across native package namespaces. | Choices retain full attributes. Collision checks use the complete snapshot before filtering. Each choice resolves to its own package. |
| The catalog discarded the maximum macOS version. | Schema 4 retains both bounds in plans and the actual Nix index. Install and upgrade check the host before mutation. Upgrade uses the installed entry's original source. |
| 7zz extraction left nested Apple metadata streams inside signed apps. | Recursive cleanup removes materialized streams. Vendor resources, ordinary colon names, and validated links remain intact. A failed removal fails the build. |

Owners: [child process boundary](../../crates/pkg-cli/src/nix/process.rs),
[source references](../../crates/pkg-cli/src/nix/reference.rs),
[tap loading](../../crates/pkg-cli/src/commands/mod.rs),
[namespace resolution](../../crates/pkg-cli/src/catalog/search.rs),
[lifecycle gates](../../crates/pkg-cli/src/commands/lifecycle.rs),
[constraint generation](../../tools/cask-catalog/src/classify.rs), and
[archive extraction](../../nix/casks/lib/plan.py).

The initial [reliability review](2026-09-30-cask-nix-reliability.md) contains
the first five failure controls. The final native keeper is
[native-cli-check.py](../../tools/native-cli-check.py). The existing archive
and builder checks cover the extraction repair and actual index projection.
These checks now run in the existing Linux/macOS client and cask CI jobs.

## Source and execution

- Base: `58d802213be132ed8e78cd72719b8c374da4de3e`.
- Model: Pi with provider `zai`, explicitly selecting GLM-5.3.
- E2B image: `safivo/codex-pi-rust-nix-2026-09-28:default`, x86-64 Linux.
- Final Linux source verification: 915 tracked source files, no hash mismatch,
  before compilation and tests. `linux/source-manifest.json` records the tested
  tree. Later completion edits affect documentation only.
- macOS VM: a new disposable clone named `pkg-reliability-fix-20260930`,
  macOS 15.7.7, Apple silicon, Determinate Nix 2.35.2.
- VM client SHA-256:
  `ae010241cb1d18a9773b8acf98f646632ce7e26661b9b2d0e92e4e6aac8be4e7`.
  This matches the parent-built final client.
- Final extractor SHA-256:
  `8eb9e4c9fe37c858c466324d50c5dbe578256fe922700a6ee41a146f87c25ce4`.
  The VM copy matches the final source.

Several bounded GLM calls reached their deadline after saving code and logs.
One E2B sandbox then stopped accepting shell commands, including `true`.
File downloads still worked. A fresh sandbox rebuilt and passed the final
checks. Earlier Git resynchronization also failed and restored stale source.
That run was excluded from final source proof. Explicit source archives and
hash verification replaced resynchronization.

## Verified behavior

| Check | Result |
| --- | --- |
| Mutable hexadecimal path: install v1, upgrade v2, rollback, remove | Pass on Linux and macOS. The old macOS client stayed at v1. |
| Full namespace choices and unrelated damaged tap | Pass on Linux and macOS. |
| Real build cancellation in default and verbose output | Exit 130, unchanged entries, explicit retry runs, final removal succeeds on both platforms. |
| macOS minimum, maximum, and numeric overflow | Refused before profile mutation in the VM. |
| Upgrade after configured cask source changes | The original source's incompatible bound refuses upgrade. Lifting its bound permits its new version. |
| Real host macOS 27.0.1, maximum 15.9 | Refused. No profile was created. |
| Actual Nix index projection | Minimum 14.2 and maximum 15.9 survive on Darwin. Linux fields remain null. |
| Projection failure control | Removing only maximum projection produces valid Nix JSON. The same expected-value check rejects it. |
| macOS launcher helper | Cold build with SBCL 2.6.8, startup, sync, and launcher removal pass in the VM. |
| Native archive regression suite | Final real bsdtar/7zz checks pass, including nested streams, preserved resources, and the native APFS fixture. |
| Native cask builder suite | 14 pass, 0 fail. Includes a real `op` payload and expected archive/installer/link failures. |
| Same-source casks with different binaries | IDs `source-casks` and `source-casks-1` are distinct. Removing alpha preserves beta. Final profile is empty. |
| Real Linux packages | hello 2.12.3, ripgrep 15.1.0, jq 1.8.1, fd 10.4.2, and 1Password CLI 2.39.0 install and run. |
| Real Linux lifecycle | Duplicate install leaves the manifest unchanged. A valid-plus-missing install fails atomically. Rollback restores jq. Final removal leaves an empty profile. |

Native entry IDs are opaque Nix identities. A source-directory name as an ID
is not a collision. A prior delegated report inferred a collision without
installing two independent casks. The final two-cask test disproves that claim.
Two packages that both provide `bin/op` have an expected file conflict.

## Stats signature failure and repair

The VM installed Stats 3.0.13 from its pinned disk image:
`be8841586446a9a1e80d4c4fd0c434d8f1182b460ca728c82dd465c1b0c23415`.
The pristine mounted app passed `codesign --verify --deep --strict`.
The original extractor's output failed in the nested `LaunchAtLogin.app`.

All original files and links were unchanged. The output contained 232 extra
`:com.apple.*` metadata files. The old cleanup checked only direct children
of the extraction directory. The real-7zz failure control reproduced this
error with a small archive.

After repair, `pkg upgrade --all` rebuilt Stats. Its 147 vendor files and links
exactly matched the original mounted app. No metadata remnants remained.
Deep and strict signature verification passed. Explicit launcher sync passed.
The app started from the new Nix store path. Removal cleared its native entry
and launcher. The final profile was empty. No vendor executable or signature
was changed. No Gatekeeper policy change was made.

## Project checks

- `cargo build --locked --all-targets`: pass.
- `cargo test --locked`: 240 tests pass on the parent macOS host. The initial
  restricted run could not bind a test canary or write a Nix cache. The
  unrestricted rerun passed.
- Format, Clippy with warnings denied, and rustdoc with warnings denied: pass.
- Dependency bans, licenses, and sources: pass.
- Installer syntax, ShellCheck, and 43 installer tests: pass; seven tests have
  platform skips.
- Final Linux release build and native checks: pass.
- Workflow Actionlint, builder shell syntax/ShellCheck, documentation links,
  strict OpenSpec validation, and diff whitespace: pass.
- Two independent catalog generations exactly match the committed output.
  Input revision remains `245947c0b920cbe83f4003bb7c6be737352c8314`.
  Input SHA-256 remains
  `1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251`.
  Baseline remains 15.7.7. There are 7709 records, with 3143 Darwin and 160 Linux
  metadata-eligible records. Eligibility does not establish vendor runtime
  compatibility.

## Limits and release order

The candidate client requires schema 4. The matching official catalog must
reach main before that client is released. Main's earlier schema-3 catalog
refusal is expected before this change is merged. Old saved tap generations
need `pkg tap update`. No compatibility fallback or feature-branch source pin
was added.

A broad Nixpkgs search timed out in an earlier stress run. That observation
remains a performance limit. These tests do not establish exhaustive vendor
compatibility or interactive GUI behavior. The final native keeper verifies
cancellation of the filtered and verbose profile paths; it does not establish
every possible captured-output failure.

The temporary VM was stopped and deleted after evidence export. Active E2B
sandboxes were stopped after downloads. Two earlier sandboxes had already
expired. Test profiles were isolated from the user's package profile.

The [evidence archive](native-package-reliability-fixes-2026-09-30.tar.gz)
contains the final source manifest, parent build/check logs, Linux commands
and results, VM failure controls, signature/content/startup/removal evidence,
and raw GLM metadata-fix notes. Raw model notes are historical evidence. This
report records the final classification, including the parent corrections.
