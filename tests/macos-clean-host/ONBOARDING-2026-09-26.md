# Onboarding and quality audit — 26 September 2026

This audit uses the published alpha.54 as a new user would use it.
Source analysis and local checks use `4ab212196956f9c53313fcd14521f737a84e98f0`.
The next release remains deferred. No product binary, channel, or trust root was
changed by this work.

The later [daily-use and logic audit](BUG-HUNT-2026-09-26.md) adds 68 cases and
controlled fault tests. It found nine further defects, including two P1
failures. The clean-state repair results here do not cover those failures.

## Environment and method

- Fresh Tart VM `pkg-onboarding-20260926`, cloned from
  `ghcr.io/cirruslabs/macos-sequoia-base@sha256:3f4d14a5ffb9efd3bda2ae0184fd4bc2773d924ff8b7565f958761420ec41a0c`.
- macOS 15.7.7 (24G720), Apple Silicon `VirtualMac2,1`, four CPUs,
  8 GiB RAM, 100 GB virtual disk. Existing VMs were not changed.
- Normal `admin` user. Public commands ran in fresh zsh login sessions over SSH.
  Approval cases used a terminal and actual Enter/yes responses.
- The documented public installer was downloaded over HTTPS and executed as
  the normal user. It verified pinned downloads before administrator access.
- Published `pkg 0.1.0-alpha.54`, Determinate installer 3.22.1, Nix 2.35.2.
- Times in the evidence use UTC. The report date uses Asia/Kolkata.
  Command durations include prompt response time and are not benchmarks.

There are 101 recorded cases: 89 completed with exit 0, ten expected refusals
or cancellations, one expected reboot disconnect, and one known alpha.54
startup failure. The output findings below remain open despite successful exits.

The original host was free of `/opt/pkg` and `/nix/receipt.json`.
The [artifact hashes](evidence/2026-09-26-onboarding/artifact-hashes.txt)
record the retained binary and Nix identities. The initial installed CLI SHA-256 was
`f5ed9404307f8c448c00e7e5e5ad80dd19a490ab3ac1b0e7e77787e4d8b3925d`.

## User path

The [command manifest](evidence/2026-09-26-onboarding/commands.json) records each
command, exit code, duration, and output digest. The
[transcript](evidence/2026-09-26-onboarding/transcript.txt) retains the user-path
output. Help-only output is omitted from that transcript; each help command and
its digest remain in the manifest.

| Area | Observed result |
| --- | --- |
| Fresh setup | Public script verified its files, installed Nix and pkg, and completed doctor |
| Discovery | Home guide, all public command help pages, search, info, empty list/outdated/history, shellenv, Bash/zsh completion syntax passed |
| Cached packages | Installed and executed ripgrep 13.0.0 and fzf 0.46.0; package paths worked in subsequent login shells |
| Package policy | Pin/unpin worked; pinned upgrade was skipped; same-version upgrade reported no change; catalog sequence 54 remained current |
| Approval | Default Enter cancelled remove and build with exit 75; cancelled build left the package list unchanged; noninteractive removal refused with exit 68 |
| Recovery | Approved removal and rollback restored fzf; history inventory/diff worked; deletion of the active generation was refused |
| Public source | Built and executed just 1.58.0 from `github:casey/just#default`, source commit `3095fc171a1369505e0c6a2fd30616e5ff46c139`; short-name pin/unpin worked |
| Repair | Verify-only, preview, and approved repair found no damaged paths; this audit did not inject corruption |
| Output | JSON, JSONL, quiet, names-only, support output, plain output, and a 40-column color terminal were exercised |
| Errors | Missing packages, missing arguments, unsupported local source, active-generation deletion, and live JSON system removal refused as expected |
| Garbage collection | Removed 2,282 store paths; retained installed tools; explicit zero-day retention pruned the expected four old generations. Reporting defects are below |
| Repeat setup | Succeeded with unchanged CLI/broker/helper and Nix receipt/installer hashes; package inventory and commands survived |
| Reboot | Real guest reboot changed the boot UUID. Package inventory, CLI, and Nix hashes matched. The first doctor failed; a later read-only retry passed |
| Reinstallation | Reinstalled from the saved public installer after full removal, verified downloads without mutation, installed fzf in JSON mode, and executed it in zsh and Bash |
| System removal | Interactive removal and terminal vendor uninstall exited 0. `/usr/local/bin/pkg`, `/opt/pkg`, vendor executable, and vendor receipt were absent. An empty synthetic `/nix` node remained |

