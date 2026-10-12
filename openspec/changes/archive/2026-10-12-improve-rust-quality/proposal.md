# Improve Rust error handling and ownership

Rust errors lose their causes when command helpers convert them to strings.
This also changes cancelled cask discovery into an ordinary failure.
Tap queries clone complete catalog indexes and use `RefCell` for local mutation.
Metadata rows use a string tag with optional fields that allow invalid states.

Keep typed errors through command reporting.
Use enums for metadata states.
Share immutable catalog indexes and borrow the query cache mutably.
Remove impossible error branches and redundant wrappers.
Keep the native Nix profile as the installed state owner.

This change follows the native client cleanup in PR #113.
It does not require a line reduction target.
