# CLI discovery verification — 2026-10-11

Change: [Improve package states and discovery](../../openspec/changes/archive/2026-10-12-improve-cli-discovery/proposal.md).
Working branch: `feat/cli-discovery-experience`.
Base commit: `befc18325acd1d0d374d05417e79d8c7e7e4c646`.
Client version: `0.2.0-alpha.12`, prepared for the next release.
Host: Apple silicon macOS 27.0.1.
Runtime: Determinate Nix 3.22.1, Nix 2.35.2.

## Behavior

The CLI tests run the compiled client with isolated paths and fake runtime
processes. They verify these outcomes:

- A broken profile link produces unknown installed state and exit code 1.
  Doctor reports the profile as unreadable.
- Installed package IDs and unique names resolve from the native manifest.
  Duplicate entries require an explicit `--entry` choice. Inactive entries
  are excluded. Nested Nixpkgs attributes keep their path.
- Literal `[` and `grep|searcher` are valid text queries. Invalid explicit
  regex input fails before runtime discovery.
- Exact names precede other names and descriptions. Human and JSON limits
  preserve the total match count. Conflicting limit options are usage errors.
- Exact excluded casks show their reason. Broad unavailable search is explicit.
- Prepared search succeeds after the fake Nix executable is replaced with
  an executable that fails every call. Cold search gives setup guidance.
- Failed refresh keeps complete data. Old catalogs carry their age state.
  Cached rows retain their own locked reference and revision.
- Broken tap state does not block Nixpkgs-only search. General search retains
  healthy results and reports missing sources.
- Installer setup passes the background opt-out correctly. Preparation
  opt-out skips setup. A setup failure retains the installed binary and
  reports incomplete setup.

## Real Nix probe

An isolated local Git flake supplied three derivations and the existing
cask index fixture. Its configuration, state, and cache used temporary
XDG directories. `pkg setup --no-background` succeeded in 0.507 seconds.
No package profile was created.

The runtime override was then changed to an unavailable path. Five searches
for `probe` succeeded in 6.30, 6.63, 5.80, 5.75, and 5.40 milliseconds.
Results placed the exact name before a prefix and a description match.
The cask query also succeeded without a usable runtime.

This is a small catalog probe. It does not measure preparation or query
time for the full Nixpkgs catalog. No full catalog speed claim is made.

## Checks

- `cargo build --locked --all-targets`
- `cargo test --locked --quiet -- --test-threads=1`
- `cargo fmt --all --check`
- `cargo clippy --locked --all-targets -- -D warnings`
- `RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps`
- `sh -n install.sh`
- `shellcheck install.sh`
- `python3 docs/tests/test_install_script.py`
- `python3 .github/scripts/check_docs_links.py`
- `git diff --check`

The Rust suite passed. One existing log-overflow timing test failed during
an earlier parallel run: it took 5.477 seconds against a five-second limit.
It passed alone in 0.29 seconds. The full suite then passed with one test
at a time. Its production code and time limit were not changed.

The installer suite passed 37 tests. Seven Linux isolation tests were
skipped on this macOS host because a private mount namespace was unavailable.
The macOS job fixture passed `plutil -lint`. Both job formats were checked
for argument escaping and catalog-only refresh. Live scheduler activation
was not tested. No user job was enabled on this host. Linux scheduler
activation still needs a Linux host check.

No release was published. The installed user package profile was not changed.

## Search quality follow-up

The matcher now shares relevance and visibility rules across Nixpkgs,
Homebrew Casks, and registered cask taps. CLI checks prepare both official
sources and publish a saved tap, then replace the runtime with a failing
executable. They verify combined search, source selection, qualified queries,
all-word matching, typo alternatives, result limits, and retained provenance.
A later source's literal result removes earlier fuzzy alternatives and resets
the count. Empty literal input is a usage error.

Normal `aws` results hide nested development scopes, library descriptions,
unwrapped variants, and versioned casks. `--broad` restores those matches.
An explicit library term can reach a matching root library. Exact IDs remain
usable. Desktop package scopes such as `kdePackages` stay searchable.

Install checks verify that a short alias cannot select an AWS library through
its internal name. The error suggests `nixpkgs:awscli2`. Exact candidates still
count for source ambiguity. Removing the prepared native catalog proves that
a cold short-name miss performs only two exact attribute probes and one cask
index read, without a full native scan or profile mutation.

For a larger check, existing saved Nixpkgs JSON data containing 113,402 rows
was converted to an isolated SQLite fixture. The existing Homebrew index was
copied into that temporary cache. The configured runtime pointed to an
unavailable executable. These are real saved rows, not a fresh preparation
measurement. Configuration and state were isolated; the user profile and
ordinary cache were not changed.

The release build placed `nixpkgs:awscli2` first. Its first 20 results included
the Homebrew AWS Vault cask and excluded `claws-mail`, `awstats`, the AWS C HTTP
library, the C++ SDK, and the Haskell/Python dependency sets. Normal `aws`
returned 101 matches; broad inspection returned 603. `ripgrpe` produced
approximate names; normal `firefox` search found both Nixpkgs and cask IDs.

Warm `aws` queries took 142.79–144.93 ms. The first process after rebuilding
took 699.81 ms. Cask-only queries took 18.82–19.08 ms. The typo query took
220.65–224.31 ms. These samples are local observations, not a cross-host
performance guarantee. [Raw results](2026-10-11-search-quality.json) record
the query options, counts, ordering, and samples.

The workspace Rust suite passed with one test at a time: 257 tests. The
affected client suite and CLI fixture passed again after the final matcher
and alias changes. Build, release build, Clippy, formatting, rustdoc, and
documentation link checks passed. No release was published or installed.

## Release candidate checks

The full Rust suite also passed with the default parallel test runner:
`cargo test --locked --quiet`. Rustdoc passed with private items included
and warnings denied. The installer suite passed again: 37 tests passed and
seven Linux isolation tests were skipped. Syntax, ShellCheck, documentation
links, and whitespace checks passed.

`tools/release/package_client.sh aarch64-darwin` produced the client archive
and collected license files for 164 third-party crates. The archive digest
was `aa4da7120e28bf3adb9d73d8dabcb535ee278763d60d5183d8def7eccb0cb42c`.
This is the local archive digest. CI produces the published archives.
