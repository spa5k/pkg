# Synthetic public-tap-check fixture. Data only; never executed here.
# Expected result: see tools/public-tap-check/matrix.json "pubtap-dup".
cask "pubtap-dup" do
  version "1.0.0"
  sha256 "50e12a43f5c68f998a78e5f9d60b1f31932459322a405efa682f56a97fea64ee"
  url "https://example.com/pubtap/pubtap-dup/pubtap-dup-1.0.0.zip"
  name "PubTap Duplicate One"
  desc "Duplicate token, file one (synthetic fixture)"
  homepage "https://example.com/pubtap/pubtap-dup"

  app "DupOne.app"
end
