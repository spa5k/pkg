# Public taps: real installs — 2026-09-29

**Passed:** four applications from three public taps installed through the real
`pkg` client in a clean Linux x86_64 E2B sandbox. Three packaging defects were
found, fixed, and tested again. The changes are in [PR #82](https://github.com/spa5k/pkg/pull/82).

| Package | Version | Actual use |
| --- | --- | --- |
| GoReleaser | 2.18.1 and 2.18.2 | Validated a release config. Built a tiny Go program with module downloads disabled. The program printed `pkg-real-install: 42`. Upgraded, rolled back to the exact old Nix output, and upgraded again. |
| vpxtool | 0.34.7 | Created a 20,480-byte VPX table. Verified it, read its info, and extracted eight valid JSON files. |
| DNSControl | 5.2.0 | Checked a local config and rendered the expected A, CNAME, and TXT records. Rejected an invalid IPv4 address. |
| PinOne Config Tool | 2.2.2 | Installed the renamed AppImage through the Nix wrapper. Its bundled Electron runtime executed an arithmetic check and returned 42 without host FUSE. |

The five unique vendor files total **215,554,198 bytes (205.6 MiB)**.
Their actual Nix store bytes matched the pinned hashes and sizes. This total
excludes Nix runtime downloads, source metadata, and retry traffic.
[Exact source revisions, URLs, hashes, and sizes](candidate-facts.json).

## Defects repaired

1. **Compressed manuals failed installation.** The builder now accepts one
   known compression suffix after sections 1–8. The real `goreleaser.1.gz`
   installed and decompressed. Its bytes still match the vendor archive.
2. **A binary AppImage needed host FUSE.** A single Linux binary with a direct
   `.AppImage` URL now uses the existing Nix AppImage wrapper. The declared
   rename stays exact. Archive and mixed-artifact cases are refused. The
   real wrapper ran Electron 32.2.0 and Node 20.18.0 with no stderr.
3. **Shell completion files had wrong names.** Names now follow the pinned
   Homebrew rules. Collision checks compare files in the same destination
   group. Zsh `compinit` changed from `registered=missing` to
   `registered=_goreleaser`. Bash registered its function. Fish returned
   real `build` and `check` suggestions. The official catalog regeneration
   changed only Sauce Connect's Bash and Zsh target names.

## Lifecycle checks

- The three imports produced 14 eligible records and one exclusion:
  `goreleaser/tap` 6/0, `gitfool/vpinball` 7/1, `dnscontrol/tap` 1/0.
- Removing one package kept the exact other Nix outputs. Rollback restored
  the exact set of three installed packages.
- Removing the DNSControl tap retained its installed binary. Explicit
  upgrade from the removed source failed. Bulk upgrade skipped it.
  The binary still worked. Re-adding the exact pin retained its installed
  entry and source binding.
- GoReleaser's real version upgrade and rollback kept the other two
  packages unchanged.
- The final four-package profile had the correct qualified names and
  installed entry bindings. All four packages and all three taps were
  removed successfully after verification.

## Environment and checks

The verifier used the real official cask source at
`/home/user/workspace/nix/casks` and nixpkgs commit
`567a49d1913ce81ac6e9582e3553dd90a955875f`. HOME and XDG directories were
isolated under `/tmp/pkg real installs`. The state path contained spaces.
The disposable Nix daemon used strict sandboxing with fallback disabled.
No model worker ran public tap Ruby or downloaded applications. GLM 5.3
workers wrote fixes in separate boxes. The parent reviewed the code and
ran the real installations. The user's host daemon was not changed.

Final Linux checks: **192 Rust tests passed**, with formatting, strict
Clippy, and strict private Rustdoc. The Python plan suite produced **51
passes**. Python formatting passed. Ruff still reports the same six
pre-existing diagnostics; there are no new diagnostics. See the
[baseline comparison](lint-baseline.json). Catalog tests, strict Clippy,
formatting, and Rustdoc also passed locally on macOS. This was not a new
macOS payload-install run. Strict OpenSpec validation passed.

## Evidence and limits

- [Summary and tested source hashes](summary.json).
- [Raw E2B logs, captured catalogs, fixtures, and results](e2b-evidence.tar.gz).
- [Exact executed command transcripts](executed-commands.tar.gz). These are
  records from a disposable E2B box, including failed attempts and their
  later corrections. They are not a general test runner or host setup guide.
- [Supplemental macOS checks](macos-checks.tar.gz).

A repeated GitHub tap refresh returned HTTP 403 after earlier imports had
passed. The final run used that tap's saved pinned catalog. Its installed
AppImage still ran. The records retain this failure, the initial packaging
failures, and the GoReleaser fixture correction that added a dummy git remote.

vpxtool reports a malformed VPX file on stderr but returns zero. That is an
observed upstream CLI limitation. The test records do not treat its exit
status as proof of valid input.

No GUI operation, USB or pinball hardware use, DNS publishing, or release
publishing is claimed. The AppImage test used `ELECTRON_RUN_AS_NODE=1`.
All test sandboxes were stopped after the evidence was saved.
