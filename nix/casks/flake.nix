{
  description = "pkg Cask source: generated Homebrew Cask catalog as native Nix packages, without Brew";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/8edc0c72e3a38faf5434e40d1f19431125fe4b30";

  outputs =
    { nixpkgs, ... }:
    let
      # Committed generated catalog (schema pkg-cask-catalog/4), plain
      # JSON: no IFD, no compiler, no per-token Nix files. All
      # package/index/status projection lives in the shared
      # ./lib/catalog.nix helper (also used by generated tap flakes).
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