## Actionable findings

### UX-01 — P2: GC reports unknown bytes as zero

Case 58 printed:

```text
Collected store paths: 2282
Space freed: 0 B
Pruned 0 generations; collected 2282 store paths.
```

The real adapter in `crates/pkg-nix/src/real/root.rs:323` constructs
`GcReport::new(GcStatus::Collected, collected, 0)`. The zero is not measured.
`commands/local.rs:2585` forwards it to the public result and
`commands/human/details.rs:81` labels it as space freed.

The preview estimate in `pkg-store/src/gc.rs` only sums selected generation
outputs. It does not describe all existing unrooted build inputs that live GC
can collect. Both preview and result need truthful scope and unknown values.
Add an adapter-to-CLI regression case with real nonempty collection evidence.

### UX-02 — P2: system-removal preview cannot be inspected

Case 67 showed only the removal scope and the no-change statement.
Case 70 returned `{"actions":28,"status":"planned"}` plus result metadata.
Neither form names the actions, paths, retained items, or affected package state.
README says to preview the files to be removed, but that preview is unavailable.
`pkg-cli/src/main.rs:123` reduces the result to an action count.

Expose a bounded, safe summary of the actual removal plan before approval.
Keep private details redacted and preserve the closed privileged operations.

### UX-03 — P2: repeat installer creates its own duplicate-PATH warning

Case 61 warned that the active bin directory appeared twice and told the user
to remove duplicate shell snippets. The VM had exactly one documented line in
`.zshrc`. Case 62, in a new login shell, passed the PATH check.

`install.sh:123` and its template prepend the active directory unconditionally
for doctor. The shellenv implementation already avoids duplicate PATH entries.
Make the installer check use the same idempotent behavior. Test repeat setup
from a shell that has already loaded pkg.

### UX-04 — P2: the first dry-run gives no progress while it waits

Case 09, `pkg install ripgrep --dry-run`, produced no product output for about
31.8 seconds before showing its preview. The normal install showed a source
check message. `preview_install` and `preview_upgrade` call `prepare_build`
synchronously without emitting the normal progress events.

Show a bounded human progress notice during preview preparation. Keep JSON,
JSONL, quiet mode, and approval boundaries correct. Do not invent percentages.

### UX-05 — P2: uninstall leaves the documented shell line noisy

The user followed the guide and added `eval "$(pkg shellenv)"` to `.zshrc`.
After successful removal, case 76 and the next installer session printed:

```text
/Users/admin/.zshrc:1: command not found: pkg
```

The line is user-owned. Do not delete arbitrary shell configuration.
Document its removal, offer a guarded setup example, and explain cleanup before
terminal vendor handoff. Test a new shell after complete removal.

### UX-06 — P3: GC retention defaults are hidden

`pkg gc --keep-generations 3` kept eight generations in case 59.
This follows the code: the default 30-day age window also protects generations,
and the count is for retired generations in addition to the active one.
With `--max-age-days 0`, cases 63–65 retained active plus three retired generations.

Show both effective retention settings in help and preview. State that both
conditions must permit deletion. This is a clarity gap, not unsafe deletion.

### Known release gap: doctor immediately after reboot

Case 72 exited 74 with ownership unknown and an incomplete broker health check.
Case 74 started about 16 seconds later and passed. No repair, service restart,
receipt deletion, or state change was used between them.

