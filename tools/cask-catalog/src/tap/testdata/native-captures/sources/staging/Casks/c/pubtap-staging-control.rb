# Synthetic public-tap-check fixture. Data only; never executed here.
# Expected result: see tools/public-tap-check/matrix.json "pubtap-staging-control".
cask "pubtap-staging-control" do
  version "1.0.0"
  sha256 "2e5f61120bd5d7bc6c4080efcf084aeda67effce79d27fa28f9b7e8de7d21de7"
  url "https://example.com/pubtap/pubtap-staging-control/pubtap-staging-control-1.0.0.zip"
  name "PubTap Staging Control"
  desc "Valid positive control for its isolated tree (synthetic fixture)"
  homepage "https://example.com/pubtap/pubtap-staging-control"

  on_macos do
    binary "bin/pubtap-staging-control"
  end
  on_linux do
    binary "bin/pubtap-staging-control"
  end
end
