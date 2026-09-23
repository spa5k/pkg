# Production metadata projection for PR-14. This function is applied to one
# pinned `legacyPackages.<system>` value. It never builds or returns store paths.
# Ordinary per-attribute evaluation failures are converted to `skipped` records;
# `builtins.abort` remains intentionally uncatchable by `tryEval`.
pkgs:

let
  cap = value:
    if builtins.isString value
    then builtins.substring 0 4096
      (builtins.replaceStrings [ "\n" "\r" "\t" ] [ " " " " " " ] value)
    else null;
  sortedStrings = values:
    if builtins.isList values then
      builtins.sort (a: b: a < b)
        (builtins.map cap
          (builtins.filter builtins.isString values))
    else [];
  supportedSystems = [
    "aarch64-darwin"
    "aarch64-linux"
    "x86_64-darwin"
    "x86_64-linux"
  ];
  outputNames = raw:
    if builtins.isList raw then sortedStrings raw
    else if builtins.isAttrs raw then sortedStrings (builtins.attrNames raw)
    else [];
  licenseName = license:
    if builtins.isString license then cap license
    else if builtins.isAttrs license then
      cap (license.spdxId or license.shortName or license.fullName or null)
    else null;
  licenseNames = raw:
    let
      values = if builtins.isList raw then raw else [ raw ];
    in sortedStrings (builtins.map licenseName values);
  force = value: builtins.deepSeq value value;
  # Match nix search: descend only into package sets which opt into discovery.
  walk = prefix: set: builtins.concatLists (builtins.map
  (name:
    let
      path = if prefix == "" then name else "${prefix}.${name}";
      evaluated = builtins.tryEval (force (
        let
          drv = set.${name};
          meta = drv.meta or {};
          broken = (meta.broken or false) == true;
          sourcePlatforms = sortedStrings (meta.platforms or []);
          platforms = builtins.filter
            (system: builtins.elem system supportedSystems)
            sourcePlatforms;
        in if !builtins.isAttrs drv then []
        else if (drv.type or null) != "derivation" then
          if (drv.recurseForDerivations or false) == true
          then walk path drv else []
        else [{
          attrPath = path;
          pname = cap (drv.pname or null);
          version = cap (drv.version or null);
          description = cap (meta.description or null);
          homepage = cap (meta.homepage or null);
          licenses = licenseNames (meta.license or []);
          inherit platforms broken;
          availableHere = !broken && pkgs.lib.meta.availableOn pkgs.stdenv.hostPlatform drv;
          position = null;
          outputs = outputNames (drv.outputs or []);
          aliases = [];
          skipped = false;
        }]));
    in
      if evaluated.success then evaluated.value
      else [{ attrPath = path; skipped = true; }])
  (builtins.attrNames set));
in walk "" pkgs
