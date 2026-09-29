# Literal local/private destination; no DNS name, no redirect.
# Expected: excluded on both targets; the destination policy must
# refuse it without a lookup.
cask "pubtap-url-linklocal" do
  version "1.0.0"
  sha256 "559eeac84b91b77f702297300d4336a9e91976334fbd945244ec11b316a92680"
  url "http://169.254.169.254/pubtap/url-linklocal.zip"
  name "PubTap URL Link-Local"
  desc "Literal local destination (synthetic fixture)"
  homepage "https://example.com/pubtap/pubtap-url-linklocal"
  on_macos do
    binary "pubtap-tool"
  end
  on_linux do
    binary "pubtap-tool"
  end
end
