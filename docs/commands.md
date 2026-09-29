# Commands

`pkg` uses one native Nix profile per user. The profile lives at
`~/.local/state/nix/profiles/pkg`. Nix owns package state, generations,
rollback, and roots.

Global options: `--help`, `--version`, `--no-color`, `--verbose`.

| Command | Behavior |
| --- | --- |
| `pkg search QUERY` | Search supported sources. Shows source-qualified names and eligibility. |
| `pkg info ID` | Show source, attribute, metadata revision, and known limits. |
| `pkg install ID…` | Resolve all names, then run one native profile add. |
| `pkg list` | Read the dedicated profile. Shows native entry IDs and source identity. |
| `pkg remove ENTRY…` | Remove exact installed entries in one native operation. |
| `pkg update` | Refresh discovery data. Does not change installed packages. Does not refresh saved tap catalogs; run `pkg tap update` for that. |
| `pkg upgrade ENTRY…` / `--all` | Follow each entry's original reference. Reports fixed references. After `tap remove`, `--all` filters removed-source references and forwards the remaining entries by explicit native entry IDs (not native `--all`); upgrading a removed-source entry refuses. |
| `pkg history` | Show native profile generations. |
| `pkg rollback [N]` | Use Nix's native previous or selected generation. Refreshes launchers. |
| `pkg prune --older-than Nd` | Remove only eligible non-current generations. Requires an explicit positive age. |
| `pkg doctor` | Read-only runtime, profile, and launcher status. Repairs nothing. |
| `pkg shellenv` | Print idempotent Bash/Zsh path settings. Does not edit shell files. |
| `pkg completion SHELL` | Generate completions for the current grammar. |
| `pkg apps sync` | Rebuild pkg-owned macOS launchers from the active profile. |
| `pkg tap add SOURCE [--revision SHA] [--trust]` | Save a public tap. Ask approval before any fetch, import, or Ruby runs. |
| `pkg tap list [--json]` | List saved taps: source, origin, revision, and consent time. |
| `pkg tap update [SOURCE] [--revision SHA]` | Re-import saved tap catalogs at a pinned revision. Runs tap Ruby under the same rules as `add`. |
| `pkg tap remove SOURCE` | Block future use of a source. Installed packages stay. |

Removed commands: `pin`, `unpin`, `outdated`, `repair`, `gc`, and
`system uninstall`. Removed options: global dry-run, build approval,
`--keep-going`, and JSONL progress. Use normal CLI usage errors for them.

## Public cask taps

`pkg tap` manages saved public Homebrew Cask taps. A saved tap is converted
locally into ordinary Nix packages. Saved taps live in a per-user state
directory. Catalog format, publication, and sandbox prerequisites are in
[casks](casks.md). The accepted behavior is specified in the OpenSpec
change
[Import public cask taps locally](../openspec/changes/import-public-cask-taps/proposal.md).

- `SOURCE` is a public GitHub `owner/tap` repository. The canonical
  repository is `owner/homebrew-<tap>`; the common short source
  `owner/tap` maps to `owner/homebrew-tap`. Arbitrary URLs are not
  accepted in this version.
- The first `tap add` for an origin shows the canonical repository and the
  approval scope, then asks for consent. Consent is remembered per origin.
  No fetch, import, or Ruby runs before it. A noninteractive `add` without
  consent must pass `--trust` explicitly. Otherwise it stops and changes
  nothing.
- Only `tap add` and `tap update` execute tap Ruby. The Ruby runtime and
  the Homebrew reader are pinned and supplied by Nix. They run inside an
  ordinary sandboxed Nix derivation. `tap update` may execute source
  revision Ruby under the same restrictions.
- `pkg update` refreshes discovery data only. It does not refresh saved
  tap catalogs.
- Normal `search`, `info`, and `install` of saved catalogs run no source
  Ruby and never prompt.
- `tap remove` blocks future use of a source. It does not uninstall
  packages. After a removal, `pkg upgrade --all` skips removed-source
  references and upgrades the remaining entries by explicit native entry
  IDs; an explicit upgrade of a removed-source entry refuses.
- Updating tap metadata does not upgrade installed packages. Run
  `pkg tap update`, then `pkg upgrade ENTRY` or `--all`.
