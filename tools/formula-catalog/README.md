# Formula catalog

This maintainer tool generates the safe Formula subset for pkg. The client
uses the generated Nix source. It does not need Brew or the Homebrew API.

`inputs/input.json` pins the official Core API payload: its URL in the
`catalog-inputs` pre-release, its `tap_git_head` revision, its SHA-256
digest, and its license. The generator downloads the gzip asset into its
cache and verifies the decompressed bytes before it uses them.
It evaluates matching metadata at a fixed Nixpkgs revision for both supported
systems. It never evaluates Formula Ruby.

Regenerate the committed source:

```sh
python3 tools/formula-catalog/generate.py
python3 tools/formula-catalog/tests.py
```

The first run needs Nix and network access for pinned Nixpkgs metadata.
Later runs reuse disposable metadata under `.cache/formula-catalog`.
Delete that directory to repeat the native evaluation.

Cold evaluation uses batches of at most 4,096 candidate names.
Each encoded name list stays below 64 KiB to leave room in the native
command argument. A batch shrinks when names are long.
An oversized single name fails before Nix starts.
Use `python3 tools/speed-check formula-metadata` with a new `--cache` path
to measure a cold run and check the exact committed catalog bytes.

The `catalog-update` workflow updates Core data every day. See the
[Cask catalog update procedure](../../docs/casks.md#catalog-update-procedure).
To update it by hand, run
`cargo run --locked -p cask-catalog -- fetch-signed --name formula --out PATH`.
The command checks Homebrew's signature and prints the payload digest.
Store `gzip -9 -n` of the payload as `formula-<sha256>.json.gz` in the
`catalog-inputs` pre-release. Then run
`generate.py --input PATH --sha256 DIGEST --pin-url URL` to regenerate the
catalog and rewrite `inputs/input.json`. Keep the payload below 128 MiB.
The Nixpkgs pin in the generator and `nix/formulae/flake.lock` must agree.

The generator first selects a Nixpkgs equivalent with a matching name or
alias, homepage, and numeric version major. It records the native version.
Otherwise, it permits a dependency-free, non-relocating Core bottle with
no install hook, service, or special requirement. Disabled and deprecated
recipes remain excluded. Bottles are supported on Apple silicon macOS.
Linux uses Nixpkgs equivalents and refuses bottles that need Linux relocation.
The builder can refuse a metadata-eligible bottle
when its contents fail the checks. Eligible does not mean build-verified.

`nix/casks/lib` owns the common public fetch helpers. Generation copies them
into the self-contained Formula flake. Change the source copies and regenerate.

Homebrew Core metadata is provided under BSD-2-Clause. See
[the retained license](inputs/LICENSE.txt).
Package licenses remain those of their upstream projects. The data license
does not change the package license.
