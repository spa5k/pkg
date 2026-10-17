# Replace wrapper tests with CLI behavior

## Why

Parser-only checks duplicate the compiled CLI. Some output tests construct the
same values they assert. The schema test accepts a version change because it
uses the production constant as its expected value. Some CLI runs inherit the
developer's home and configuration.

## What changes

Move the reviewed grammar, path, config, output, and doctor assertions to the
compiled CLI and the existing real Nix lifecycle check. Remove their redundant
internal tests after keeper proof. Keep the typed config error check because
CLI text cannot prove that the original error type survives.

## Scope

This change has no product behavior change. It adds no test framework or
production export. Other filesystem, process, parser, and security tests remain
outside this pruning batch. The audit records the reviewed declarations and
execution limits.
