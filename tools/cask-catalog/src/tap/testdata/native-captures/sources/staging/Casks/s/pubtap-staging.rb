# Synthetic public-tap-check fixture. Data only; never executed here.
# Expected result: see tools/public-tap-check/matrix.json "pubtap-staging".
cask "pubtap-staging" do
  version "1.0.0"
  sha256 "df6e6fdec62af3e15826e81f1a6f73c7c645d259a75da5398993c0d6f15288f1"
  url "https://example.com/pubtap/pubtap-staging/pubtap-staging-1.0.0.zip"
  name "PubTap Staging"
  desc "Payload-dependent artifact list (synthetic fixture)"
  homepage "https://example.com/pubtap/pubtap-staging"

  app "StagingTool.app"
  binary "#{staged_path}/bin/staging-tool"
end
