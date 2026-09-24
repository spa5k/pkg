# Batched build-cache planning

Validated on 2026-09-24 in the disposable Apple Silicon Tart VM
`pkg-alpha48-upgrade-receipt-test`, with the alpha.48 CLI and broker and
Determinate Nix 3.22.1 (Nix 2.35.2).

The command was:

```sh
/usr/bin/time -p /usr/local/bin/pkg --verbose install 'github:casey/just#default' </dev/null
```

It selected `just 1.58.0`. The observed source revision was
`6119e6aff518b6550d9030955f905b01051b5772`.

| Helper | Elapsed command time | Result |
| --- | ---: | --- |
| Published alpha.48 | 260.70 seconds | Still preparing; interrupted before a preview |
| Batched path-info prototype | 106.79 seconds | Preview returned; approval required |
| Final batched validity and closure probes | 41.23 seconds | Preview returned; approval required |
| Final helper, fresh helper binary-cache metadata | 36.77 seconds | Preview returned; approval required |

These are end-to-end command times, including source evaluation and network
access. They are not isolated cache-probe timings or a controlled speed ratio.
Later runs reused source data. For the last run, the helper was stopped and its
`binary-cache-detsys-v3.sqlite` file was moved aside before restart. Source data
and the local store were retained.

The successful previews exited with code 68 (`ACQUIRE_NEEDS_APPROVAL`). No
package build or activation was approved. The preview still showed the existing
109.8 GiB disk estimate. That estimate requires a separate correction.

Only the root helper binary was replaced in the disposable VM. This proves the
runtime path, not an authenticated installer upgrade. The published helper and
its previous metadata cache were restored after the test. The host installation
was not changed.

## Cause and correction

The previous adapter launched separate Nix processes for each root and each
member of its download closure. The replacement uses groups of up to 32 paths
for local metadata, remote validity, recursive metadata, and signature checks.
It retains local observations for the duration of one inspection.

Remote `path-info` can fail without JSON when a batch contains a missing path.
The final implementation uses the fixed `nix-store --check-validity
--print-invalid --store https://cache.nixos.org` query to select remote roots.
Only a successful command with valid, unique, requested paths is accepted.
Recursive metadata and the existing cryptographic signature checks must then
succeed before a hit is returned. A failed validity query is a probe failure,
not an ordinary cache miss.

Each root's references are followed within the recursive batch response.
Unrelated roots do not inherit each other's missing dependencies. Missing
reference entries, invalid signatures, failed verification, and malformed
responses are rejected.

The regression with 65 cached leaf dependencies uses 14 Nix commands. The
previous algorithm needed 327 for that case.

## Validation

- `cargo test -p pkg-nix -p pkg-pipeline -p pkg-installer --locked --quiet`:
  762 tests passed; four existing tests ignored.
- Workspace formatting, Clippy with warnings denied, documentation with
  warnings denied, and the all-targets/all-features build passed.
- Release build of `pkg-root-helper` passed.
- `cargo deny --locked check` and `cargo audit` passed. The deny check reported
  existing duplicate-dependency warnings.
- The repository documentation link check still reports seven existing errors
  in local `.opencode/node_modules` files.

Final helper SHA-256:
`b9d377ec3257c928add5d3f462c1499bc3cae9198ea8c50bda2c08dcf8bffead`.

Published alpha.48 helper SHA-256:
`1f8d279b80bbcf722fd4fe0e2b5e9b2bb33ff882831075452a96cf6a6172abe8`.

## Refusal after build approval

A follow-up check used the same VM, the final batched helper, and a CLI built
with typed build-error reporting and deferred build progress. The published
broker remained unchanged. The test invoked the standalone CLI from `/var/tmp`:

```sh
/usr/bin/time -p /var/tmp/pkg-cli-build-preflight --verbose install 'github:casey/just#default' --yes
```

The command returned in 50.26 seconds with exit 65 (`PREFLIGHT_FAIL`):

```text
Checking for a trusted download...
Package source ready.
Preparing a local build...
Preparing the approved build...
error[PREFLIGHT_FAIL]: the build did not start because the disk-space or system-load check failed
hint: review the disk estimate, available disk space, and system load before retrying
```

The VM had about 62 GiB free. The unchanged 109.8 GiB heuristic requires about
132 GiB after the 20% disk margin. Resource admission therefore refused the
operation before dependency acquisition or compilation. This reproduces a
resource refusal; the original user's generic error did not retain its exact
broker refusal category in the public operation log.

Previously the CLI printed `Building` and a synthetic 0% before the broker
replanned and checked resources. It also discarded the typed execution refusal.
The CLI now reports the approved preparation phase and waits for a real progress
update, or a successful build result, before emitting build progress. Resource
refusals, expired approvals, changed plans, cancellation, service failures, and
execution failures keep distinct messages and exit categories.

All 152 CLI tests passed. They include resource refusal without build-start or
percentage events, preservation of real progress before an execution failure,
and successful completion with and without upstream progress. CLI Clippy with
warnings denied, workspace formatting, and the release CLI build passed.

