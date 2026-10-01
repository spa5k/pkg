# Search and real-use repair verification

Date: 1 October 2026. Client: `0.2.0-alpha.10`, built from the changed
workspace. Base commit: `1f4903788d595a15293a29a29bdb1abd229d6b9d`.
This work implements [the repair change](../../openspec/changes/repair-realistic-use-faults/proposal.md).
The [original findings](2026-10-01-realistic-use.md) remain unchanged.

All implementation and verification tasks are complete. All required local
repository checks pass. The installed clients passed the Linux and Mac
command workflows. Actual, Obsidian, and Calibre opened after removal and
explicit restore. Saved data remained usable. The final Mac visual checks
used Tart's built-in display.

The [evidence archive](search-and-real-use-fixes-2026-10-01.tar.gz) contains
commands, exit codes, memory samples, native inventories, complete search
output, the final SQLite database, screenshots, original synthetic data,
source hashes, build source, and check logs. Large Nix source-cache object
files and duplicate full search outputs were excluded. The archive contains
one complete full search output. Neither sandbox received model keys or
host account credentials. Both E2B sandboxes were stopped after download.
The task-owned Tart VM and its downloaded base image were removed after
the final evidence was saved.

## Environment and source identity

Two E2B sandboxes ran Debian 12, x86_64 Linux, with about 2 GB RAM and no
swap. Their IDs were `ilm8none5yh9ofaa1s0jc` and `irir4ou7s68sp2kchrbaw`.
The client was built in each sandbox and installed at
`/home/user/.local/bin/pkg`. Each ran external Determinate Nix 3.22.5 /
Nix 2.35.2. Local configuration selected this fixed Nixpkgs revision:

```text
github:NixOS/nixpkgs/567a49d1913ce81ac6e9582e3553dd90a955875f
```

The cask source was the changed local `nix/casks` flake. Its Nixpkgs input
uses the same revision. Its catalog generation identity is `34fcbb13d1c4`.
A local path is mutable. Its cask index is evaluated on each command.
The client hashes and source hashes are in the archive.

The Mac VM ran macOS 15.7.7, build 24G720, on Apple silicon. Tart 2.38.0
provided 6 CPUs, 16 GB RAM, an 80 GB APFS disk, and a 1280 by 900 display.
Its name was `pkg-real-use-rf`. Its base image was pinned to:

```text
ghcr.io/cirruslabs/macos-sequoia-base@sha256:3f4d14a5ffb9efd3bda2ae0184fd4bc2773d924ff8b7565f958761420ec41a0c
```

The official installer installed external Determinate Nix 3.22.5 /
Nix 2.35.2 in the guest. The local Darwin client was copied into the guest
and installed at `/Users/admin/.local/bin/pkg`. Package mutations used guest
profiles. The host's installed packages were not changed.

## Search memory and completeness

The final database contains **108,714 native entries**. Every attribute,
package name, version, and description matches a native full-catalog oracle
at the fixed revision. There are no missing, extra, or different entries.
The complete broad result has 108,874 rows, including 160 eligible casks.
Native result IDs and order retain the existing rules. JSON keeps schema 1
and the `command` / `result` envelope.

The native oracle JSON is 18,167,780 bytes. The SQLite database is
22,589,440 bytes, or **21.54 MiB**. These are metadata sizes. They do not
explain the old 1.69 GiB evaluation peak. Nix also holds parsed expressions,
evaluation objects, and allocator storage. SQLite reduces query memory.
Small Nix batches and GC settings reduce evaluation memory.

| Real command | Seconds | Combined peak MiB | Evaluator peak MiB | Result |
| --- | ---: | ---: | ---: | --- |
| `pkg update`, empty discovery cache | 495.011 | 504.35 | 478.45 | Complete cache |
| `pkg --json search '^'`, no prepared cache | 461.784 | 504.04 | 477.07 | Complete catalog |
| `pkg --json search '^ripgrep$'`, first query after update | 1.265 | 223.07 | 201.72 | Pass |
| `pkg --json search ripgrep`, warm cache, final installed client | 1.981 | 206.87 | 184.44 | Pass |
| `pkg --json search '^'`, prepared cache | 10.413 | 216.70 | 195.34 | Complete broad result |
| `pkg info hello`, final installed client | 2.335 | 235.90 | 185.09 | Targeted resolution |
| `pkg info nixpkgs:python312Packages.requests` | 0.607 | 143.53 | 131.39 | Targeted metadata |
| Four simultaneous searches | 13.412 | 529.00 | 430.09 total | All four exit 0 |
| One deliberately oversized metadata entry | 3.784 | 496.48 | 481.54 | Exit 1; old cache retained |

The monitor sampled `/proc` every 20 ms. Combined peak is the sum of live
RSS for the command and its descendants at each sample. Independent client
and evaluator maxima are not added together. The four-command row includes
the Python driver and its JSON parsing. It is a conservative process-tree
measurement. It does not describe all VM memory. Source fetches are included
when they are command descendants. Nix store build services are separate.
No package build is needed to prepare the native discovery cache.

