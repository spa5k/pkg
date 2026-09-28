{
  description = "pkg Cask source: generated Homebrew Cask catalog as native Nix packages, without Brew";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/567a49d1913ce81ac6e9582e3553dd90a955875f";

  outputs =
    { nixpkgs, ... }:
    let
      inherit (nixpkgs) lib;

      # Committed generated catalog (schema pkg-cask-catalog/2), plain JSON:
      # no IFD, no compiler, no per-token Nix files.
      catalog = builtins.fromJSON (builtins.readFile ./catalog/catalog.json);

      # Whole-catalog integrity gate: schema must match, and invalid token
      # identifiers (not usable as map keys or store names) fail evaluation.
      schemaOk = catalog.schema or "" == "pkg-cask-catalog/2";

      systems = catalog.targets;

      forSystem =
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            # Vendor archives are proprietary; pass them explicitly instead
            # of requiring --impure NIXPKGS_ALLOW_UNFREE from every client.
            config.allowUnfree = true;
          };
          builders = import ./lib/builders.nix { inherit pkgs; };
        in
        lib.mapAttrs
          (
            token: entry:
            let
              t = entry.targets.${system};
            in
            builders.buildEntry {
              inherit token system;
              inherit (t) plan;
              version = t.version or entry.version;
              inherit (entry) description;
              homepage = t.homepage or entry.homepage;
              baseline = catalog.macosBaseline;
            }
          )
          (
            lib.filterAttrs (
              token: entry:
              tokenOk token
              && entry ? targets.${system}
              && entry.targets.${system}.status == "eligible"
              && entry.targets.${system}.plan != null
            ) catalog.entries
          );

      tokenOk =
        token:
        # single store-name-safe component; '/' is outside the charset,
        # so no dot-dot path is possible
        lib.strings.match "[a-z0-9][a-z0-9+._@-]*" token != null;

      # Whole-catalog integrity gate: invalid token identifiers are not
      # usable as map keys or store names.
      validTokens = lib.all tokenOk (lib.attrNames catalog.entries);

      # Client index envelope: same provenance, entries replaced by a
      # per-system entry map; effective version/homepage per target; no
      # plans exposed to the client.
      catalogIndex = (removeAttrs catalog [ "entries" ]) // {
        systems = lib.genAttrs systems (system: {
          entries = lib.mapAttrs (
            token: entry:
            let
              t = entry.targets.${system};
            in
            {
              inherit token;
              inherit (entry) name description;
              version = t.version or entry.version;
              homepage = t.homepage or entry.homepage;
              inherit (t)
                status
                kind
                reason
                detail
                ;
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
          input
          targets
          macosBaseline
          ;
        systems = lib.genAttrs systems perSystemStatus;
      };
    in
    {
      packages =
        if schemaOk && validTokens then
          lib.genAttrs systems forSystem
        else
          throw "catalog: schema or token integrity failure";
      catalogIndex =
        if schemaOk && validTokens then
          catalogIndex
        else
          throw "catalog: schema or token integrity failure";
      catalogStatus =
        if schemaOk && validTokens then
          catalogStatusRaw
        else
          throw "catalog: schema or token integrity failure";
    };
}
