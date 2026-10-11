{ pkgs }:
{ name, entry, target, system }:
let
  fetch = import ./public-fetch.nix { inherit pkgs; };
  source = fetch { inherit (target) url sha256; };
  plan = pkgs.writeText "formula-bottle-plan.json" (builtins.toJSON {
    inherit name;
    inherit (target) cellarVersion;
  });
in
assert target.kind == "formula-bottle";
assert builtins.match "https://ghcr.io/v2/homebrew/core/[a-z0-9+./_-]+/blobs/sha256:[0-9a-f]{64}" target.url != null;
assert builtins.match "[0-9a-f]{64}" target.sha256 != null;
pkgs.stdenvNoCC.mkDerivation {
  pname = "formula-${name}";
  inherit (target) version;
  src = source;
  dontUnpack = true;
  dontConfigure = true;
  dontBuild = true;
  dontStrip = true;
  nativeBuildInputs = [ pkgs.python3 ] ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.autoPatchelfHook ];
  buildInputs = pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.stdenv.cc.cc.lib ];
  installPhase = ''
    python3 ${./bottle.py} "$src" ${plan} "$out"
  '';
  meta = {
    inherit (entry) description homepage;
    platforms = [ system ];
    sourceProvenance = [ pkgs.lib.sourceTypes.binaryNativeCode ];
  };
}
