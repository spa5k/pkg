# Pre-merge cask stress checks

PR [#81](https://github.com/spa5k/pkg/pull/81) stays open. This run found and
fixed defects in the generator, Linux launcher names, and archive extraction.
It did not download or install the top 1,000 applications.

The starting tree was `d627f92ee461d5d14941baadbc70e2bda0943a72`.
The results below include the fixes added by this review.
[Source hashes](cask-stress-2026-09-29/source-sha256.json) identify the checked
files. Four E2B workers used Z.ai **GLM 5.3**, not GLM 5.3 Flash. The parent
reviewed their patches and repeated the combined checks. The workers used
the clean Rust/Nix snapshot created in the previous review.

## Popularity and catalog coverage

The [Homebrew analytics API](https://formulae.brew.sh/docs/api/) supplied
the [90-day cask install counts](https://formulae.brew.sh/api/analytics/cask-install/homebrew-cask/90d.json).
The response covers 2026-06-30 through 2026-09-28. It contains 7,824 tokens
and 5,251,679 installs. Counts for each token are summed, then sorted by
count descending and token ascending. These are global Homebrew counts.
They are not platform-specific install counts.

| Scope | macOS ARM64 eligible | macOS excluded | Linux x86-64 eligible | Linux excluded | Missing tokens |
| --- | ---: | ---: | ---: | ---: | ---: |
| Top 1,000 | 625 | 372 | 67 | 930 | 3 |
| Entire 7,709-record catalog | 3,144 | 4,565 | 160 | 7,549 | — |

The three missing popularity tokens are `homebrew-app`, `crisp`, and
`unsloth`. They were not replaced with lower-ranked tokens. Eligibility is
a metadata decision. It does not establish that the vendor payload builds.

- [All 1,000 ranked results](cask-stress-2026-09-29/top1000.csv)
- [Metadata report, exclusions, and provenance](cask-stress-2026-09-29/corpus-report.json)
- [Exact analytics input, gzip compressed](cask-stress-2026-09-29/analytics-90d.json.gz)
- [Selected token list](cask-stress-2026-09-29/top1000-tokens.json)

The uncompressed analytics SHA-256 is
`d657b63fd99f7c2260a905bf955d33fd0351f8d63e9f45ac22603b75273afab7`.
The catalog SHA-256 is
`956ae8a7c698c6abe6ccc9bb642e85d2f8697b9a907f82dad653917080c1433f`.
The generator still emits byte-identical catalog data after these fixes.
The full metadata audit found zero invariant violations.

## Defects and regression proof

| Defect before the fix | Result after the fix | Retained check |
| --- | --- | --- |
| Nix rejected spaces in AppImage launcher names. 23 of 160 Linux derivations failed. | All 160 evaluate. Names remain exact. Shell quoting protects quotes, substitutions, backticks, semicolons, and leading dashes. | `appimage_names.py`: real Beaver Notes and Unity Hub plans, hostile names, invalid names, and exact diagnostics. |
| A nested AppImage source produced a launcher name with `/`. | The launcher uses the metadata rename or source basename, without `.AppImage`. | Rust nested-source regression. |
| `depends_on.linux: null` excluded macOS. | Null clears the requirement. A non-null requirement still excludes macOS. | Rust null/non-null regression. |
| ASCII controls could enter source paths, target names, and fetch URLs. | The generator excludes these records. | Two Rust regressions with C0 and DEL examples. |
| ZIP link targets were read without a size bound. | Targets over 4,096 bytes are refused before reading. NUL, LF, and CR are refused. | Oversized compressed target and corrupt-target fixtures. |
| A tar special member could fail after earlier files were written. A gzip fallback could then accept the refused archive as a binary. | Special members and tar hard links are refused before extraction. No partial payload is written. | Real tar and tar.gz fixtures; FIFO, device, and hard-link cases. |
| Duplicate manpage or completion targets overwrote the first output. | The second write fails. | Real install calls verify failure and the first file's contents. |
| `./Applications` did not match the normalized shortcut exclusion. | ZIP, tar, cpio/xar, and 7z use the same normalized member key. | Real ZIP/tar shortcut fixtures and existing cpio/xar checks. |
| Some real 7z symlinks use `Attributes`, with no `Mode` or `Symbolic Link` field. | Those links enter the pre-extraction link check. | Real `7zz -snl` archives with safe and escaping links. |
| APFS DMG listings contain an empty `Symbolic Link` field for ordinary files and directories. These were treated as dangling links. | Empty fields no longer change normal files into links. Actual empty link targets still fail. | A native 17,777-byte DMG with a real app layout, internal link, and `/Applications` shortcut. |

The four new generator regressions failed on the old code: 35 passed and
four failed. All 39 generator library tests pass after the fixes. The
AppImage derivation failures and the native DMG failure were also reproduced
before the changes. The archive worker reproduced the unbounded ZIP read,
tar partial write, and duplicate output before fixing them.

The hard-link rule is deliberately strict. Even an in-tree tar hard link is
unsupported. We did not add a second path resolver to support it.

## What ran

| Check | Result | What it proves |
| --- | --- | --- |
| Nix evaluation of every eligible `.drvPath`, with IFD disabled | 3,144 macOS + 160 Linux, zero failures | Every eligible plan can form a derivation. No vendor payload is fetched by this check. |
| Whole-catalog synthetic replay on Linux | 2,822 passed: 2,778 macOS plans + 44 Linux plans | Tiny payloads follow real declared paths through the real unpack/install helper. Regular and linked commands execute and return the expected marker. |
| Top-1,000 synthetic replay on native macOS | 575 passed: 551 macOS plans + 24 Linux-shaped plans | Same helper contract on macOS. Linux-shaped shell fixtures do not prove Linux binary compatibility. |
| Archive regression script on Linux and macOS | 37 PASS lines on each; no failures | Real ZIP, tar, 7z, xar/cpio/gzip, native APFS DMG, links, collisions, and package refusals. |
| AppImage name checks | 30 passed | Real Nix-generated rename commands preserve names and do not execute name content. No AppImage payload runs here. |
| Nix builder checks on Linux | 13 passed | App rename/CLI/docs, relocatable pkg payload, static CLI, dynamic ELF repair, and expected refusals. |
| Workspace Rust checks | 86 tests passed; build, fmt, Clippy, and rustdoc passed | Generator and client regression checks on Rust 1.96.1. |
| New and changed Python files | Ruff rules `E4,E7,E9,F,B,UP,SIM` and format passed | All five checked Python files pass the stated lint scope. |
| Nix files | nixfmt, Statix, and deadnix passed | The flake, builders, and fixture definitions pass their lint checks. |

The full synthetic run excludes 366 macOS pkg plans and 116 Linux AppImage
plans. The top-1,000 run excludes 74 pkg plans and 43 AppImage plans. Those
formats have separate focused checks. No pkg or AppImage is counted as a
successful synthetic replay. Other excluded metadata records also do not run.

The Linux workers used Python 3.13.13 and 7-Zip 26.01. The native runner was
a disposable macOS 15.7.7 ARM64 VM with Determinate Nix 3.22.1 / Nix 2.35.2.
The archive tools came from the pinned Nixpkgs input.

Evidence:
[derivation counts and former failures](cask-stress-2026-09-29/derivations.json),
[full replay](cask-stress-2026-09-29/replay-all.json),
[macOS replay](cask-stress-2026-09-29/replay-mac.json),
[Linux archive log](cask-stress-2026-09-29/archive-linux.txt),
[macOS archive log](cask-stress-2026-09-29/archive-macos.txt),
[AppImage log](cask-stress-2026-09-29/appimage-names.txt),
[Nix builder log](cask-stress-2026-09-29/nix-builders.txt).

## Test-script defects also corrected

The first replay script used alphabetical catalog order as a popularity
subset. Its token filter did not limit execution. Missing tokens caused a
crash. Failure and skip results still returned exit code zero. These errors
were corrected before the reported runs.

Controls verified that an unresolved `$APPDIR` and a traversal source return
nonzero status. A real selected-token run reported missing tokens without
substitution. A macOS run then exposed a `/var` versus `/private/var` path
comparison error. It falsely rejected 66 valid links. Both sides now use
resolved paths; all 575 applicable plans passed on the repeated macOS run.
The metadata report also counted each analytics token twice, once per target.
It now counts each token once and checks this with an independent example.

These are test corrections, not additional package compatibility fixes.

## Homebrew scenarios and future taps

Six scenarios were adapted from
[Homebrew/brew at `cc9ff034`](https://github.com/Homebrew/brew/tree/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/Library/Homebrew/test/cask).
They cover an app rename, ARM64 scope, an inert pkg option, app plus binary,
Linux AppImage, and whole-array Linux variation replacement. The full
BSD-2-Clause license is retained. The [fixture guide](../../tools/cask-catalog/AUDIT.md)
describes the adaptations. The real Rust classifier checks all 12
record/system pairs in normal Cargo CI. The upstream Ruby test suite was
not run. Installer execution, receipts, token migration, and Brew command
options remain outside our contract.

A separate synthetic public-source pin was tested through the compiled
generator CLI. Its macOS app plan and Linux binary plan retained the source
URL, revision, hash, and license. Duplicate bare tokens and a slash-containing
token were refused. Both failures left the previous catalog bytes unchanged.

This proves that the generator can consume another pinned JSON source in
the accepted schema. It does not implement raw Ruby tap ingestion or merging
several taps. Future tap support must retain these rules:

1. Give each source a stable identity, revision, content hash, and license.
2. Convert each source to the same metadata schema. Keep package rules generic.
3. Define namespaces and collision errors before combining catalogs. Current
   token keys do not accept `tap/token`; that needs an explicit schema change.
4. Apply the same per-target exclusions and archive checks to every source.
5. Keep source trust separate from content hashes. A hash fixes bytes; it does
   not establish that a source or application is trustworthy.

The supported targets remain ARM64 macOS and x86-64 Linux. This run does not
add Intel macOS or ARM64 Linux support.

## Cost and limits

Bulk checks used metadata, Nix evaluation, and tiny synthetic files. They
downloaded no vendor applications. The existing 13-case Nix builder smoke
test includes one small real CLI, 1Password CLI 2.39.0. Nixpkgs, build tools,
and upstream test source were also fetched where not cached. This is not a
claim of zero network traffic or a measured E2B invoice.

Catalog eligibility still does not guarantee a working vendor archive.
Changes in archive layouts, dynamic library needs, signatures, launch
behavior, and upstream metadata can still cause failures. No GUI session or
bulk vendor install was tested in this run. The earlier signed-app and
native profile lifecycle proof remains in the
[implementation report](2026-09-28-rust-cask-catalog.md).

There is no general decompression size quota. This run bounded ZIP link
targets, not all archive resource use. The bsdtar text listing parser still
has whitespace-name ambiguity; tested examples were refused later, and no
escape was reproduced. Native pbzx package coverage comes from the earlier
implementation run; these new package fixtures use gzip payloads.

Rust regressions and adapted Homebrew scenarios run in the existing CI.
The Python/Nix stress scripts are explicit maintainer commands. They are
not a new scheduled service or an automatic full-catalog build gate.

## Repeat the cheap checks

See the [metadata audit guide](../../tools/cask-catalog/AUDIT.md) for input
paths and provenance options. Decompress the committed analytics snapshot
for the same ranking. Fetch the raw metadata from the committed input pin.

```sh
cargo test --locked -p cask-catalog
python3 tools/cask-build-check/plan_tests.py
python3 tools/cask-build-check/appimage_names.py
python3 tools/cask-build-check/catalog_plans.py \
  --catalog nix/casks/catalog/catalog.json --jobs 2
python3 tools/cask-build-check/catalog_plans.py \
  --catalog nix/casks/catalog/catalog.json \
  --tokens docs/verification/cask-stress-2026-09-29/top1000-tokens.json --jobs 2
bash tools/cask-build-check/run.sh
```

Use Python 3.13 or later, `7zz`, `bsdtar`, `file`, and the pinned Nix tools.
Missing tools, zero applicable plans, failed plans, or fixture construction
errors must fail the check. Missing popularity tokens are reported separately.
