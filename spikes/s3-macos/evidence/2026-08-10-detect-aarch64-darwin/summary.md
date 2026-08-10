# S3 macOS Detect — Complete observed candidate evidence

| Field | Value |
|---|---|
| mode / state | `detect` / `complete` |
| harnessOnly / source | `false` / `observed` |
| host system | `aarch64-darwin` |
| Nix | present; pinned probe target `2.34.8` |
| Xcode | full Xcode selected |
| `nixbld` | group present; 32 `_nixbld*` users |
| Developer ID Application identities | 0 |
| Developer ID Installer identities | 0 |
| Apple tools | codesign, xcrun/notarytool/stapler, pkgbuild/productbuild/productsign, spctl, security all present |

Inactive Fake, Preflight, BuildProbe, and SignPlan lanes are Pending /
NotSelected. This is capability evidence only. It is not a cache-coverage,
native-build, signature, submission, notarization, stapling, or Gatekeeper result.
