# External vendor signatures (NR-08)

Calibre and Obsidian store signatures for data files in macOS extended
attributes. Nix store registration and NAR transfer do not retain these
attributes. NR-07 refuses these archives to prevent silent signature loss.

The builder will preserve supported `com.apple.cs.*` values as ordinary
hex-encoded JSON data. It will keep the corresponding app payload outside
the output's `Applications` folder. It will not expose the incomplete
store bundle as an app. Other apps retain the existing store layout.

One materialization module will serve both launcher sync and bundled CLI
commands. It will copy the payload into a private versioned cache under
`~/Library/Caches/pkg/cask-apps/`, restore the original attributes, verify
the bundle and each externally signed file, and publish the completed copy
with an atomic rename. It will not re-sign vendor files or change security
policy. A per-output lock will serialize concurrent preparation. Failed or
cancelled preparation will not publish a copy.

The cache is derived state. Native Nix profiles remain the package inventory.
Removal removes the entry and launcher. Cached versions remain available
for running apps and can be recreated after cache deletion or rollback.
The cache may consume one additional app copy per version. Launcher status
will report a missing copy. Preparation must refuse an unowned or symlinked
cache destination. Public tap assessment will check the restored bundle
before launcher exposure.

Malformed, unknown, orphaned, or symlinked signature streams remain refused.
The builder will capture native signing attributes when extraction retains
them. This does not add a general AppleDouble parser. Runtime verification
must fail before app exposure if the supported capture did not preserve a
required signature.

Verification will use the pinned Calibre 9.13.0 and Obsidian 1.13.7 vendor
archives in a disposable macOS VM. Compare file bytes, link targets,
executable bits, external attribute bytes, and vendor signature identity.
Check GUI startup, bundled commands, duplicate install, sync, removal,
rollback, and damaged-cache recovery. Existing archive and CLI tests will
exercise the public behavior of the shared module.