[PR #61](https://github.com/spa5k/pkg/pull/61) already merged the bounded startup
probe. This audit used published alpha.54 and does not re-prove that source fix.
Its publication remains deferred and is not a new implementation item here.

### Informational observation: raw Nix on PATH

A normal login shell exposes the vendor Nix command. Doctor reports a UX warning
but exits healthy. This is intentional in `raw_nix_check`, and raw Nix visibility
is not a product security boundary. The fresh installer's restricted check and
a normal login shell can therefore show different informational results.

## Measured code quality

The [quality snapshot](evidence/2026-09-26-onboarding/quality-summary.json) records
counts and scope. The baseline is not a measurement of one platform.

| Evidence | Result |
| --- | --- |
| Shared checked-in baseline | 936 sites across 23 lints |
| Fresh macOS quality gate, Rust 1.96.1 | 924 sites across 23 lints; gate passed |
| Linux at the same main commit | 913 sites; [CI run 36191406641](https://github.com/spa5k/pkg/actions/runs/36191406641) passed |
| `unused_crate_dependencies` audit, all targets/features on macOS | 297 emitted diagnostics; 212 distinct package/target/kind/message tuples |
| `cargo test --locked --workspace`, Rust 1.96.1 | 1,218 passed, zero failed, five ignored, including doc-test summaries |

The dependency diagnostics include dev-dependencies and repeated builds of
individual targets. They are not a count of unused packages that can be removed.
No quality baseline was changed. Do not rebase a cross-platform baseline from
macOS alone.

Largest measured lint groups on macOS:

| Lint | Sites |
| --- | ---: |
| missing_errors_doc | 428 |
| too_many_lines | 142 |
| use_self | 116 |
| too_many_arguments | 76 |
| significant_drop_tightening | 49 |
| match_same_arms | 23 |
| doc_markdown | 12 |
| option_if_let_else | 12 |
| expect_used | 12 |

Highest measured file totals include `pkg-nix/src/broker.rs` (78),
`pkg-nix/src/contract.rs` (54), `pkg-nix/src/build.rs` (41), and
`pkg-pipeline/src/commit.rs` (33). These totals mix documentation and complexity.
They are not a defect ranking.

TraceDecay was used to identify code paths for direct inspection. Its freshness
changed during local compiler-config measurements, so graph scores and alleged
dead functions are not used as release criteria or deletion evidence. Direct
inspection confirmed that `acquire_install_evidence` combines acquisition,
approval, build execution, progress, evidence, and cancellation. Broker drop
warnings include lock guards; changing their lifetimes needs behavior review.
Static test attribution is not executed line or branch coverage.

Priorities for [issue #4](https://github.com/spa5k/pkg/issues/4):

1. Fix the reproduced public-result and onboarding gaps above.
2. Add focused regressions at the actual adapter, installer, and CLI boundaries.
3. Reduce documentation debt and refactor complex live functions in small changes.
4. Implement the missing ADR 0005 pattern counter with documented exclusions.
5. Resolve target dependency diagnostics, then enable the compiler lint.
6. Reconcile sanctioned invariant `expect` sites with the touched-file policy
   before enabling strict touched-file enforcement everywhere.

The audit VM was shut down after successful reinstallation and the final fzf
check. It remains available as `pkg-onboarding-20260926`. Raw logs remain in
`/tmp/pkg-onboarding-20260926` on the development host. The checked-in transcript
and command manifest retain the review evidence.

## Assurance and limits

The current main CI and alpha.54 public checks passed. The separate latest
[repeat-proof run](https://github.com/spa5k/pkg/actions/runs/33898972544) failed,
and the inspected [nightly run](https://github.com/spa5k/pkg/actions/runs/36109155550)
was cancelled before required fault and real-Nix jobs. These gaps remain in
[VERIFY-01](../../plans/determinate-nix-stacked-prs.md).

This is one native macOS VM audit. It does not add native Linux reboot evidence,
prove every crash boundary, test damaged-package recovery, establish production
key custody, or certify Developer ID signing and notarization. Normal sudo used
the disposable image's configured account; this is not a test of every macOS
administrator or GUI permission setup.
