# Dynamic cask selection and Linux support

Checked on 28 September 2026. Research only. This report changes no
package support.

Answers three questions: why the cask list is hardcoded, whether `pkg`
can select casks dynamically, and what Homebrew on Linux would add.

## Correction to older notes

Homebrew Casks can target macOS, Linux, or both. Older notes that say
Casks are macOS-only are outdated.

Current Cask Cookbook rules, in own words:

- Artifact stanzas carry an OS scope. `app_image` is Linux-only, `app`
  and `pkg` are macOS-only, `binary` works on both.
- `depends_on :macos` or `depends_on :linux` marks a single-OS cask.
  Cross-platform casks put OS-specific stanzas into `on_macos` or
  `on_linux` blocks, so the same cask can serve both systems.
- `sha256` accepts per-target hashes (`arm:`, `intel:`, `arm64_linux:`,
  `x86_64_linux:`), and `arch:` / `os:` stanzas substitute URL parts.

Sources: [Cask Cookbook](https://docs.brew.sh/Cask-Cookbook),
[Acceptable Casks](https://docs.brew.sh/Acceptable-Casks).

## Current architecture trace

The chain today. These files are the authority; older design notes lag.

1. **Pinned metadata.** `nix/casks/flake.nix` reads
   `brew-api + "/cask.json"` with `builtins.fromJSON`. Plain JSON import;
   no IFD; no Ruby. All four inputs are pinned to exact commits in
   `nix/casks/flake.lock`: brew-nix `16131ae4`, brew-api `245947c0`,
   nixpkgs `567a49d1`, nix-darwin `a1fa429e`.
2. **Classifier.** `classify` in the same flake derives each decision
   from that pinned metadata: artifact kinds with `target` stripped;
   inert kinds are `app`, `binary`, `zap`, `uninstall`,
   `generate_completions_from_executable`; excludes `pkg`
   (`native-installer`), `installer` (`installer-script`), other artifact
   kinds (`unsupported-artifact`), absent or `no_check` checksums
   (`missing-checksum`), and missing URL (`missing-url`; the flake's
   system is `aarch64-darwin`, which uses the top-level cask URL).
3. **Selected token list.** `supportedTokens = [ "iterm2"
   "chromedriver" "cursor" ]` and `excludedTokens` (plus
   `inspectedExclusions` for on-device findings) are literal lists in the
   flake. The system itself is the literal `"aarch64-darwin"`. This is
   the hardcoding in question: a deliberate curated reviewed set, not a
   limit of the metadata.
4. **brew-nix derivation.** Each supported token maps to
   `brew-nix.packages.${system}.${token}`. The one override (`cursor`)
   appends to `installPhase`, because the converter defines
   `installPhase` as a plain string, so `postInstall` hooks never run.
5. **Support manifest.** `caskSupport.${system}` publishes schema
   `pkg-cask-support/1`: locked source revisions (read from `input.rev`),
   `supported` rows, `excluded` rows with metadata-computed reasons.
   Evaluation throws if a curated token stops classifying as supported,
   so a metadata bump cannot merge silently.
6. **CLI gate.** `crates/pkg-cli/src/catalog/cask.rs`:
   `CASK_SYSTEM = "aarch64-darwin"`, `casks_supported_on()`, and
   `classify_cask()` over the manifest (excluded wins; unknown tokens are
   `Unlisted`). `crates/pkg-cli/src/commands/lifecycle.rs` `gate_cask()`
   enforces platform and manifest status before install.
   `crates/pkg-cli/src/config.rs` defaults the source to
   `github:spa5k/pkg/main?dir=nix/casks`.
7. **Native profile.** `crates/pkg-cli/src/catalog/id.rs`
   `CatalogId::Cask(token)` resolves to `{casks source}#{token}` and one
   `nix profile add` into `~/.local/state/nix/profiles/pkg`.
8. **Launcher.** `crates/pkg-cli/src/apps.rs` builds launchers under
   `~/Applications/pkg/` on macOS only. Linux has no launcher path.

## Moving source, pinned metadata

`pkg install cask:X` resolves the moving outer reference
(`github:spa5k/pkg/main?dir=nix/casks`). Nix records the resolved
revision in the profile; `pkg upgrade` follows the original reference and
re-resolves. The inner `flake.lock` pins brew-nix, the metadata snapshot,
and nixpkgs. That pin makes evaluation and every support decision
reproducible and self-consistent. It does not freeze the user's source:
the outer reference keeps moving, and each resolution evaluates against
the metadata locked at that time.

## Metadata mechanics (verified examples)

Variations are partial overrides of the base record, not complete
records. A per-system decision must merge base and selected variation
first, then classify the effective record.

Verified against the exact pinned snapshot `245947c0` (25 August 2026,
raw file inspected: 7709 casks; every entry carries `supported_platforms`):

- `1password-cli` is cross-platform. Base `url`/`sha256` select the macOS
  arm64 zip. The `x86_64_linux` and `arm64_linux` variations each
  override `url` and `sha256` with their Linux zips. The artifact is one
  portable `binary` (`op` into `$HOMEBREW_PREFIX/bin/op`).
- `koreader` depends on Linux. Base `url` names the aarch64 AppImage and
  base artifacts install an `app_image`. `x86_64_linux` overrides
  `artifacts`, `url`, and `sha256`. `arm64_linux` overrides `sha256`
  only; its URL and artifacts are inherited from the base. This record
  only makes sense after the merge.

Rules this implies for any adapter:

1. Read support intent from the official constraint fields first:
   `supported_platforms` and the `depends_on` OS/arch entries. A missing
   `*_linux` variation alone proves nothing; the same payload can serve
   several systems through the base record.
2. Compute the effective record per system: base merged with the
   selected variation, including any `artifacts` override.
3. Then apply artifact, checksum, and dependency rules to the effective
   record. Reject records that stay ambiguous after the merge.
4. Never infer Linux support from the presence or count of `*_linux`
   variation keys.

## Snapshot sources compared

- Exact pinned input used today:
  <https://raw.githubusercontent.com/BatteredBunny/brew-api/245947c0b920cbe83f4003bb7c6be737352c8314/cask.json>
  (verified as described above).
- Current mirror HEAD when checked (28 September 2026):
  <https://raw.githubusercontent.com/BatteredBunny/brew-api/main/cask.json>
  has more entries and omits the `supported_platforms` field. Statements
  about HEAD describe HEAD, not our pin. Any lock update must re-check
  which fields the new snapshot carries.
- Official aggregate: <https://formulae.brew.sh/api/cask.json> — the
  schema-complete reference; per-cask files live under
  <https://formulae.brew.sh/api/cask/> (see the
  [JSON API docs](https://formulae.brew.sh/docs/api/)).

## Metadata source options

Four ways to feed the catalog. Whatever the source, two invariants hold:
the dataset must pass a schema check for the fields the classifier reads
(`token`, `url`, `sha256`, `variations`, `artifacts`, `depends_on`, and
`supported_platforms` when present), and the partial-variation merge
rules from "Metadata mechanics" apply unchanged.

| Option | Shape for the converter | Maintenance | Risk |
| --- | --- | --- | --- |
| (a) Pinned `brew-api` flake input (today) | Same `cask.json` the converter already imports; pin by commit in the input URL | Lowest: a lock update is edit-URL-and-relock; upstream owns generation | Upstream schema can drift — HEAD dropped `supported_platforms` when checked — so each bump must re-run the schema check |
| (b) Pinned official JSON snapshot | Official aggregate carries the same entry shape and the full field set. A bare file can be pinned by content hash or file fetch. The converter reads a directory holding `cask.json` (`importJSON (input + "/cask.json")`), so an adapter or wrapper is needed — an integration cost, not an intrinsic Nix limit | Medium: we own a small fetch/wrap step or a mirror repo | Duplicates what the mirror does; only worth it if the mirror keeps losing fields we need |
| (c) Vendored JSON snapshot in this repository | Our commit is the revision; converter path unchanged | Highest: we own refresh and review; measured snapshot sizes run from roughly 9 to 19 MB per revision, so diffs are large | Repository weight and noisy reviews; must retain upstream license notices |
| (d) Full Ruby repository checkout or vendor | Cask definitions are Ruby, not data. Running Ruby in the client would violate the runtime rule. Offline JSON generation in CI is possible, but it adds a toolchain, a parser, and a generation step that duplicate the official JSON | Rejected: extra toolchain, parser, and generation maintenance for data Homebrew already publishes | Largest payload; a poor fit even though the client itself stays Ruby-free |

Recommendation: keep option (a), the pinned metadata input, for least
maintenance. Re-check the schema on every lock bump; the existing eval
guard already throws when a curated token stops classifying. Consider
option (b) only if the mirror keeps dropping fields the classifier needs,
and option (c) only if we must own the snapshots ourselves, retaining
license notices. Reject option (d).

Optional future simplification (a proposal, not current behavior): make
the input URLs updateable and let `flake.lock` hold the exact revisions,
so ordinary lock updates can advance the metadata. Today the input URLs
pin exact commits by design, so a plain `nix flake update` cannot advance
them.

## brew-nix gap on current main

Read 28 September 2026 from
[`flake.nix`](https://github.com/BatteredBunny/brew-nix/blob/main/flake.nix)
and
[`casks.nix`](https://github.com/BatteredBunny/brew-nix/blob/main/casks.nix)
at `main`:

- Outputs are generated for `x86_64-darwin` and `aarch64-darwin` only.
  No Linux system.
- Every derivation gets `meta.platforms = lib.platforms.darwin`.
- `defaultVariationFor` selects a variation only on x86_64 macOS hosts,
  from an Intel-macOS preference list. It never selects `x86_64_linux`
  or `arm64_linux`.
- `binary` artifacts install into `$out/bin`; `app` artifacts extract
  the bundle and wrap the executable; a `pkg` path exists but prefers
  the `.app` from a dmg to keep signatures. No `app_image` handling
  appears.
- The token attrset is generated from one JSON import, so per-token
  derivations stay lazy: only requested tokens evaluate.

Conclusion: Linux Casks are possible with pinned data, but the converter
needs a bounded Linux adapter — variation merge, host-based selection,
`binary` and `app_image` install paths, per-OS artifact rules. brew-nix
has no formula importer; its repository contains cask code only. Do not
claim brew-nix supports Linux or formulae.

## Recommended order

Constraints that hold for every step: no Brew installation, no Ruby
evaluation in the client, lazy derivations, installs stay pinned to
locked metadata.

1. **Dynamic macOS catalog first.** Publish per-system eligibility from
   the pinned metadata at evaluation time, keeping the current converter
   unchanged. Three states: **tested** (today's curated tokens, verified
   on-device), **eligible** (a known record that passes full per-system
   classification), **excluded** (computed reason). Unknown tokens stay
   unknown; `Unlisted` never silently becomes eligible.
2. **Bounded Linux adapter second.** Merge base and `*_linux` variation,
   select by host, support `binary` and `app_image`, exclude macOS-only
   stanzas, and reuse the manifest gate. Contribute upstream if
   accepted; carry a local override otherwise.
3. **CI metadata refresh later, as a proposal only.** A scheduled change
   that advances the metadata revision, re-locks, and relies on the
   existing eval guard. Nothing automatic is created now.

### Full eligibility checks (all required, per system)

- Merge base and selected variation; reject records that stay ambiguous.
- OS/arch intent from `supported_platforms` and `depends_on`.
- Effective artifacts within the allowed kinds for that system.
- Effective checksum present and not `no_check`; effective URL present.
- Dependencies resolved: `depends_on` cask and formula entries resolve
  to eligible records, or the cask is excluded with that reason.
- OS version requirements satisfied or excluded with that reason.
- Scripts and hooks (`installer`, preflight, postflight, service
  stanzas) absent or inert, else excluded.
- Disabled or deprecated metadata rejected.
- Multiple artifacts: every artifact in the record must classify.

## Linux beyond casks

Nixpkgs stays the default Linux source; it already covers most CLI
software. The cask lane adds vendor-archive apps that Homebrew packages.

Formula bottles are a third lane, optional and the most complex. Facts
from [Bottles](https://docs.brew.sh/Bottles) and
[Homebrew on Linux](https://docs.brew.sh/Homebrew-on-Linux): bottles are
built for a Cellar location; relocation depends on explicit tags
(`:any`, `:any_skip_relocation`); compiled software usually embeds its
build location; `pour_bottle?` rules can require the default prefix.
Linux bottles exist — for example the `ripgrep` formula JSON lists
`arm64_linux` and `x86_64_linux` files at `ghcr.io/v2/homebrew/core` —
but pouring them into `/nix/store` needs prefix relocation, an ELF
loader and RPATH story, the formula dependency graph, and service and
postinstall semantics written for Homebrew's layout. A limited
conversion of static or dependency-free bottles is conceivable; the
Nixpkgs overlap makes it low value now. Recommendation: do not build a
bottle importer. Preserve the thin client.

## Vendoring note

Retain upstream license and attribution notices for any metadata
vendored into this repository.
