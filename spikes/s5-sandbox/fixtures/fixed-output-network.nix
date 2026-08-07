{ system }:
let
  nixpkgs = builtins.getFlake "github:NixOS/nixpkgs/a62e6edd6d5e1fa0329b8653c801147986f8d446?narHash=sha256-oamiKNfr2MS6yH64rUn99mIZjc45nGJlj9eGth%2F3Xuw%3D";
  pkgs = nixpkgs.legacyPackages.${system};
in
pkgs.runCommand "s5-fixed-output-network" {
  nativeBuildInputs = [ pkgs.cacert pkgs.curl ];
  outputHashMode = "flat";
  outputHashAlgo = "sha256";
  outputHash = "sha256-LJ3jc651pScWN2NQNERaXNOmrjWsbDBtQMDgZ2R4WJc=";
} ''
  SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt \
    curl --fail --silent --show-error --max-time 30 https://cache.nixos.org/nix-cache-info > "$out"
''
