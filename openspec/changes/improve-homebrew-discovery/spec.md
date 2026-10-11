# Requirements

- Formulae SHALL install through Nix into the native pkg profile.
- The client SHALL NOT install, embed, or invoke Brew.
- A generated catalog SHALL prefer verified Nixpkgs equivalents.
- Equivalents SHALL match the upstream project and numeric version major.
- The index SHALL report the actual selected package version.
- A restricted bottle importer SHALL refuse dependencies, hooks, services,
  unknown requirements, relocation, and unsupported origins.
- The builder SHALL reject archive escapes, external links, and remaining
  Homebrew paths before it publishes an output.
- Install and upgrade SHALL check recorded support before profile mutation.
- Formulae SHALL use native installed state, removal, history, and rollback.
- Normal search SHALL include prepared official Homebrew Formula metadata.
- Search SHALL use local data without external commands or network requests.
- Formula aliases SHALL return canonical formula IDs.
- Failed preparation SHALL retain the previous complete catalog.
- Human output SHALL show source sections with counts and a global limit.
- The default human limit SHALL allocate space across nonempty source sections.
- JSON SHALL retain flat global ranking and a global result limit.
- Exact unsupported names SHALL show their refusal reason.
- Eligible Formula rows SHALL suggest a pkg install command.
- Scoped Formula commands SHALL ignore unrelated broken tap state.