- Install, upgrade, rollback, and removal stay on the native Nix profile.
  There is no second package state engine.

Example with a real public tap:

```sh
pkg tap add goreleaser/tap
pkg info cask:goreleaser/tap/mcp
pkg install cask:goreleaser/tap/mcp   # only if info reports it eligible
```

This example shows the accepted flow. The final verification passed the
whole import on both hosts (Linux 13 eligible / 17 excluded; macOS
7 eligible / 23 excluded) and the negative product gates on both hosts
(C6/C7). The parent review is complete; only the implementation PR
remains open. See
the [plan](plans/public-cask-taps.md) and the
[verification status](verification/public-taps-2026-09-29/results.md).

## JSON query output

`--json` works only with the five query commands: `search`, `info`,
`list`, `tap list`, and `doctor`. With any other command it is a usage
error (exit code 2). Every JSON result uses the same versioned envelope,
pretty-printed:

```json
{
  "schema": 1,
  "command": "list",
  "result": []
}
```

`schema` is the envelope version; it is `1` today. Fields listed as
nullable are `null` when the value is unknown or does not apply.

### `search`

The result holds `system` (string), `sources` (one report per queried
source), and `results` (the matched rows). When every source fails and no
usable cache exists, `search` exits nonzero instead of reporting an empty
success. Rows reused from cache after a live failure carry
`"stale": true`.

Search pattern grammar, per lane:

- The `nixpkgs` lane passes the pattern to native `nix search` unchanged;
  native regex semantics apply.
- The `cask` lane evaluates the generated catalog index once and filters
  locally: the pattern is a Rust regex (the `regex` crate), compiled
  case-insensitively and matched against each entry's token, name, and
  description. An invalid pattern is reported as a source failure with the
  regex error. Only eligible entries are listed; excluded tokens stay
  discoverable through `pkg info`.

Source report fields:

| Field | Type | Meaning |
| --- | --- | --- |
| `source` | string | The moving reference queried. |
| `display` | string, nullable | Human identity from flake metadata. |
| `locked_reference` | string, nullable | The locked reference queried. |
| `revision` | string, nullable | Locked revision of that reference. |
| `status` | string | `fresh`, `stale`, `failed`, or `skipped-platform`. |
| `detail` | string, nullable | Failure or limitation detail. |
| `support_detail` | string, nullable | Why cask support badges are absent. |

Row fields:

| Field | Type | Meaning |
| --- | --- | --- |
| `id` | string | Source-qualified ID such as `nixpkgs:fd`; usable as an install ID. |
| `attribute` | string | The full native attribute behind the row. |
| `name`, `version`, `description` | string | Evaluated metadata. `description` can be empty. |
| `source` | string | `nixpkgs` or `cask`. |
| `reference` | string, nullable | The reference used for evaluation: normally the locked reference; the moving reference when source metadata failed. |
| `revision` | string, nullable | Locked revision of that reference. |
| `system` | string | System the row was evaluated for. |
| `stale` | boolean | Row reused from cache after a live query failure. |
| `support` | object, nullable | `{"status":"eligible"}` or `{"status":"excluded","reason":…,"detail":…}` for cask rows; `null` for nixpkgs rows. Eligible is the generated metadata status, not a verification promise. |

### `info`

| Field | Type | Meaning |
| --- | --- | --- |
| `id` | string | Qualified ID; an explicit GitHub installable shows as `flake:<attribute>`. |
| `source` | string | Display identity, or the reference when metadata was unreadable. |
| `reference` | string | The moving source reference used. |
| `locked_reference`, `revision` | string, nullable | The locked source Nix resolved. |
| `system` | string | System the query ran for. |
| `attribute` | string | The requested attribute or token. |
| `matched_attribute` | string, nullable | The actual full native attribute; `null` when only recorded cask metadata answered. |
| `name` | string | Package name or the requested attribute. |
| `version` | string, nullable | Evaluated or recorded version. |
| `description` | string | Evaluated description; can be empty. |
| `installed` | object, nullable | `{"entry_id":…,"original_url":…,"locked_url":…}` when an installed entry matches the full attribute and the canonical source; else `null`. |
| `cask` | object, nullable | Cask classification, below; `null` for non-cask IDs. |

