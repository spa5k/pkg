{ system }:
let
  nixpkgs = builtins.getFlake "github:NixOS/nixpkgs/a62e6edd6d5e1fa0329b8653c801147986f8d446?narHash=sha256-oamiKNfr2MS6yH64rUn99mIZjc45nGJlj9eGth%2F3Xuw%3D";
  pkgs = nixpkgs.legacyPackages.${system};
in
pkgs.runCommand "s5-cgroup-boundary" { } ''
  sleep 5
  printf '%s' 'cgroup-probe-complete' > "$out"
''