Cold discovery means an empty pkg discovery cache. Nix's source cache and
its prebuilt-image state are recorded separately. The final empty-cache
runs used an already fetched source. The first sandbox also fetched the
source before the first scan. The full native oracle ran on the host as a
read-only comparison. It did not run inside the 2 GB sandbox. It used about
4.80 GB peak RSS on that host. Host and sandbox timings are not directly
comparable.

The evaluator guard samples RSS every 5 ms. It stops at 480 MiB, leaving
32 MiB below the 512 MiB budget for growth between samples. RSS sampling
cannot measure every transient spike. The recorded final evaluator peaks
stay below 512 MiB. The SQLite page cache is 8 MiB. Memory mapping is off.
Matched output rows are spooled to disk.

Early runs found two implementation faults. A process could exit between
`try_wait` and the RSS read. A short grace period now handles that race.
An otherwise valid singleton, `tests.nixos-functions.nixos-test`, exceeded
the first GC settings. A smaller initial heap, more frequent collection,
and immediate allocator purging fixed the complete scan. An initial
512 MiB trigger also allowed a measured short peak above the budget.
The final guard adds headroom and samples more often. Failed runs and their
logs remain in the archive.

## Cache failure and name-resolution commands

These checks used an actual Git flake with real Nixpkgs hello derivations.
Fault cases use this controlled source. They are not substitutes for the
full-catalog runs above.

- A fatal `packages` metadata error made `pkg update` exit 1. The previous
  database hash stayed unchanged. Search returned its two complete stale
  rows with the previous revision.
- After repair, update succeeded. Replacing the SQLite file with corrupt
  bytes caused search to rebuild a complete current database.
- SIGINT during a new scan made update exit 130. The previous complete
  database hash stayed unchanged.
- A 4 MiB tmpfs caused SQLite to report `database or disk is full`.
  Update exited 1. The previous complete database hash stayed unchanged.
- A real derivation with an oversized generated description stopped the
  batch. The client split it, retried the single entry, failed the refresh,
  and retained the previous complete database.
- A broader `hello` lookup in the alias source found two current matches.
  `info` refused the ambiguity. Ordinary `hello` and nested qualified info
  in full Nixpkgs passed through targeted lookup.
- Four simultaneous full-source searches used `^`, `ripgrep`,
  `python.*requests`, and `openssl|sqlite`. All exited 0. All source reports
  were fresh. Their result counts were 108,874, 8, 76, and 345.

## Profile mutation and recovery

Each platform ran **20 independent concurrent install rounds**. Each round
started `pkg install nixpkgs:hello` and `pkg install nixpkgs:jq` together in
one new pkg profile. Every command exited 0. Native inventory held both
active attributes after every round: **20/20 Linux and 20/20 Mac**.

Both platforms also started remove, upgrade, install, and explicit rollback
commands together after installing hello and jq. All four commands exited
0. Native inventories and generation histories were read after completion.
Different lock acquisition orders can produce different valid final sets.
The checks do not assume one fixed order. History and pruning passed.

Repeated hello and repeated cask installs reported `Already installed`.
The Linux output-selection check prepared separate native `zlib^dev` and
`zlib^out` entries at distinct native priorities. A pkg default install found
the matching output among all active entries and kept both entries.
Two fixed Git revisions of the real hello alias source also kept separate
entries. Repeating each pin skipped it. The CLI grammar still rejects `^`
selectors. This change does not extend that grammar. Nix 2.35.2 itself can
skip an output change at the same native name and priority; that behavior
is recorded in the initial selection attempt.

Recovery checks populated the native default profile with hello. A separate
pkg profile installed Python 3.12. Python 3.13 then failed on its file
conflict. The printed native remove command included the exact pkg profile
path. The driver followed that printed command. The pkg profile changed.
The default profile JSON stayed identical. This passed on both platforms.

Native rollback remains unchanged. The client prints the destination
before the switch and reports the restored generation. Default rollback
can select an older retained package set after a branched history.
The commands documentation states this behavior.

## Linux Actual workflow

The changed AppImage runtime supplied DejaVu fonts and its Fontconfig file.
A fresh home had no `.fonts` or `.local/share/fonts` directory. No font files
were copied into it. Actual 26.8.1 opened under Xvfb and displayed readable
text. The driver created `Verification Checking` with a balance of 123.45.
It closed and reopened the app. The saved account returned.

The correct native entry, `casks`, was removed. The Actual command then
disappeared from the profile. `pkg rollback 1` restored it. The saved account
returned. A final launch through the normal `Actual` command, with no
sandbox option, displayed the account and balance. See
[the normal-launch screenshot](search-and-real-use-fixes-2026-10-01/actual-normal.png).

The first GUI driver used `--no-sandbox` while checking the sandbox runtime.
That flag is in the harness logs. It is not in the package wrapper. The
normal launch also passed. Xvfb printed GPU and D-Bus warnings, but the
window rendered and saved data remained usable.

