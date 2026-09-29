# Literal local/private destination; no DNS name, no redirect.
# Expected: excluded on both targets; the destination policy must
# refuse it without a lookup.
cask "pubtap-url-private" do
  version "1.0.0"
  sha256 "894fa06cd174705c6649460f65609e2a14fb14656d26c794e53af41cd44f0a02"
  url "http://192.168.1.9/pubtap/url-private.zip"
  name "PubTap URL Private"
  desc "Literal local destination (synthetic fixture)"
  homepage "https://example.com/pubtap/pubtap-url-private"
  on_macos do
    binary "pubtap-tool"
  end
  on_linux do
    binary "pubtap-tool"
  end
end