This change does not alter the disk heuristic or its admission margin. The
helper was restored after the test. The installed host CLI and services were
not changed.

## Disk admission correction (candidate)

The candidate removes the one-GiB-per-missing-output heuristic. A graph with
100 small missing outputs no longer receives a 100 GiB size estimate.
`approxNewDiskBytes` stays null until a real size estimate is available.

The new `minimumFreeDiskBytes` field is a start threshold. It is the sum of:

- Missing cache content and compressed download bytes, with a 20% margin.
- One 5 GiB workspace reserve for the entire operation.

The 5 GiB value is a fixed admission reserve, not a measured build size or a
bound on peak usage. The preview says that the build can need more disk space.
This replaces a per-output heuristic with an explicit operation policy.
The policy is included in the approval-bound plan. Arithmetic overflow and
unavailable resource measurements still refuse execution. The threshold is
retained by the broker; execution requests cannot supply or reduce it.

Local paths now have a distinct private cache observation. They remain cache
hits but add no download or new-content bytes. The helper protocol preserves
that distinction and rejects inconsistent local observations.

The CLI, broker, and helper must be shipped together. This candidate is not
included in the published alpha.48 release.

Candidate validation on 2026-09-25:

- Full workspace tests passed with `PKG_HERMETIC=1`; doctests also passed.
- Workspace formatting, warnings-as-errors Clippy, documentation, and the
  all-targets/all-features build passed.
- G-QUALITY passed with 928 debt sites across 23 lints.
- Dependency policy and vulnerability checks passed.
- Both static hermetic audits passed. The strict network-denial wrapper could
  not run because this host refused `sandbox-exec`. The full test run therefore
  does not claim network isolation.
- Documentation links still have the same seven failures in local
  `.opencode/node_modules` files.

Native build validation will use the new disposable VM `pkg-build-fix-test`.
The previous proof VM was deleted by the user.

## Successful native build and lifecycle

The new VM used macOS 15.7.7, four CPU cores, 8 GiB RAM, and a 100 GB virtual
disk. The installed engine was Nix 2.35.2. The candidate broker was built with
the public alpha channel URLs and the verified alpha.48 root of trust. Only
the disposable guest received the candidate runtime binaries.

A fresh local store exposed another per-path fallback: failed local metadata
batches were retried one path at a time. That fallback now uses one legacy
validity query per batch and a metadata query only for present paths. A
regression with 65 entirely missing paths uses 11 commands for the complete
local and remote inspection. This also stops treating arbitrary single-path
command failures as confirmed local misses.

After this change, planning attempts took about 38–40 seconds. Two attempts
still returned a cache-probe refusal. Private failure-stage diagnostics were
added before further testing. The next preview succeeded in 39.73 seconds:

```text
Packages: just 1.58.0 (out)
New disk estimate: unknown
Free disk needed to start: 8.9 GiB
The build can need more disk space.
```

The approved command then completed in 170.08 seconds, including planning,
revalidation, dependency acquisition, compilation, and activation. It emitted
real build progress starting at 1%, completed at 100%, and activated gen-0001.
It resolved source revision `1d3ddccda956cdcce24c104c14dddcb1a9f46cf3`. This differs
from the source revision used in the earlier September 24 timing checks.

`just --version` returned `just 1.58.0`. Removal deleted its active command.
Rollback restored it, and the version command passed again. The final doctor
result was healthy, with only the guest shell's raw-Nix PATH warning.
`du -sh /nix/store` reported 3.6 GiB for the whole store, including Nix itself,
source data, and build dependencies. This is retained disk use, not peak build
usage or the size of just alone. About 62 GiB remained free.

Logs, scripts, and exact tested binary hashes are retained in
`target/build-fix-proof/` on the development host. The test VM was then reset
from the same pinned base for a separate clean-install preview check.

## Clean-install confirmation

The second VM instance was cloned from the same pinned base image after the
first instance was removed. It started with about 213 MiB in `/nix`, fresh
helper cache metadata, and no installed user packages. The same tested
candidate binaries were installed before its first package command.

The exact unapproved `just` command reached its preview on the first attempt
in 68.34 seconds. It showed the same 8.9 GiB start threshold and unknown total
build size. Exit 68 was expected because stdin was not interactive and no
`--yes` was supplied. This test included the first source and channel data
downloads. It did not reuse the first VM's source data or cache metadata.
The earlier cache-probe refusals did not recur in this clean-install check.

Final candidate SHA-256 values:

- CLI: `2758aa457131e77a656fbecd4e0995c16905f992012fc74cbcb3642b13ccca2c`
- Broker: `fbf2f3a58a7ce3a0140ae9ac84d5dbf211088c9c00e72c55671279e237cc5fbb`
- Helper: `a0bd4df12c70c378c4d2b4a096035a40aea28085d2463e862a76f357a279bf79`

The final adapter and installer tests, G-QUALITY, and workspace Clippy,
documentation, and all-targets/all-features build passed after the last source
changes. The published service binaries were restored in the second VM after
the check. The VM was stopped. No host runtime, public channel, or release was
changed.
