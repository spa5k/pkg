# Commands

`pkg` uses one native Nix profile per user. The profile lives at
`~/.local/state/nix/profiles/pkg`. Nix owns package state, generations,
rollback, and roots.

Global options: `--help`, `--version`, `--no-color`, `--verbose`.

| Command | Behavior |
| --- | --- |
| `pkg search QUERY` | Search supported sources. Shows source-qualified names and support status. |
| `pkg info ID` | Show source, attribute, metadata revision, and known limits. |
| `pkg install ID…` | Resolve all names, then run one native profile add. |
| `pkg list` | Read the dedicated profile. Shows native entry IDs and source identity. |
| `pkg remove ENTRY…` | Remove exact installed entries in one native operation. |
| `pkg update` | Refresh discovery data. Does not change installed packages. |
| `pkg upgrade ENTRY…` / `--all` | Follow each entry's original reference. Reports fixed references. |
| `pkg history` | Show native profile generations. |
| `pkg rollback [N]` | Use Nix's native previous or selected generation. Refreshes launchers. |
| `pkg prune --older-than Nd` | Remove only eligible non-current generations. Requires an explicit positive age. |
| `pkg doctor` | Read-only runtime, profile, and launcher status. Repairs nothing. |
| `pkg shellenv` | Print idempotent Bash/Zsh path settings. Does not edit shell files. |
| `pkg completion SHELL` | Generate completions for the current grammar. |
| `pkg apps sync` | Rebuild pkg-owned macOS launchers from the active profile. |

Removed commands: `pin`, `unpin`, `outdated`, `repair`, `gc`, and
`system uninstall`. Removed options: global dry-run, build approval,
`--keep-going`, and JSONL progress. Use normal CLI usage errors for them.

## JSON query output

`--json` works only with the four query commands: `search`, `info`,
`list`, and `doctor`. With any other command it is a usage error
(exit code 2). Every JSON result uses the same versioned envelope,
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
| `support` | object, nullable | `{"status":"supported"}` or `{"status":"excluded","reason":…,"detail":…}` for cask rows; `null` for nixpkgs rows, unlisted tokens, and when the support manifest is unavailable. |

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

- `{"status":"supported","kind":…,"version":…,"homepage":…,"artifactKinds":[…],"override":…,"sourceRevision":…}`
- `{"status":"excluded","version":…,"reason":…,"detail":…,"sourceRevision":…}`
- `{"status":"unlisted"}` — the token is not in the support manifest for this system.
- `{"status":"unsupported-platform","system":…,"required":"aarch64-darwin"}`
- `{"status":"unavailable","detail":…}` — the support manifest could not be evaluated.

### `list`

The result is an array with one row per **active** profile entry:

| Field | Type | Meaning |
| --- | --- | --- |
| `entry_id` | string | The exact native entry ID. Pass it back verbatim to `remove` and `upgrade`. |
| `name` | string | Final attribute segment. |
| `source` | string | The original moving reference installed from. |
| `locked_source` | string | The locked reference Nix resolved. |
| `revision` | string, nullable | Locked revision of the locked source. |

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
- `cask:<token>` selects a supported Cask package.
- A bare name is valid only when it has one supported exact match.
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
only. It does not change package selection.