`cask` is one of:

- `{"status":"eligible","kind":…,"homepage":…}` — the generated index
  marks the token eligible on this system. This is a metadata claim; the
  build proves the payload.
- `{"status":"excluded","reason":…,"detail":…}` — the recorded exclusion
  reason and detail.
- `{"status":"unknown"}` — the token is not in the generated catalog for
  this system.
- `{"status":"unsupported-platform","system":…,"targets":[…]}` — this
  system is outside the catalog's target list.

### `list`

The result is an array with one row per **active** profile entry:

| Field | Type | Meaning |
| --- | --- | --- |
| `entry_id` | string | The exact native entry ID. Pass it back verbatim to `remove` and `upgrade`. |
| `name` | string | Final attribute segment. |
| `source` | string | The original moving reference installed from. |
| `locked_source` | string | The locked reference Nix resolved. |
| `revision` | string, nullable | Locked revision of the locked source. |

### `tap list`

The envelope `command` is `tap-list`; the envelope is the same versioned
envelope as the other query commands. The result holds one row per saved
tap. Each row carries, at a high level:

- `source` — the saved `owner/tap` identity.
- `origin` — the canonical repository for the source.
- `revision`, `eligible`, `excluded` — the recorded revision and catalog
  counts, when a catalog is recorded.
- `added_unix` — the recorded consent time for the source, in Unix time.

### `doctor`

The result is an array of rows: `component` (string), `status` (string),
and `detail` (string, nullable). Statuses by component: `nix runtime`
`ok`/`missing`; `nix daemon` `ok`/`unreachable`; `pkg profile`
`ok`/`unreadable`/`fresh`; `app launchers`
`healthy`/`unhealthy`/`skipped`/`unknown`/`unreadable`; `config` `present`/`defaults`;
`discovery cache` `present`/`empty`. `doctor` exits nonzero when any
status is `missing`, `unreachable`, `unreadable`, or `unhealthy`. It
never repairs anything.

## Package IDs

- `nixpkgs:<attribute>` selects a Nixpkgs attribute.
- `cask:<token>` selects a Cask package by bare token. Tokens may use
  lowercase letters, digits, `+`, `.`, `_`, `-`, and `@`. A bare token
  resolves only when it is unique across the official catalog and every
  saved tap catalog. Excluded entries count as matches.
- `cask:owner/tap/token` selects a Cask package from one saved tap. It
  resolves only inside its named source.
- A bare name is valid only when it has one supported exact match. A cask
  bare name must equal one token exactly; a display name never resolves.
  An ambiguous token lists the qualified choices instead of picking a
  source by ordering.
- A saved catalog that is unreadable or malformed fails bare-token
  resolution and names the failed source. It is never silently ignored.
- An unknown `cask:owner/tap/token` source fails with an explicit
  `pkg tap add owner/tap` instruction. It is never converted silently and
  never reinterpreted as Nix code.
- Ambiguous names require a source-qualified ID.
- Explicit public GitHub flake installables with an attribute after `#` are
  accepted.

Default source: `github:NixOS/nixpkgs/nixpkgs-unstable`. Configuration at
`~/.config/pkg/config.toml` can replace the default reference.

## Fixed references

There is no pin state. A fixed package does not advance during normal
upgrade. To change its reference, remove the entry and install the new
reference. Retained generations provide rollback.

## Conflicts

`pkg` reports Nix's conflict and the conflicting entries. It does not
resolve file collisions silently. Remove the conflicting entry first.

## macOS apps

Supported packages expose their app bundles as launchers in
`~/Applications/pkg/`. Launchers refresh after install, remove, upgrade, and
rollback. If a launcher sync fails, the command reports partial completion
and names `pkg apps sync` as the retry. `pkg apps sync` rebuilds launchers
only. It does not change package selection. For public-tap vendor `.app`
bundles, sync requires vendor signatures to assess cleanly, and it
requires Gatekeeper assessments to be enabled (`spctl --status` reports
`assessments enabled`) before codesign/spctl assessment; a rejected app
never changes existing launchers. This gate does not cover CLI binaries
or ordinary Nixpkgs apps, and it does not claim GUI launch or
application runtime isolation.
