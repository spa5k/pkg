# Rust Cask catalog — end-to-end plan

Status: substantially implemented and verified on real hosts, with
verification results and limits recorded explicitly. This page is the
readable plan. The full binding design is
[openspec/changes/generate-cask-catalog-with-rust/design.md](../../openspec/changes/generate-cask-catalog-with-rust/design.md);
the contract is
[contract.md](../../openspec/changes/generate-cask-catalog-with-rust/contract.md);
per-task truth is
[tasks.md](../../openspec/changes/generate-cask-catalog-with-rust/tasks.md);
evidence is
[docs/verification/2026-09-28-rust-cask-catalog.md](../verification/2026-09-28-rust-cask-catalog.md).
An offline HTML mirror of this plan is
[artifacts/rust-cask-catalog-plan.html](../../artifacts/rust-cask-catalog-plan.html).

## Goal

Turn one pinned Homebrew Cask snapshot into native Nix packages on
two systems (`aarch64-darwin`, `x86_64-linux`) with no Brew, no Ruby,
no token allowlists, no hosted API, no scheduler, no auto-updater.
Breaking changes are allowed; there is no compatibility layer.

## Architecture flow

1. **Maintainer, network:** `tools/cask-catalog` (Rust, workspace
   member, never shipped) downloads `cask.json` at an exact revision
   and writes the pin `nix/casks/catalog/input.json`.
2. **Maintainer, offline:** the same tool reads the pin plus the
   cached snapshot and writes one committed file:
   `nix/casks/catalog/catalog.json` (schema `pkg-cask-catalog/2`).
   Deterministic: same pin plus same generator version, identical
   bytes.
3. **Review:** catalog and pin are committed together in one change.
4. **Nix (generic, data-driven):** `nix/casks/flake.nix` (only input:
   pinned nixpkgs) reads the catalog with `builtins.fromJSON` and
   exposes `packages.<system>.<token>` (lazy, one derivation per
   eligible token), `catalogIndex` (client search/status), and
   `catalogStatus` (counts). Building uses generic builders
   (`nix/casks/lib/builders.nix`) plus a stdlib-only Python build-time
   helper (`nix/casks/lib/plan.py`). The helper is build machinery
   only; no Python runs on the client at any time.
5. **Client (Rust `pkg`):** reads `catalogIndex` for search and
   status; installs through the ordinary native profile lifecycle.

Why Rust and Python together: generation is complex logic
(variation merge, eligibility, plan typing) that benefits from a
unit-testable offline binary with the workspace's lint and test
policy. The Nix build itself stays generic Nix plus the small
build-time Python stdlib helper so vendor strings never become shell
syntax. The client keeps zero conversion logic.

## Scope and deletions

In: macOS `.app` bundles (with CLI paths, completions, manpages),
`.pkg` payloads under a structural app-bundle rule, Linux
binaries (autoPatchelf with a bounded closure) and AppImages
(`appimageTools.wrapType2`).

Out (excluded at generation, recorded with a reason): formula or cask
dependencies (`formula-dependency`,
`cask-dependency-integration` — there is no dependency graph, no
closure expansion, no cycle handling), installer scripts
(`installer-script`), and more — the full bounded vocabulary is in the
design. Fonts, services, drivers, privileged installers stay out.

Deleted: the brew-nix flake input, curated token lists and overrides,
client `CASK_SYSTEM`/`casks_supported_on`, the `pkg-cask-support/1`
decode path and its fixture.

## Ownership table

| Piece | Location | Owner |
| --- | --- | --- |
| Generator | `tools/cask-catalog` | this change (implemented) |
| Builders + helper | `nix/casks/lib/{builders.nix,plan.py}` | this change (implemented and verified) |
| Generated data + pin | `nix/casks/catalog/` | maintainer-run regeneration |
| Flake outputs | `nix/casks/flake.nix` | Nix owner (implemented) |
| Client | `crates/pkg-cli` | client owners (implemented) |
| Release packaging | `tools/release/package_client.sh` | implemented and verified on both systems |
| Integration, final counts, final PR | — | parent |

## Catalog contract (summary)

Envelope: `schema`, `generator{name,version}`, `input{url,revision,sha256,license}`,
`targets`, `macosBaseline:"15.7.7"`, `entries`. Every entry carries
`token`, `name`, `description`, `version`, `homepage` (string or
null) and a `targets` map with one record per target system:

```json
{
  "status": "eligible", "kind": "app+cli", "reason": null,
  "detail": null, "version": "3.17.19", "homepage": "https://cursor.com",
  "plan": {
    "source": { "url": "https://down.../Cursor.dmg", "sha256": "..." },
    "archive": { "kind": "auto" },
    "artifacts": [
      { "kind": "app", "source": "Cursor.app", "target": "Cursor.app" },
      { "kind": "binary",
        "source": "$APPDIR/Cursor.app/Contents/Resources/app/bin/cursor",
        "target": "cursor" }
    ],
    "minMacos": null
  }
}
```

Artifacts are uniform `{kind, source, target}`; `target` is null only
for `pkg`. Excluded targets carry `kind: null`, a reason code, a
nullable detail, and `plan: null`. A token absent from `entries` is
unknown. Generator collects the actual target list; it never infers OS
support from variation keys or artifact kinds — only
`supported_platforms` membership and `depends_on` constraints count.

## Build path and the safe `.pkg` boundary

One generic derivation handles `auto` and `raw-binary` archives:
fetchurl fetches; the helper unpacks with pre-write member and symlink
validation; artifacts are placed; a post-install audit resolves every
output link. Vendor strings travel as JSON data, never shell syntax.
AppImages go through `appimageTools.wrapType2`. `.pkg` payloads are
proven structurally: no `Scripts`, no `Distribution` (choice-requiring
installers unsupported), no plugins, no nested pkgs, no system
locations; only app-bundle layouts place files (plain files are
rejected, naming the component). Nothing ever invokes
`/usr/sbin/installer` or any privileged installer.

## Client lifecycle

`pkg search <pattern>` reads `catalogIndex` (Rust regex, local
filter; the Nixpkgs lane passes patterns to `nix search` unchanged).
`pkg info <token>` and `pkg install <token>` resolve
`packages.<system>.<token>`; excluded tokens are refused with their
reason, unknown tokens as unknown. Installs keep the original moving
reference, so `pkg update`, `pkg upgrade`, `pkg rollback`,
`pkg remove`, and `pkg apps sync` are the ordinary native lifecycle.
Homebrew cleanup hooks never run.

## Catalog update procedure (maintainer)

```
cargo run -p cask-catalog -- fetch \
  --revision <40-hex> --sha256 <64-hex> \
  --pin nix/casks/catalog/input.json
cargo run -p cask-catalog -- generate \
  --pin nix/casks/catalog/input.json \
  --out-dir nix/casks/catalog
git diff nix/casks/catalog/   # review, then commit pin + catalog together
```

Actual flags: `fetch --revision --repo --sha256 --cache-dir --pin`;
`generate --pin --input --cache-dir --out-dir` (defaults shown). No
server, no scheduler.

## Status

Implementation and local verification are complete. See the dated
[verification record](../verification/2026-09-28-rust-cask-catalog.md)
for source pins, coverage, commands, and limits. Both native client
journeys passed. macOS bundle signatures, relocatable gzip and pbzx
package payloads, Linux dynamic linking, and both client archives were
checked. The GitHub CI result is recorded on the pull request.

The catalog contains 7,709 records. Metadata eligibility is 3,144 on
Apple silicon macOS and 160 on x86_64 Linux. This is not a claim that
every eligible package was built or used interactively.
