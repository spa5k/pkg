# macOS technical preview package

This recipe builds a script-only macOS component package.

The package contains the shipping `pkg-install` artifact in its script area.
It does not compile code on the install host.
It does not install product files before `pkg-install` authenticates the product bundle.

Run this command on macOS:

```sh
packaging/macos/build-preview.sh \
  /absolute/path/to/pkg-install \
  /absolute/path/to/pkg-0.1.0-alpha.3-preview.pkg
```

The recipe ad-hoc signs a temporary copy of `pkg-install`.
The result is for local technical-preview tests only.

Use a shipping `pkg-install` binary that was compiled with the signed TUF root and the fixed HTTPS metadata and target URLs.
The manual macOS proof workflow is the canonical preview build recipe.

The package is not Developer ID signed.
The package is not notarized.
The package is not proven Gatekeeper-clean.

TODO: Add Developer ID signing and notarization after the required credentials are available.