An early removal used the display token instead of the native entry ID.
It failed without changing the profile. The corrected remove and explicit
restore were checked separately. The report does not count the earlier
restart screenshot as a successful removal and restore.

## Mac apps and tap setup

Obsidian 1.13.7 and Calibre 9.13.0 installed together in one profile at normal
priority. They opened together. See [both app windows](search-and-real-use-fixes-2026-10-01/mac-apps-open.png)
and [Calibre's main window](search-and-real-use-fixes-2026-10-01/mac-calibre.png).
A repeat install reported `Already installed` for both. Native inventory
still held exactly two entries.

Calibre's actual bundled `ebook-convert` command converted the original test
text into an EPUB. The ZIP passed integrity checks. Its HTML contained the
original saved text. Obsidian created a local vault through the graphical
console. A note displayed `saved data survives restore`.

`pkg remove casks casks-1` removed both entries and both launchers.
`pkg rollback 1` restored both. Explicit `pkg apps sync` passed. The note,
welcome note, and EPUB hashes stayed unchanged during remove and restore.
The apps remained running during that first command check.

For the final visual check, the guest was shut down and restarted with
Tart's built-in display. Both apps were then quit through their menus.
A process check confirmed that neither app was running. The restored pkg
launchers were opened from the guest terminal:

```sh
open /users/admin/applications/pkg/obsidian.app
open /users/admin/applications/pkg/calibre.app
```

Both created new main processes. [Obsidian showed the saved note](search-and-real-use-fixes-2026-10-01/mac-obsidian-restored.png).
[Calibre showed its saved library](search-and-real-use-fixes-2026-10-01/mac-calibre-restored.png).
The note hashes still matched. Calibre's restored bundled converter also
produced a valid EPUB under `Documents`. The original test EPUB was under
`/tmp`; macOS cleared that temporary file during the shutdown. Its earlier
remove/restore hashes and its downloaded copy remain in the archive.

Initial Finder input attempts did not start a process. Opening both
restored launchers from the guest terminal succeeded. The final evidence
records the launcher commands, new process IDs, native inventory, note
hashes, and post-restore conversion. Modifier keys through the VM input
were unreliable, so app menus were used to quit.

Deep strict `codesign` verification passed on both materialized vendor
bundles after restore. An initial signature command targeted the derived
pkg launcher stubs and failed. Those stubs are not the vendor bundles.
The corrected commands and exact bundle paths are in `vendor-signatures.json`.
Older schema-1 app layouts retain their reader and runtime path support.
Existing repository coverage passes. A real older-layout app installation
was not repeated in this run.

The real public tap was `goreleaser/tap` at revision
`77e442b6000587bf57e543081e95fd908b918bd7`. Setup was exercised in a terminal.
The driver approved the displayed setup steps inside the disposable VM.
The live service initially used the old temp directory. Setup preserved
`PKG_VERIFICATION_SENTINEL=keep-me`, set TMPDIR to `/nix/var/nix/builds`, and
used `bootout` plus `bootstrap`. Its effective grants passed. The mandatory
fresh sandbox build probe passed. Import returned six eligible packages.
**The VM was not rebooted for this import.**

The first setup attempt found `/private/var/tmp` in the default grants.
[Nix's pinned source defines both temp grants](https://github.com/NixOS/nix/blob/2.35.2/src/libstore/globals.cc).
Setup now removes only these two known mandatory self-mapped defaults.
A later override of old boolean settings is repaired from effective values.
Launchd also returned transient status 5 immediately after bootout.
The client now retries that specific bootstrap status for up to ten seconds.
The successful real run needed a short retry.

An added custom `/Users/admin` grant refused automatic setup and printed
manual instructions. The custom config hash stayed unchanged during the
refused command. The driver restored the test configuration afterwards.
Doctor reported healthy. Setup never accepted readiness from the two
boolean settings alone.

## Required checks and limits

The following all passed:

```text
cargo build --locked --all-targets
cargo test --locked
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps
sh -n install.sh
shellcheck install.sh
python3 docs/tests/test_install_script.py
python3 .github/scripts/check_docs_links.py
```

The installer checks ran 43 cases and skipped seven platform-specific cases
on the host. The real extraction-plan checks also passed in Linux with
actual 7zz and bsdtar. The host lacked 7zz for that separate harness.
The PR preparation also passed private-item rustdoc and the pinned
dependency policy check:

```text
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --document-private-items
cargo fetch --locked
cargo deny --locked --offline check bans licenses sources
```

Check logs are in `checks/` in the archive.

The local design meets the Linux memory and complete-catalog acceptance
checks. Cold preparation still takes about eight minutes. Repeated Nix
startup and evaluation across hundreds of batches remains its main cost.
Keep foreground `pkg update` as the preparation step. Profile that cost
before increasing batch sizes. Larger batches must retain the memory guard.
The mutable cask source also causes a fresh JSON-index evaluation on warm
commands. Storing those index rows in SQLite could reduce that remaining
cost in a later change. Installed state should remain native.

The Mac visual reopen checks passed in Tart. RF-08 is complete. Other Nix
versions and OS versions were not covered by this run.
