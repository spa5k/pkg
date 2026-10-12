# Design

The Nix adapter decodes the verified multiline profile conflict diagnostic.
ANSI formatting does not affect decoding. Both paths must identify the same
relative file beneath separate store outputs. Unrecognized diagnostics retain
the ordinary failure report.

Normal live reporting suppresses the native conflict block. Warnings and
interactive prompts stay visible. Verbose output retains the native block.

The command re-reads the authoritative profile after failure. It finds every
active owner by comparing the existing file with each output path using path
component boundaries. It derives package IDs from the current configuration.
Every matching owner uses the same package ID as list. If several entries
provide the file, show their native entry IDs with their package IDs.
An install failure also shows the canonical pending IDs and source labels.
An identical installed output keeps the successful `Already installed` result.

If the profile or owner is unknown, show the relative file without naming an
unverified owner. Show no replacement commands. Never infer equivalence from
matching names or versions.
