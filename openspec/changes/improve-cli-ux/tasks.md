# Tasks: CLI user experience

## 1. Foundations

- [ ] 1.1 `UxErrorCode` enum in `pkg-core` with the six-class mapping (usage, state, channel,
      verification, internal, success) plus per-failure-class variants
- [ ] 1.2 Map installer/channel error enums to `UxErrorCode` in one conversion module per crate
- [ ] 1.3 Exit-code table in `pkg-cli::exit` implementing the documented mapping
- [ ] 1.4 `ux::Text` registry module with stable identifiers for all strings in scope below

## 2. Help

- [ ] 2.1 Usage + examples + docs link for every existing command (install, update, list,
      repair, uninstall, plus broker/helper subcommands where user-facing)
- [ ] 2.2 Golden test running `--help` for every command and comparing to
      `crates/pkg-cli/tests/golden/help/`
- [ ] 2.3 Docs page `docs/cli.md` with the exit-code table and JSON schema

## 3. Errors and progress

- [ ] 3.1 Human error renderer: what happened + next action, sourced from the registry
- [ ] 3.2 `--json` error shape with `code`, `class`, `message`, `detail`
- [ ] 3.3 Phase-line progress emitter wired into install/update/repair/uninstall operations,
      elapsed from the injected clock
- [ ] 3.4 Context detection (TTY, `CI`, `--quiet`) gating progress; errors always print
- [ ] 3.5 Golden tests for one error and one progress transcript per command family

## 4. Safety rails

- [ ] 4.1 Interactive confirmation for uninstall and repair reading from the TTY
- [ ] 4.2 `--yes` flag; non-interactive refusal path tested in both commands
- [ ] 4.3 `--dry-run` for repair reusing the existing plan printing

## 5. Doctor

- [ ] 5.1 `doctor` command composing preflight, channel probe, TUF root comparison, and
      journal/filesystem consistency into a pass/fail table
- [ ] 5.2 Exit codes per the mapping (healthy=0, channel=3, verification=4)
- [ ] 5.3 Doctor transcript golden test with a fake channel fixture

## 6. Wrap-up

- [ ] 6.1 Full workspace tests, clippy, fmt, quality gate
- [ ] 6.2 Rebase `tools/quality/baseline.json` if lint sites moved
- [ ] 6.3 Update `docs/cli.md` cross-links from README
