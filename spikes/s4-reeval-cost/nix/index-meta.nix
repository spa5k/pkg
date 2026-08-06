# pkg — Spike S4 (PR-6 / DR-004): index meta projection over legacyPackages.
#
# This file is the MAINTAINED projection expression. It is a FUNCTION of one
# argument — the evaluated `legacyPackages.<system>` attribute set — and returns
# a JSON-safe, deterministically ordered list of BOUNDED per-attribute records.
# It never builds anything (meta-only evaluation).
#
# Pure-flake mechanism (no `--impure`, no `NIX_PATH`, no mutable channel):
#
#     nix eval --json \
#       'github:NixOS/nixpkgs/<rev>?narHash=<url-encoded-hash>#legacyPackages.<system>' \
#       --apply "$(cat nix/index-meta.nix)"
#
# Nix applies this function to the lazy `legacyPackages.<system>` value. We only
# force what we extract, and we force each retained primitive with
# `builtins.deepSeq` so a lazy `throw`/`assert` surfaces inside the surrounding
# `builtins.tryEval`. IMPORTANT: `builtins.tryEval` catches ORDINARY evaluation
# errors such as `throw` and `assert` failures, but it does NOT catch
# `builtins.abort`, which terminates the whole evaluation regardless. That
# `abort`-uncatchability is a documented Real-lane limitation: an attribute that
# calls `builtins.abort` aborts the entire projection, and we never claim
# `tryEval` swallows every attribute failure. `builtins.attrNames` enumerates
# top-level names in deterministic (lexicographic) order; the emitted list is
# therefore deterministic. `outputs` is normalized to a sorted, length-capped
# list of output-name strings (see `normalizeOutputs`). All string fields are
# length-capped so the result is bounded and JSON-safe.
#
# Scope of `deepSeq` (deliberate, documented decision): we `deepSeq` only the
# small primitive fields we keep (pname/version/boolean broken/the normalized
# `outputs` list), NOT the entire derivation or nested package sets. Deep-forcing
# a whole nested attrset such as `python3Packages` would force tens of thousands
# of derivations and is the classic meta-eval footgun; bounding the deep force
# to the kept primitives still realizes a throwing attribute (the intent of the
# `tryEval` + `deepSeq` requirement) without making the projection impractical.
# This is a spike-owned, defensible scope choice recorded in `findings.md`.
#
# This is NOT production pkg code (the product index builder is PR-14). It exists
# to measure the cost described in `plans/03` §8.2 and to feed DR-004.

pkgs:

let
  # Deterministic top-level attribute enumeration (sorted by Nix).
  names = builtins.attrNames pkgs;

  # Cap a string at `n` characters; pass through non-strings as null so the
  # record stays JSON-homogeneous (no functions / derivations leak).
  capStr = n: s:
    if builtins.isString s
    then (if builtins.stringLength s > n then builtins.substring 0 n s else s)
    else null;

  # Cap a single output name at `n` characters. The caller guarantees a string.
  capName = n: name:
    if builtins.stringLength name > n
    then builtins.substring 0 n name
    else name;

  # Sort a list of strings into ascending lexicographic order. Shared by both
  # `outputs` shapes so they produce the identical deterministic ordering.
  sortStrs = builtins.sort (a: b: a < b);

  # Normalize a derivation's `outputs` field to a sorted, length-capped list of
  # output-name strings. Modern Nixpkgs derivations carry `outputs` as a LIST of
  # strings (e.g. [ "out" "dev" ]); a LEGACY attrset form (name -> output) may
  # also appear. Any other type (function, int, the missing-attribute default,
  # ...) normalizes to an empty list so the record stays JSON-homogeneous and
  # bounded:
  #   * list    -> keep only strings, cap each name at 4096 chars, sort;
  #   * attrset -> attrNames (already sorted) each capped at 4096 chars, sort;
  #   * other   -> [ ].
  # Only shallow forces are used here (isList/isAttrs/isString/attrNames); the
  # retained strings are the primitives deep-forced by `force` below, so the
  # projection stays pure and deterministic.
  normalizeOutputs = raw:
    let
      cap = builtins.map (capName 4096);
    in
    if builtins.isList raw
    then sortStrs (cap (builtins.filter builtins.isString raw))
    else if builtins.isAttrs raw
    then sortStrs (cap (builtins.attrNames raw))
    else [ ];

  # Force a value deeply and return it, so a lazy `throw`/`assert` is realized
  # and caught by the surrounding `builtins.tryEval`. As noted above,
  # `builtins.abort` is NOT catchable by `tryEval` and remains a Real-lane
  # limitation.
  force = x: builtins.deepSeq x x;
in
builtins.map
  (name:
    let
      r =
        builtins.tryEval
          (
            let
              d = pkgs.${name};
              meta = d.meta or { };
              pname = force (capStr 4096 (d.pname or null));
              version = force (capStr 4096 (d.version or null));
              broken =
                force
                  (
                    let b = meta.broken or false; in
                    b == true
                  );
              outputs = force (normalizeOutputs (d.outputs or null));
            in
            # One more deepSeq over the small finished record so any structural
            # throw in field assembly is also caught (cheap: all fields are now
            # forced primitives).
            force {
              attrPath = name;
              inherit pname version broken outputs;
            }
          );
    in
    if r.success then r.value else { attrPath = name; skipped = true; }
  )
  names
