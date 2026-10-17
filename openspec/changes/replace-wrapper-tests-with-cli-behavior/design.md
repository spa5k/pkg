# Test ownership

The compiled CLI owns option refusal, validation order, bad config handling, and
native history presentation. Every helper starts with a temporary home and
explicit XDG bases. Child process output supplies the color boundary cases.

The existing native CLI check owns JSON output against the real Nix manifest,
source and entry selection, aliases, fresh and broken doctor state, and retention
of recent profile generations. It uses local derivations and disposable profiles.
The Linux and macOS smoke jobs already run this check.

Keep a narrow config test for the downcastable TOML cause. No CLI output can
establish that type contract. Remove the default serialization roundtrip because
production does not write that representation.

Before removal, run each keeper and compare the schema test with an isolated
version mutation. Also verify the new CLI checks against option, config, age,
and color mutations. Keep the bounded results in the audit report. The existing
CI routing stays unchanged.
