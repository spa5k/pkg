# Formula catalog

This maintainer tool generates the safe Formula subset for pkg. The client
uses the generated Nix source. It does not need Brew or the Homebrew API.

The committed gzip snapshot contains the official Core API response.
`inputs/input.json` records its source revision, SHA-256 digest, and license.
The generator verifies the decompressed bytes before it uses them.
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

To update Core data, fetch `https://formulae.brew.sh/api/formula.json`.
Keep the response below 64 MiB. Compute its SHA-256 digest. Pass the file
and digest to `generate.py --input PATH --sha256 DIGEST`. Review the result.
Replace the committed gzip snapshot with these exact response bytes.
Set gzip `mtime` to zero. Update `inputs/input.json` with the response digest
and `tap_git_head`. Regenerate from the committed snapshot before review.
The Nixpkgs pin in the generator and `nix/formulae/flake.lock` must agree.

The generator first selects a Nixpkgs equivalent with a matching name or
alias, homepage, and numeric version major. It records the native version.
Otherwise, it permits a dependency-free, non-relocating Core bottle with
no install hook, service, or special requirement. Disabled and deprecated
recipes remain excluded. The builder can refuse a metadata-eligible bottle
when its contents fail the checks. Eligible does not mean build-verified.

`nix/casks/lib` owns the common public fetch helpers. Generation copies them
into the self-contained Formula flake. Change the source copies and regenerate.

Homebrew Core metadata is provided under BSD-2-Clause. See
[the retained license](inputs/LICENSE.txt).
Package licenses remain those of their upstream projects. The data license
does not change the package license.
