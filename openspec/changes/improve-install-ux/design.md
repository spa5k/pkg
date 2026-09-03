# Design: Installation experience

## Context

Install surfaces today: a macOS `.pkg` built by the release workflow, staged Linux binaries,
and the proof harness that orchestrates them. None is a user path. This change builds the user
path on top of the existing verification machinery instead of parallel to it.

## Decisions

### D1. The script is a courier, not a bootstrapper

`install.sh` does detection, fetch, verify, invoke — nothing else. All install logic stays in
the signed product binaries. The script must remain small enough to audit by reading
(< ~300 lines), because it is the one component users run before any trust is established.

### D2. Verify through the same TUF client the product uses

The script does not implement its own TUF logic; it downloads the verified release manifest
through a minimal static helper (`kelv-fetch`, a tiny release of the existing channel client)
or, before that helper exists, through the product binary's own `doctor --fetch-manifest`
mode. One verification implementation, zero drift.

### D3. Preflight rules are the product's rules

Every preflight in the script calls a product-provided check (`doctor --preflight`) where
possible, so the script cannot accept an installation the product would refuse. Shell-only
checks (disk space, TTY) stay local.

### D4. Failure messaging from the journal, not from vibes

Installer failure messages derive from the journal state machine: the phase is read from the
journal's last entry, the on-disk state from the existing state classification, and the next
action from the documented recovery matrix (the same matrix the fault-injection tests prove).
This keeps what users are told in lockstep with what is actually true.

### D5. Both scripts ship with the channel

`install.sh` and `uninstall.sh` are versioned artifacts in the channel (TUF targets), not
hand-edited files on the web host. `https://kelv.dev/install.sh` is a stable redirect to the
current channel copy. Users can pin a script version the same way they pin a release.

## Risks / Trade-offs

- **curl | sh trust bootstrap**: the script is the initial trust root for a new user until the
  product's pinned TUF root takes over. Mitigation: the script is tiny, audit-able, and prints
  the release digests it will verify against before fetching. Full cure (signed script) is the
  trust-ceremony change.
- **Doctor dependency ordering**: post-install verification requires the CLI-UX change's
  `doctor`. Sequenced as a dependency; the install change lands after.
- **Two-script maintenance**: install and uninstall share a common sourced library file
  (`lib.sh`) to avoid drift.

## Open Questions

- Should `install.sh` support `--channel <url>` for private channels on day one? Proposal:
  yes, one flag, defaulting to production; the proof harness already needs it.
