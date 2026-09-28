# nix/casks/lib — provenance and licenses

## What lives here

- `builders.nix` — generic data-driven builders over `pkg-cask-catalog/2`
  plans (no per-token files, lists, or overrides; vendor strings never
  become shell syntax; the plan travels as JSON into `plan.py`).
- `plan.py` — stdlib-only build-time helper: content sniffing, pre-write
  member and symlink validation, safe extraction, artifact placement,
  read-only plist checks (XML and binary via `plistlib`), pkg structural
  rules, post-install link audit.
- `brew-nix-LICENSE` — full MIT license text of the ported upstream.
- `licenses/homebrew-cask-LICENSE.txt` — actual BSD-2-Clause license text
  of the Homebrew cask data.

## Ported upstream: brew-nix (MIT)

Code in `builders.nix`/`plan.py` ports archive dispatch, DMG volume-nesting
handling, bundle placement, binary linking, and the xar/pbzx/cpio pkg
payload extraction from:

- Repository: <https://github.com/BatteredBunny/brew-nix>
- Ported revision: `16131ae4126c54b1502aa7eaf6573d7fbf16b656`
  (this is the same revision previously consumed as a flake input; the
  input itself was removed and replaced by this port)
- License: MIT — full text preserved verbatim in `brew-nix-LICENSE`.

## Cask metadata: Homebrew / homebrew-cask (BSD-2-Clause)

The generated catalog (`../catalog/catalog.json`) is derived from Homebrew
cask data via the `BatteredBunny/brew-api` mirror snapshot pinned in the
catalog's `input` provenance block. The mirror repository itself carries no
LICENSE file at the pinned revision; the underlying data license is
Homebrew's BSD-2-Clause, whose actual text we preserve in
`licenses/homebrew-cask-LICENSE.txt` (fetched from
<https://github.com/Homebrew/homebrew-cask>, `main` branch, file `LICENSE`,
sha256 `431d9708f8f5009fe4f5518790e471ad589c23e4350f53404e5620dcc5a5048f`).
Attribution is also embedded in every generated catalog's `input.license`
field; do not strip it when regenerating or vendoring data.

## Archive limits

Tar hard links and special files are unsupported. They are refused before
extraction. ZIP symlink targets are limited to 4,096 bytes. This is not a
general archive size quota. See the
[stress-check report](../../../docs/verification/2026-09-29-cask-stress.md)
for verified formats, regressions, and remaining limits.
