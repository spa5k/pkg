{
  description = "pkg Homebrew Formulae as native Nix packages, without Brew";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/567a49d1913ce81ac6e9582e3553dd90a955875f";
  outputs = { nixpkgs, ... }:
    import ./lib/catalog.nix {
      inherit nixpkgs;
      catalog = builtins.fromJSON (builtins.readFile ./catalog/catalog.json);
    };
}
