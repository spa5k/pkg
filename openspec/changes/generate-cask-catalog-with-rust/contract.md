# Final manager contract (binding for this change)

This contract overrides earlier drafts. It records what the change
delivers. All items below are implemented as stated.

1. **Dependencies.** There is no dependency graph and no profile
   expansion. A record with any Homebrew formula dependency is excluded
   as `formula-dependency`. A record with any cask dependency is excluded
   as `cask-dependency-integration`. The native Nix lifecycle stays
   single-package. There is no `depends` output and no client closure
   expansion.

2. **macOS baseline.** The declared baseline is exactly `15.7.7`.
   Version comparisons are numeric on major, minor, and patch, with
   missing components treated as zero.

3. **Plans.** Artifact plans uniformly use `{kind, source, target}`.
   Kinds: `app`, `binary`, `pkg`, `appimage`, `manpage`,
   `bash-completion`, `zsh-completion`, `fish-completion`. A `pkg`
   artifact has a null target. An app source is a bundle-relative path
   and its target is the renamed bundle ending `.app`. A binary source
   may start with `$APPDIR`; its target comes from the metadata rename
   or the source basename. An AppImage source is the metadata name; its
   target is the metadata rename or source basename without `.AppImage`.
   It must be one path component with no ASCII control characters. Spaces
   and Unicode are preserved. No wrapper is
   guessed; only explicit binary artifacts exist.

4. **Plan shape.** A plan is
   `{source:{url,sha256}, archive:{kind}, artifacts:[…], minMacos}` with
   `kind` one of `auto`, `raw-binary`, `appimage`. `auto` sniffs archive
   content, so extensionless URLs work. Only an explicit naked container
   selects `raw-binary`; unsupported container shapes are excluded as
   `unsupported-container`. A `.pkg` is an artifact inside an `auto`
   container or a raw xar.

5. **Catalog file.** One generated JSON file, schema
   `pkg-cask-catalog/2`, with `generator{name:"cask-catalog",version}`,
   `input{url,revision,sha256,license}`, `targets`
   (`aarch64-darwin`, `x86_64-linux`), `macosBaseline:"15.7.7"`, and
   `entries` keyed by token with per-target `status`, `kind`, `reason`,
   `detail`, effective `version`/`homepage`, and `plan`. Entry-level
   `name`, `description`, `version`, and `homepage` are string or null.
   Eligible entries carry a kind string, null reason and detail, and a
   plan; excluded entries carry a null kind, a reason code, a nullable
   detail, and a null plan. Every target is present for every valid
   token. Invalid token identifiers are whole-catalog integrity errors.

6. **Client index.** The flake exposes `catalogIndex`: the same
   provenance fields, but `systems.<system>.entries.<token>` status
   records without plans and without a `depends` field. The client
   checks `targets` first. The flake also exposes `catalogStatus` with
   provenance and per-system counts (total, eligible, excluded,
   by reason).

7. **Nix source.** The only flake input is the existing pinned
   `nixpkgs` revision `567a49d1…`. Vendor archives are proprietary, so
   the flake imports Nixpkgs with `config.allowUnfree = true`; clients
   need no `--impure` flag.

8. **Input discipline.** `supported_platforms` is strictly required; OS
   support is never guessed from variations or artifacts. The exact
   pinned input revision is
   `245947c0b920cbe83f4003bb7c6be737352c8314`.

9. **No production self-test command.** Focused `cargo test` suites are
   the checks.

10. **Evidence, honestly stated.** Linux probes passed headless
    (`op` 2.39.0, KOReader `v2026.07.1 --help`); GUI was not tested.
    The macOS VM (15.7.7) built and launched Cursor 3.17.19 with a valid
    deep codesign and a working `cursor --version`. Full catalog output
    states metadata eligibility only, never verification. There are no
    per-token flags or allowlists. A raw `.pkg` plan must still pass the
    strict payload script and layout checks at build time.

11. **Ownership.** Integration, verification, and the final PR belong to
    the parent. No user confirmation is required for this change.
