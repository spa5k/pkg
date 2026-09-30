# Shared catalog projection helper (schema pkg-cask-catalog/4).
#
# Used by BOTH flake wrappers:
#   - the official `nix/casks/flake.nix` (two targets, official snapshot)
#   - the generated raw-tap flake template
#     (`tools/cask-catalog/src/tap/flake-template.nix`)
#
# Interface: { nixpkgs, catalog } -> { packages, catalogIndex,
# catalogStatus, gate }. The wrappers stay small: they read the
# committed/generated `catalog/catalog.json` and delegate every
# projection here. No app-specific routing; target lists come from
# `catalog.targets` so official two-target and public native-only
# catalogs are handled by the same code.
#
# Entry keys are the full identity `source/token` (official:
# `homebrew/cask/token`; raw taps: `owner/tap/token`).
# `packages` attribute names are the WHOLE id encoded in one pass:
# `_` -> `_u_` and `.` -> `_d_` (same rule as the root
# `cask_catalog::package_attribute` helper), so unquoted `nix build`
# never splits a path. `catalogIndex` keys stay the HUMAN ids and each
# entry repeats its `source` and bare `token`. Store names carry a
# `pnameSuffix` derived from the source identity hash so entries from
# different sources never collide.

{ nixpkgs, catalog }:

let
  inherit (nixpkgs) lib;

  # Whole-catalog integrity gate: schema must match, and invalid
  # identity segments (not usable as map keys or store names) fail
  # evaluation.
  schemaOk = catalog.schema or "" == "pkg-cask-catalog/4";

  segmentOk =
    seg:
    # one store-name-safe component; '/' is outside the charset,
    # so no dot-dot path is possible
    lib.strings.match "[a-z0-9][a-z0-9+._@-]*" seg != null;

  # Entry keys are exactly `source/token` with a `owner/tap` source.
  idOk = id: lib.length (lib.splitString "/" id) == 3 && lib.all segmentOk (lib.splitString "/" id);

  validIds = lib.all idOk (lib.attrNames catalog.entries);

  gate = x: if schemaOk && validIds then x else throw "catalog: schema or token integrity failure";

  systems = catalog.targets;

  # Encoded packages attributes: whole-id single-pass
  # `_` -> `_u_`, `.` -> `_d_` (see `cask_catalog::package_attribute`).
  forSystem =
    system:
    let
      pkgs = import nixpkgs {
        inherit system;
        # Vendor archives are proprietary; pass them explicitly instead
        # of requiring --impure NIXPKGS_ALLOW_UNFREE from every client.
        config.allowUnfree = true;
      };
      builders = import ./builders.nix { inherit pkgs; };
    in
    lib.mapAttrs'
      (
        id: entry:
        let
          t = entry.targets.${system};
          # Store names stay unique per source: bare token plus a short
          # hash of the source identity (never the raw '/'-bearing id).
          pnameSuffix = "-" + lib.substring 0 12 (builtins.hashString "sha256" entry.source);
        in
        {
          name = builtins.replaceStrings [ "_" "." ] [ "_u_" "_d_" ] id;
          value = builders.buildEntry {
            inherit pnameSuffix;
            inherit (entry) token;
            inherit system;
            inherit (t) plan;
            version = t.version or entry.version;
            inherit (entry) description;
            homepage = t.homepage or entry.homepage;
            baseline = catalog.macosBaseline;
          };
        }
      )
      (
        lib.filterAttrs (
          id: entry:
          idOk id
          && entry ? targets.${system}
          && entry.targets.${system}.status == "eligible"
          && entry.targets.${system}.plan != null
        ) catalog.entries
      );

  # Client index envelope: same provenance, entries replaced by a
  # per-system entry map; effective version/homepage per target; no
  # plans exposed to the client.
  catalogIndex = (removeAttrs catalog [ "entries" ]) // {
    systems = lib.genAttrs systems (system: {
      entries = lib.mapAttrs (
        id: entry:
        let
          t = entry.targets.${system};
        in
        {
          # Full identity plus the source and bare token, so clients
          # never re-derive them from the key.
          inherit id;
          inherit (entry)
            source
            token
            name
            description
            ;
          version = t.version or entry.version;
          homepage = t.homepage or entry.homepage;
          inherit (t)
            status
            kind
            reason
            detail
            ;
          # Declared macOS runtime range of the plan, for the cheap
          # client gate: null on Linux records and when undeclared.
          minMacos = if t ? plan then t.plan.minMacos or null else null;
          maxMacos = if t ? plan then t.plan.maxMacos or null else null;
        }
      ) (lib.filterAttrs (_: entry: entry ? targets.${system}) catalog.entries);
    });
  };

  perSystemStatus =
    system:
    let
      entries = lib.filterAttrs (_: entry: entry ? targets.${system}) catalog.entries;
      statuses = lib.mapAttrsToList (_: entry: entry.targets.${system}.status) entries;
      eligible = lib.length (lib.filter (s: s == "eligible") statuses);
      total = lib.length statuses;
      reasons = lib.sort (a: b: a < b) (
        lib.unique (
          lib.filter (r: r != null) (
            lib.mapAttrsToList (_: entry: entry.targets.${system}.reason or null) entries
          )
        )
      );
    in
    {
      inherit total eligible;
      excluded = total - eligible;
      byReason = lib.genAttrs reasons (
        r:
        lib.length (
          lib.filter (e: e.targets.${system}.status == "excluded" && e.targets.${system}.reason == r) (
            lib.attrValues entries
          )
        )
      );
    };

  catalogStatusRaw = {
    schema = "pkg-cask-catalog-status/1";
    inherit (catalog)
      generator
      inputs
      targets
      macosBaseline
      ;
    systems = lib.genAttrs systems perSystemStatus;
  };
in
{
  inherit gate;
  packages = gate (lib.genAttrs systems forSystem);
  catalogIndex = gate catalogIndex;
  catalogStatus = gate catalogStatusRaw;
}
