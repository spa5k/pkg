# Public tap approval and security boundaries

Checked on 29 September 2026. Research only. No tap code ran. No service
or sandbox was started. Context7 was queried before primary source review.

## Finding

A first-use public-tap confirmation is a useful consent boundary. It is
not an isolation boundary. Providing native Ruby through Nix does not
change what the Ruby process can access. The relevant question is where
that process runs and which resources it can reach.

There are three separate decisions: trust executable metadata, accept the
generated installation plan, and run the installed vendor application.
Approval at the first stage does not establish safety at the other two.
It also does not establish that future tap revisions or app releases are
safe.

## Verified Homebrew behavior

Homebrew source below is pinned to
`cc9ff034b1d98cdd4061f68cf715bb967c483a8f`.

- The raw Cask path loader checks trust, then executes the file with
  `instance_eval`. It removes selected sensitive environment variables.
  That does not confine filesystem access or child processes.
  [Path loader][loader]
- The Cask refresh step evaluates its DSL block and selected language
  block. The DSL also evaluates caveats and livecheck declaration blocks.
  Thus metadata conversion can execute arbitrary Ruby before installation.
  [Cask refresh][cask] [DSL evaluation][dsl]
- Current Homebrew requires explicit trust for non-official taps by
  default. It supports either specific-item trust or whole-tap trust.
  Whole-tap trust covers current and future formulae, casks, and external
  commands. Its documentation explicitly describes loading tap code with
  the user's privileges. [Tap trust model][trust-doc] [Enforcement][trust]
- Official default installs normally use signed JSON metadata. This
  avoids local evaluation of official Ruby definitions on that path.
  Therefore raw public-tap conversion should not be compared only with
  an assumption that every Brew install evaluates Ruby locally.
  [Signed API metadata][security]
- Cask checksums establish agreement with declared bytes. They do not
  prove publisher identity or benign behavior. Vendor installer scripts
  and macOS package installers run outside Homebrew's sandbox.
  [Cask security model][security]

Consequently, neither “Brew is insecure” nor “Nix makes taps safe” follows
from these facts. A comparison must name the operation and its actual
isolation policy. A verified restricted conversion process could reduce
metadata execution exposure. That benefit must be demonstrated separately
from the interpreter's source and from installation behavior.

The Nix manual distinguishes ordinary sandboxed builds from fixed-output
builds. On Linux, fixed-output builds are an exception to network-namespace
isolation; this does not mean every sandbox restriction disappears.
The upstream macOS sandbox default is false. Require an effective enabled
policy and test its behavior with the installed daemon. `nix shell` alone
only supplies a tool environment. [Nix sandbox setting][nix-sandbox]

## Application runtime and macOS checks

A prebuilt application runs after its Nix build has finished. The build
sandbox does not remain around the launched application. The application
has its normal OS runtime permissions. It can still contain malicious
code, request sensitive access, or use a compromised update channel.
Immutable package files and rollback do not undo data access or external
side effects.

Apple describes separate checks for developer identity, signature
integrity, notarization, known malware, and first-open consent. These are
useful controls, not a general proof of safe behavior. A signature check
alone does not establish notarization or a complete Gatekeeper result.
[Apple Platform Security][apple-security] [Apple app-opening guidance][apple-open]

Homebrew says it applies quarantine attributes to Cask downloads and
requires assessable official macOS artifacts to pass Gatekeeper. A
different package path should not claim the same protection without
checking how it preserves download provenance, signatures, and the final
launch assessment. [Homebrew Cask protections][security]

Do not silently remove quarantine or replace a vendor signature to make
an app launch. Such changes can alter independent checks and provenance.
They are not a substitute for an unsupported-installation result. This is
a proposed policy; this note does not claim that the current pkg builder
implements it. It also does not claim that removing one attribute disables
all macOS malware protection.

## Proposed controls for the accepted design

1. Before first raw evaluation, identify the canonical source and explain
   that conversion executes its Ruby. Store consent per source. If consent
   covers later updates, state that scope plainly. A different origin or
   identity needs a new decision.
2. Pin and verify each source revision and payload hash. Update those pins
   only when requested. Preserve provenance in generated data. A pin
   identifies bytes; it does not certify them. Fetch public HTTPS sources
   with redirect and destination checks before evaluation.
3. Run native Ruby inside our own ordinary Nix derivation with the verified
   build isolation policy. Do not evaluate tap-provided Nix expressions or
   configuration. Keep credentials and the user's home outside that process.
   Never silently disable the sandbox. Bound outputs and validate generated
   JSON as untrusted data. Refuse unsupported host-dependent behavior.
4. Keep active installation operations visible. Reject unsupported scripts,
   callbacks, and structured steps. Do not translate them into successful
   installation by dropping them.
5. Treat application execution as a separate boundary. Preserve applicable
   platform checks. Describe catalog eligibility as supported metadata and
   build behavior, not a malware verdict.

These are proposals. Nix sandbox configuration and the current pkg builder
are reviewed separately. This note establishes neither sandbox parity
between operating systems nor an overall security ranking against Brew.

[loader]: https://github.com/Homebrew/brew/blob/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/Library/Homebrew/cask/cask_loader.rb#L167
[cask]: https://github.com/Homebrew/brew/blob/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/Library/Homebrew/cask/cask.rb#L173
[dsl]: https://github.com/Homebrew/brew/blob/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/Library/Homebrew/cask/dsl.rb#L698
[trust-doc]: https://github.com/Homebrew/brew/blob/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/docs/Tap-Trust.md
[trust]: https://github.com/Homebrew/brew/blob/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/Library/Homebrew/trust.rb#L237
[security]: https://github.com/Homebrew/brew/blob/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/docs/Homebrew-Security-and-Supply-Chain.md
[apple-security]: https://support.apple.com/guide/security/gatekeeper-and-runtime-protection-sec5599b66df/web
[apple-open]: https://support.apple.com/en-us/102445
[nix-sandbox]: https://nix.dev/manual/nix/2.35/command-ref/conf-file.html#conf-sandbox
