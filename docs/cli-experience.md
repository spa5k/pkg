# CLI experience

This is the acceptance checklist for PUBLIC-12 and PUBLIC-13 in the
[active plan](../plans/determinate-nix-stacked-prs.md).

Use one quiet visual language: cyan headings, green completed checks, amber
warnings, red failures, and muted secondary text. Color must never carry the
only meaning. Show a clear command title, the useful result, and a short next
step. Avoid large banners, nested boxes, and repeated progress lines.

Running `pkg` opens the command guide. Every command has usage and examples.
Short help focuses on daily options. Full help also lists advanced options.
Command names stay lowercase. Keep normal shell conventions and existing flags.

| Command | Human result and guidance |
| --- | --- |
| `search` | Package, version, availability, description; next step to inspect or install |
| `info` | Package details with license, outputs, availability, and homepage |
| `install` | Installed packages, versions, outputs, saved generation; local build plan before approval |
| `list` | Installed selectors, versions, pins; optional outputs and readable disk size |
| `remove` | Selected packages and a clear completed or preview status |
| `outdated` | Installed and available versions, pins, update type; flake limitations stay visible |
| `update` | Catalog refresh or check result; next step to inspect package updates |
| `upgrade` | Changed packages, skipped pins, saved generation |
| `pin` / `unpin` | Changed and unchanged packages; next step explains upgrade behavior |
| `history` | Saved environments, dates, active marker; differences or deletion preview |
| `rollback` | Source and target environments, package count, completed or preview status |
| `gc` | Selected or removed generations, collected paths, readable disk values |
| `repair` | Generation, damaged path count, verification or repair result |
| `doctor` | PASS, WARN, FAIL, WAIT checks with corrective instructions |
| `shellenv` | Only valid shell source; help explains how to load it |
| `completion` | Only completion source for Bash, Zsh, Fish, or PowerShell |
| `uninstall` | Whole-product scope, explicit approval, preview or completed status |

Terminal tables wrap long values. Small terminals use compact labeled cards.
Use display width for Unicode text. Read terminal dimensions, with `COLUMNS`
as an explicit override. Keep redraws within one line, including after a resize.

A preview must never show a completed mutation. Show the planned targets and
say that no changes were applied. Unknown estimates stay unknown. A disk
threshold is not a prediction of total build size. Build percentages come only
from reported progress. Keep prompts separate from active progress.

Errors retain stable codes and provide one corrective next step. Cancellation
must remain cancellation. Never hide a failure behind a successful check mark.

JSON and JSONL keep their schemas. Redirected and CI output use static text.
`--no-color`, `NO_COLOR`, and `TERM=dumb` disable ANSI. `--quiet` keeps the final
result. `list --name-only`, `shellenv`, and `completion` remain safe to pipe.
Result text must pass the existing public-data validation before rendering.

Review all command help pages, real daily commands in a disposable VM, wide and
narrow terminal transcripts, and the existing machine-output tests. The CLI
must not modify the developer host during this validation.
