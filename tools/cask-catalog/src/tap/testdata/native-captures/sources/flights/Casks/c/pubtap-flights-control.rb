# Synthetic public-tap-check fixture. Data only; never executed here.
# Expected result: see tools/public-tap-check/matrix.json "pubtap-flights-control".
cask "pubtap-flights-control" do
  version "1.0.0"
  sha256 "f63b5a4a664fda7c21d5c2ec98607755583a08417f79376cffdbad5d54f1ad2a"
  url "https://example.com/pubtap/pubtap-flights-control/pubtap-flights-control-1.0.0.zip"
  name "PubTap Flights Control"
  desc "Valid positive control for its isolated tree (synthetic fixture)"
  homepage "https://example.com/pubtap/pubtap-flights-control"

  on_macos do
    binary "bin/pubtap-flights-control"
  end
  on_linux do
    binary "bin/pubtap-flights-control"
  end
end
