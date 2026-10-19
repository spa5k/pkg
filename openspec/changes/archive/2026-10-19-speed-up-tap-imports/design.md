# Design

First measure the existing export phases in a temporary source copy.
Use shell timestamps inside the normal sandbox build.
Keep every fresh isolation probe in the timed path.
Select a production change only after the phase data supports it.
Any change must preserve exact captured metadata and failed-import behavior.

## Existing standalone boot follow-up

The exporter already has a direct Ruby boot path through the pinned upstream
standalone.rb. Test that path in a temporary source copy before any change.
Keep the existing adapter environment setup. Skip only its final brew.sh
handoff for this prototype. Keep the actual upstream loader, vendor gems,
per-Cask child isolation, output and time bounds, and fresh live probe.
Use existing first-party native capture fixtures as the same input data.
Require exact exported bytes. A startup failure or different capture rejects
the prototype. A faster result alone does not justify a production change.


## Complete Ruby runtime follow-up

The first direct entry omits brew_irb_helpers and shared shim PATH state.
It also leaves the Ruby program name at entry.rb. Reject that prototype.
Keep the pinned brew.sh environment and global startup in the next trial.
Load the same upstream Ruby helper file. Preserve the shared PATH prefix.
Set the program name to the exporter and consume only its fixed arguments.
Delete the temporary entry selector before loading any tap code.
Keep the original brew.rb for nested Homebrew commands.
Measure a first-party fixture that requires all of this runtime state.
Every build retains fresh positive controls and the unchanged sandbox probe.


## Upstream command-object trial

The manually recreated Ruby entry also changes running_command state.
Normal brew ruby executes a fresh Ruby process through its upstream command.
Test calling Homebrew::DevCmd::Ruby.new.run after the normal global startup
and shared PATH setup. Keep its actual exec, helper loading, program name,
and process state. Delete the private selector before loading upstream code.
The runtime fixture compares running_command_with_args in its capture.
The original brew.rb remains available to all child Homebrew invocations.
