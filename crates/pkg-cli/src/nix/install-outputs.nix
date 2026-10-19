{
  reference,
  system,
  requests,
}:
let
  flake = builtins.getFlake reference;
  lookup =
    path:
    builtins.foldl'
      (
        found: name:
        if found.exists && builtins.isAttrs found.value && builtins.hasAttr name found.value then
          {
            exists = true;
            value = builtins.getAttr name found.value;
          }
        else
          {
            exists = false;
            value = null;
          }
      )
      {
        exists = true;
        value = flake;
      }
      path;
  inspect =
    path:
    let
      paths = [
        ([ "packages" system ] ++ path)
        ([ "legacyPackages" system ] ++ path)
        path
      ];
      existing = builtins.filter (row: row.result.exists) (
        map (path: { inherit path; result = lookup path; }) paths
      );
      single = builtins.length existing == 1;
      selected = builtins.head existing;
      p = selected.result.value;
      parsed = builtins.parseDrvName p.name;
      outputs = p.meta.outputsToInstall or [ (p.outputName or "out") ];
    in
    if !single || !builtins.isAttrs p || (p.type or null) != "derivation" then
      null
    else
      {
        attribute = builtins.concatStringsSep "." selected.path;
        paths = map (name: p.${name}.outPath) outputs;
        meta = {
          pname = parsed.name;
          version = parsed.version;
          description = builtins.replaceStrings [ "\n" ] [ " " ] (p.meta.description or "");
        };
      };
in
map inspect requests
