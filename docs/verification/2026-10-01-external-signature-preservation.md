# Preserve external app signatures — 2026-10-01

This records the initial repair. The later
[shared storage update](2026-10-01-shared-app-storage.md) replaces its full
home-folder copies with same-volume APFS clones.

Calibre 9.13.0 and Obsidian 1.13.7 now install and start with their original
vendor signatures. This completes NR-08 in [PR #98](https://github.com/spa5k/pkg/pull/98).
It replaces the refusal recorded in the earlier [15-app test](2026-09-30-real-app-stress.md).

## Cause and repair

These apps store a data file's signature in `com.apple.cs.*` extended
attributes. Calibre uses them on `python-lib.bypy.frozen`. Obsidian uses them
on `obsidian.asar`. Nix store import removes the attributes. This breaks
Calibre's deep bundle verification. Obsidian's main bundle still verifies,
but its separately signed resource no longer verifies.

The independent native control downloads both pinned vendor archives and
checks their SHA-256 values. It imports each original bundle with
`nix store add-path`, then restores the original attributes onto a copy.
Both resources and both bundles verify after restoration. File bytes and
attribute bytes match the vendor originals. This confirms the repair
without relying on pkg's extractor.

The builder now stores supported signature attributes as ordinary JSON data.
It moves the incomplete store app into a private payload location. It does
not expose that bundle in `Applications`. One runtime module serves app
sync and bundled commands. It restores the exact attributes on a private
versioned copy, verifies the bundle and every external signature, and then
publishes the completed copy. It anchors the main executable and plist to
the source output so a validly signed auto-update cannot change the package
version silently. No vendor app is re-signed.

The runtime uses Darwin's native attribute functions through ctypes.
macOS Python does not provide Linux's `os.getxattr` functions. It also
temporarily enables owner write permission on the app root for the final
rename. Darwin refuses that rename when the copied store directory has
mode 0555. The published directory retains its original mode.

## Final native checks

The final run used a disposable Apple silicon VM with macOS 15.7.7 and
Determinate Nix 2.35.2. It used a separate test home and native profile.

| Check | Calibre 9.13.0 | Obsidian 1.13.7 | Stats 3.0.13 control |
| --- | --- | --- | --- |
| pkg install and launcher sync | Pass | Pass | Pass |
| Deep strict bundle verification | Pass | Pass | Pass |
| External file verification and original attribute bytes | Pass | Pass | Not needed |
| Every original regular file, link target, and executable bit | Match | Match | Earlier vendor comparison retained |
| Duplicate install leaves the manifest unchanged | Pass | Pass | Pass |
| Startup through the launcher at the expected app path | Pass | Pass | Pass |
| Native exact-ID removal removes entry and launcher | Pass | Pass | Pass |
| Rollback restores entry, launcher, and startup | Pass | Pass | Pass |
| Damaged signing attribute is restored from the store | Pass | Pass | Not needed |

Calibre's installed `ebook-convert --version` reports 9.13.0. Obsidian's
installed command starts and reports that its optional CLI setting is
disabled. This establishes wrapper execution, not enabled Obsidian CLI
functionality. No Obsidian setting was changed.

The test also interrupts an observed Calibre copy. The materializer exits
130 and publishes no app. Its temporary directory is removed. Two concurrent
real preparation processes both succeed and return the same completed copy.
An unowned version directory and a symlinked version directory are refused.
Their sentinel files remain unchanged. The final package profile is empty.

## Regression and project checks

The existing archive test now creates a tiny real signed app on macOS.
Its data file has an external ad-hoc signature. It passes through a real
7zz archive, JSON capture, canonical read-only modes, cache restoration,
and a generated command wrapper. Native signature verification passes.
Arguments with spaces and literal shell characters arrive unchanged.
This synthetic fixture uses ad-hoc signing; vendor apps retain their
original signatures.

The fixture found that a valid ad-hoc signature can have an empty
`CodeSignature` attribute. That value is preserved. `CodeDirectory` must
remain nonempty. Orphan and symlink signing streams still fail before
payload writes. The unchanged acceptance test fails against the previous
extractor because it refuses the supported signature stream. The final
suite passes all 60 checks on the host and in the VM. macOS CI now runs this
same suite. Linux CI retains the archive and 14 native builder checks.

All 241 Rust tests pass. Build, format, Clippy, rustdoc, dependency policy,
installer checks, documentation links, OpenSpec validation, and whitespace
checks pass. The first sandboxed Rust test attempt could not open its test
listeners or Nix cache locks. The normal-access repeat passes.

## Source and limits

- Base commit: `bafd2c1af8da3723733319ea87a36cc6ef796f3f`.
- Final extractor SHA-256: `9ea022a6de672f1bb11b4b8a8ebcd02364bbd525bc037c70aed45ad5f6db80bd`.
- Runtime SHA-256: `1f2d157eabc5e28981b473c4d0c0c99632d7168c1bcc03b30bcb94587004fbb1`.
- Final client SHA-256: `7d646d7177bc1f1ebc5cc67527e9f5584fe578fafa39ff8f5a44cb2436315450`.
- Guest extractor, runtime, builder, test, and client hashes match the host.

An early VM attempt ran out of guest disk space. The task-owned virtual
disk and APFS container were expanded. The final run completed with enough
space. An intermediate startup check found that macOS reused an Obsidian
process from the preceding test. Those task processes were stopped before
the final clean-start repeat. These attempts are recorded as controls.

Only regular in-bundle files with the known signing attributes are supported.
Unknown, orphaned, and symlinked streams remain refused. Native signing
attributes are captured before copying. This adds no general AppleDouble
parser. An unsupported signature layout cannot be exposed without passing
runtime verification. Other GUI features and Gatekeeper quarantine were
not tested. The earlier 15-app run is not claimed as a new 15-app run.

The private cache uses one additional app copy per version. Removal removes
the native entry and launcher. Cached versions can remain for running apps.
Cache presence is not package inventory. A missing or damaged owned copy
can be recreated. The user's installed packages and other VMs were unchanged.

The [evidence archive](external-signatures-2026-10-01.tar.gz) contains the
native controls, final lifecycle records, source hashes, regression controls,
and project logs. It excludes vendor payloads, broad process snapshots,
credentials, and model traces.

Apple describes this signature format in
[TN3126](https://developer.apple.com/documentation/technotes/tn3126-inside-code-signing-hashes).
The implementation follows the [NR-08 design](../../openspec/changes/harden-native-package-lifecycle/external-signatures.md).
