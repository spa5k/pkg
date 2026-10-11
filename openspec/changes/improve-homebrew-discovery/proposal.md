# Show Homebrew formulae and source sections

Mixed search results make Homebrew Casks hard to find. Homebrew Formulae
are absent from discovery. Add the official formula catalog and clear
sections for each source. Keep normal search local and bounded.

Both Formulae and Casks install through Nix into the pkg profile. Formulae
use verified Nixpkgs equivalents or a restricted bottle importer. The
client never installs, embeds, or invokes Homebrew. Unsupported Formulae
have recorded reasons and are refused before a profile change.

This extends [CLI discovery](../improve-cli-discovery/proposal.md).
