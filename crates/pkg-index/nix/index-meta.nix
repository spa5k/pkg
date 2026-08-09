# Production metadata projection for PR-14. This function is applied to one
# pinned `legacyPackages.<system>` value. It never builds or returns store paths.
# Ordinary per-attribute evaluation failures are converted to `skipped` records;
# `builtins.abort` remains intentionally uncatchable by `tryEval`.
pkgs:

let
  names = builtins.attrNames pkgs;
  cap = value:
    if builtins.isString value
    then builtins.substring 0 4096 value
    else null;
  sortedStrings = values:
    if builtins.isList values then
      builtins.sort (a: b: a < b)
        (builtins.map cap
          (builtins.filter builtins.isString values))
    else [];
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
  hostSystem = pkgs.stdenv.hostPlatform.system or null;
in
builtins.map
  (name:
    let
      evaluated = builtins.tryEval (force (
        let
          drv = pkgs.${name};
          meta = drv.meta or {};
          broken = (meta.broken or false) == true;
          platforms = sortedStrings (meta.platforms or []);
        in if (drv.type or null) != "derivation" then
          throw "not a derivation"
        else {
          attrPath = name;
          pname = cap (drv.pname or null);
          version = cap (drv.version or null);
          description = cap (meta.description or null);
          homepage = cap (meta.homepage or null);
          licenses = licenseNames (meta.license or []);
          inherit platforms broken;
          availableHere = !broken
            && (platforms == [] || builtins.elem hostSystem platforms);
          position = null;
          outputs = outputNames (drv.outputs or []);
          aliases = [];
          skipped = false;
        }));
    in
      if evaluated.success then evaluated.value
      else { attrPath = name; skipped = true; })
  names
