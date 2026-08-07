{ system }:
let
  nixpkgs = builtins.getFlake "github:NixOS/nixpkgs/a62e6edd6d5e1fa0329b8653c801147986f8d446?narHash=sha256-oamiKNfr2MS6yH64rUn99mIZjc45nGJlj9eGth%2F3Xuw%3D";
  pkgs = nixpkgs.legacyPackages.${system};
in
pkgs.runCommand "s5-regular-network-denied" { nativeBuildInputs = [ pkgs.curl ]; } ''
  if curl --fail --silent --show-error --max-time 10 https://cache.nixos.org/nix-cache-info >/dev/null 2>&1; then
    printf '%s' 'network-unexpectedly-available' > "$out"
    exit 1
  fi
  printf '%s' 'network-denied' > "$out"
''
