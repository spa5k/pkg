{
  description = "pkg Homebrew Formulae as native Nix packages, without Brew";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/8edc0c72e3a38faf5434e40d1f19431125fe4b30";
  outputs = { nixpkgs, ... }:
    import ./lib/catalog.nix {
      inherit nixpkgs;
      catalog = builtins.fromJSON (builtins.readFile ./catalog/catalog.json);
    };
}
