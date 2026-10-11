{ nixpkgs, catalog }:
let
  inherit (nixpkgs) lib;
  valid = catalog.schema == "pkg-formula-catalog/1"
    && catalog.nixpkgsRevision == nixpkgs.rev
    && lib.all (name: builtins.match "[a-z0-9][a-z0-9+._@-]*" name != null)
      (builtins.attrNames catalog.entries);
  forSystem = system:
    let
      pkgs = import nixpkgs { inherit system; config.allowUnfree = true; };
      bottle = import ./bottle.nix { inherit pkgs; };
    in lib.mapAttrs' (name: entry:
      let target = entry.targets.${system}; in {
        name = builtins.replaceStrings [ "_" "." ] [ "_u_" "_d_" ] "homebrew/core/${name}";
        value = if target.kind == "nixpkgs-equivalent" then
          let package = lib.attrByPath target.attribute null pkgs; in
          assert package != null && lib.isDerivation package;
          assert package.version == target.version;
          package
        else bottle { inherit name entry target system; };
      }) (lib.filterAttrs (_: entry: entry.targets.${system}.status == "eligible") catalog.entries);
  index = {
    inherit (catalog) schema generator inputs targets macosBaseline aliases executables deprecated;
    systems = lib.genAttrs catalog.targets (system: {
      entries = lib.mapAttrs' (name: entry: {
        name = "homebrew/core/${name}";
        value = {
          source = "homebrew/core";
          token = name;
          inherit (entry) name description homepage;
          inherit (entry.targets.${system}) status version kind reason detail minMacos;
          maxMacos = null;
        };
      }) catalog.entries;
    });
  };
in assert valid; {
  packages = lib.genAttrs catalog.targets forSystem;
  formulaIndex = index;
}
