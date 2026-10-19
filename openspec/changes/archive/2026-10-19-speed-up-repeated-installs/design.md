# Design

## Native duplicate checks

Keep pkg's comparison against all active profile entries.
The native add check only starts at one native entry key. Same-name entries
from different sources can bypass its duplicate test.
Both macOS and Linux reproduced a third entry and a new generation.
See [Nix 2.35.2 profile add](https://github.com/NixOS/nix/blob/2.35.2/src/nix/profile.cc#L336).

## Batched live evaluation

Group only requests with matching installed candidates and plain attribute
segments. Resolve each source to one locked reference for the check.
Evaluate at most 128 requests per process with a bounded JSON argument.
Check the native package, legacy package, and direct attribute paths.
A fast result needs exactly one existing derivation path. Other cases retain
the existing per-request native search and output check.
Validate the same name and description metadata that native search reads.
The fast path covers default outputs only.
Keep explicit output selection in the existing exact-check route.
The public CLI still refuses explicit output suffixes and quoted Nixpkgs names.
Compare full attribute paths and sorted store output paths with all active
matching entries. Keep original references separate from locked references.

Cancellation must stop immediately. A failed or resource-limited batch can
fall back to the existing checks. Malformed batch output must not authorize
a duplicate decision. Exact source reads and real Nix tests must verify this
path before the prototype becomes production code.

The reference prefix and cursor behavior are from
[the Nix installable implementation](https://github.com/NixOS/nix/blob/2.35.2/src/libcmd/installable-flake.cc#L15)
and [native search](https://github.com/NixOS/nix/blob/2.35.2/src/nix/search.cc#L172).
