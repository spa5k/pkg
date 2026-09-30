# Shared app storage — 2026-10-01

The external-signature repair now shares app file data with the Nix store.
It no longer creates a full second set of app data. This updates
[PR #98](https://github.com/spa5k/pkg/pull/98) after the
[initial signature repair](2026-10-01-external-signature-preservation.md).

## Storage design

The shared runtime uses Darwin's `clonefile` for each regular file. Each
file gets a separate inode and independent attributes. APFS retains shared
data blocks until a private write changes them. Bundle links remain links.
The runtime restores the original vendor signatures on this private view.
It retains the same verification, ownership marker, output lock, atomic
publication, repair, and cancellation behavior. It never re-signs vendor apps.

The cache is `/nix/var/pkg/cask-apps/<uid>/`. `pkg apps setup` creates the
root-owned parents and a user-owned directory with mode 0700. Only this
explicit setup command requests administrator access. Repeating it checks
the existing directories and makes no change. Normal app use and sync are
unprivileged. A custom `PKG_APP_CACHE_DIR` must be absolute, private,
user-owned, and on the payload's volume.

There is no full-copy fallback. Missing setup, unsafe cache paths, a
different volume, or an unavailable clone operation stops preparation.
Both Rust launcher discovery and bundled commands calculate the same path.
No administrator setup was performed on the developer's Mac.

## Real app verification

A disposable Apple silicon VM ran macOS 15.7.7 with Nix 2.35.2. It used
an isolated home and native profile. Independently downloaded vendor DMGs
were SHA-256 checked. Their original apps were imported separately with
`nix store add-path` for the vendor-file comparison. The checked DMG bytes
were also seeded under the native fixed-output payload name, avoiding a
second download without changing the cask's source plan or builder.

| Final runtime check | Calibre 9.13.0 | Obsidian 1.13.7 |
| --- | --- | --- |
| Original regular files, executable bits, and link targets | Match | Match |
| Original external signature attributes | Match | Match |
| Deep strict vendor signature verification | Pass | Pass |
| Every mapped regular-file extent shares source blocks | Pass | Pass |
| Total regular-file bytes shared | 1,152,794,295 | 538,580,072 |
| Regular files with separate inodes | 1,446 | 278 |
| Startup through the generated launcher | Pass | Pass |
| Duplicate install and explicit sync | Pass | Pass |
| Removal, rollback, and restored startup | Pass | Pass |
| Damaged signature attributes repaired | Pass | Pass |
| Private data write leaves store bytes unchanged | Pass | Pass |
| Damaged private data repaired | Pass | Pass |
| Different-volume preparation and setup refused | Pass | Pass |

The physical sharing check uses `F_LOG2PHYS_EXT` with Apple's packed
`log2phys` structure. It compares every mapped extent of every regular file
in the final runtime view against its store payload. All 1,691,374,367 bytes
are shared. This is stronger than `du`, which counts shared blocks in each
view. Metadata and restored attributes still allocate space; no exact
metadata-only byte count is claimed. Background disk activity made free
space deltas unsuitable for that measurement.

The Stats 3.0.13 control also passed installation, signature verification,
startup, removal, and rollback through its ordinary store bundle. Calibre's
`ebook-convert --version` reports 9.13.0. Obsidian's command reports its
optional CLI setting is disabled. That setting was not changed.

Setup and repeat setup succeed. Cancellation during an observed Calibre
clone exits 130, leaves no published view, and removes its staging tree.
Two concurrent real callers return the same verified path. Unowned and
symlinked version directories are refused; their sentinels remain unchanged.
The final native package profile is empty.

## Regression and limits

All 242 Rust tests pass. Build, format, Clippy, rustdoc, local documentation
links, and strict OpenSpec validation pass. All 60 native archive checks pass
on the host and in the VM. The existing signed-app acceptance test now checks
separate inodes and physical extent sharing, original signature bytes,
read-only publication, and exact bundled-command arguments. It runs in
macOS CI. A focused Rust test rejects shared permissions, a different owner,
and a symlink for private storage.

Transparent compression was also tested before choosing clones. It reduced
Calibre's allocated app space from 1,167,613,952 to 497,463,296 bytes, and
Obsidian's from 542,814,208 to 259,084,288 bytes. Both apps verified and
started, but this still duplicated hundreds of megabytes. It is not a
fallback in the shipped implementation.

The shared-data claim applies while the source blocks remain shared. Private
writes allocate new blocks. If Nix later collects an old source, its remaining
clone retains the blocks. Old views can therefore retain space until removed;
this change adds no automatic cleanup of old views. Earlier development
builds' home-folder copies are not migrated automatically. This change
requires the matching client and cask runtime to ship together.

The [evidence archive](shared-app-storage-2026-10-01.tar.gz) contains native
lifecycle and sharing records, source hashes, regression and project logs,
and the measurement scripts. It excludes vendor payloads, broad process
snapshots, credentials, and model traces. The task-owned VM was removed
after collecting the evidence.

Apple documents same-volume clones and their independent later writes in
[About Apple File System](https://developer.apple.com/documentation/foundation/about-apple-file-system).
The native API and extent layout were checked against the installed Apple
SDK headers and `clonefile(2)` manual. The
[NR-08 design](../../openspec/changes/harden-native-package-lifecycle/external-signatures.md)
records the setup and no-full-copy requirement.
