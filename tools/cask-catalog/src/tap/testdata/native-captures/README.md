# native-captures: real reader-capture regression fixtures

Committed, self-contained evidence for `tap::capture_cases`. Every test
in that module reads ONLY this directory: no `/tmp` ambient evidence, no
skipped runs, no mocked classifier results.

## Provenance (exact native pins)

Each `captures/<system>/<case>.json` file is the `capture` envelope
(`pkg-tap-raw-export/1`) that the actual pinned upstream reader produced
on a NATIVE runner inside the sandboxed Nix export build. Both platform
runs used the same pins:

| Pin | Value |
| --- | --- |
| Homebrew | `cc9ff034b1d98cdd4061f68cf715bb967c483a8f` |
| Ruby | `4.0.5` |
| Fixture tap revision | `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa` (synthetic fixture tree) |
| Probe marker | `denied: host-read, host-write, loopback` |
| Native systems | `x86_64-linux` (Linux runner), `aarch64-darwin` (macOS runner) |

Parent evidence archives: `fixture-linux.tar.gz` and `fixture-mac.tar.gz`
(JSON runner results; each `capture` field is the reader envelope). The
source trees under `sources/` were regenerated with the DATA-ONLY writer
`public-tap-check/make_fixtures.py` (matrix v4, generator v4) from
`fixture-v4-source.tar.gz`; the writer never executes Ruby. The Ruby
files are data only and never run in tests.

Every `pkg_origin.sha256` in every committed capture was verified against
the SHA-256 of the corresponding committed source file.

## Normalization applied to the captures

Only the `capture` envelope is kept. Runner fields (`stdout`, `stderr`,
returncode, canary flags, Nix store out-paths, build logs) are dropped.
Inside the envelope the only further normalization is: ANSI escape
sequences are stripped and the sandbox build mount prefix
`/build/stage/brew/Library/Taps/` is replaced with `<tap-root>/` (this
affects load-error details only). Nothing else is edited; the envelopes
are byte-faithful reader output otherwise.

## Layout

- `captures/x86_64-linux/` — dup, url-localhost, url-private,
  url-linklocal, url-ipv6, release-archive, raw-binary, staging,
  invalidation-alpha-v1, invalidation-alpha-v2
- `captures/aarch64-darwin/` — representative subset: dup, url-localhost,
  release-archive, raw-binary, invalidation-alpha-v1,
  invalidation-alpha-v2
- `sources/<case>/` — the exact fixture tap tree for each case (the tree
  inventory the importer derives from the real codeload archive)

## Case intent

- `dup`: the reader exports both files of a duplicate token; conversion
  must refuse the whole capture.
- `url-*`: literal `http://localhost`, RFC1918, `169.254.169.254`, and
  `[::1]` destinations; the catalog destination policy must exclude them
  with a reason on both targets (no DNS lookup).
- `release-archive` / `raw-binary`: eligible archive and naked-container
  records with real plans.
- `staging`: `staged_path` load-error row must retain validated origin;
  the healthy control in the same tree stays eligible.
- `invalidation-alpha-v1` (helper `VERSION 1.4.0`) vs `alpha-v2`
  (`1.4.1`): the cask file is byte-identical; only `lib/pubtap_helper.rb`
  differs, and the recorded catalog version must change
  (whole-tree identity).

## Normal-mode legacy flights capture (macOS)

`captures/aarch64-darwin/flights.json` plus `sources/flights/` come from
the same fixture harness in NORMAL mode (no special reader options): a
legacy cask using `preflight`/`postflight` Ruby callback blocks and a
healthy binary control in the same tree. The reader emits the callbacks
as null-valued artifact keys; conversion excludes that cask while the
control stays eligible. Provenance and normalization are identical to
the table above.
