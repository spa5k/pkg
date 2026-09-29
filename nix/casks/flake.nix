{
  description = "pkg Cask source: generated Homebrew Cask catalog as native Nix packages, without Brew";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/567a49d1913ce81ac6e9582e3553dd90a955875f";

  outputs =
    { nixpkgs, ... }:
    let
      # Committed generated catalog (schema pkg-cask-catalog/3), plain
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
