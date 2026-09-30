# Generated flake for one imported public Homebrew tap (schema
# pkg-cask-catalog/4, single native target). SELF-CONTAINED by contract:
# the catalog and every builder asset live inside this directory; no
# path outside it is ever imported. Regenerating this output is the
# importer's job (`cask_catalog::tap`), not the client's.
#
# Entry attribute IDs are the full catalog identity `owner/tap/token`
# encoded as ONE Nix attribute segment: `_` -> `_u_` and `.` -> `_d_`
# (same rule as the root `cask_catalog::package_attribute` helper;
# '/' is already a safe attribute character), so `nix profile`
# operations never split a dotted path:
#   nix build .#"example/tap/my-cask"
# catalogIndex keeps the HUMAN ids unchanged.
#
# All package/index/status projection lives in the shared
# ./lib/catalog.nix helper (copied here by the importer, same file as
# the official `nix/casks/flake.nix` uses).
{
  description = "@DESCRIPTION@";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/567a49d1913ce81ac6e9582e3553dd90a955875f";

  outputs =
    { nixpkgs, ... }:
    let
      catalog = builtins.fromJSON (builtins.readFile ./catalog/catalog.json);
      helper = import ./lib/catalog.nix { inherit nixpkgs catalog; };
    in
    {
      inherit (helper)
        packages
        catalogIndex
        catalogStatus
        ;
    };
}
