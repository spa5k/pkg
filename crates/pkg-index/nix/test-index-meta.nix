# Run: nix eval --json --file crates/pkg-index/nix/test-index-meta.nix
let
  package = {
    type = "derivation";
    pname = "example";
    version = "1";
    outputs = [ "out" ];
    meta = { description = "line one\nline two\ttext"; };
  };
  pkgs = {
    stdenv.hostPlatform.system = "aarch64-darwin";
    # Prove that availability uses the upstream helper, including exclusions.
    lib.meta.availableOn = host: drv:
      !(builtins.elem host.system (drv.meta.badPlatforms or []));
    top = package;
    broken = package // { meta = { broken = true; }; };
    excluded = package // { meta = { badPlatforms = [ "aarch64-darwin" ]; }; };
    failure = throw "unavailable package";
    hidden = { child = package; };
    group = {
      recurseForDerivations = true;
      child = package;
      failure = throw "unavailable child";
      nested = { recurseForDerivations = true; child = package; };
    };
  };
  records = import ./index-meta.nix pkgs;
  find = path: builtins.head (builtins.filter (item: item.attrPath == path) records);
in
assert builtins.map (item: item.attrPath) records == [
  "broken" "excluded" "failure" "group.child" "group.failure" "group.nested.child" "top"
];
assert (find "failure").skipped && (find "group.failure").skipped;
assert !(find "broken").availableHere && !(find "excluded").availableHere;
assert (find "top").availableHere;
assert (find "top").description == "line one line two text";
records
