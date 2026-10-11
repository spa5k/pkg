# Requirements

Recognized native profile file conflicts SHALL identify the profile-relative
file in normal output. Known owners SHALL use the package IDs shown by list.
Install conflicts SHALL show the canonical pending IDs. Installed and requested
IDs SHALL carry source labels. Identical installed packages SHALL retain the
successful `Already installed` result.
Normal output SHALL hide store paths, replacement commands, and native priority instructions.
Verbose output SHALL retain native diagnostics.

Named owners MUST be active entries in the re-read profile. Multiple owners
SHALL show their exact entry IDs. Unknown profile state MUST NOT identify an
installed owner as verified.

The operation MUST retain its failure status and verified profile outcome.
The client MUST NOT remove a conflicting package, change priorities, or
silently reuse a different package from another source.
