# Literal local/private destination; no DNS name, no redirect.
# Expected: excluded on both targets; the destination policy must
# refuse it without a lookup.
cask "pubtap-url-localhost" do
  version "1.0.0"
  sha256 "3f303bffae5699ec7299aa1002ae49a3bac705d6c481b1ea0a081ec38be2f838"
  url "http://localhost:9/pubtap/url-localhost.zip"
  name "PubTap URL Localhost"
  desc "Literal local destination (synthetic fixture)"
  homepage "https://example.com/pubtap/pubtap-url-localhost"
  on_macos do
    binary "pubtap-tool"
  end
  on_linux do
    binary "pubtap-tool"
  end
end
