# Literal local/private destination; no DNS name, no redirect.
# Expected: excluded on both targets; the destination policy must
# refuse it without a lookup.
cask "pubtap-url-ipv6" do
  version "1.0.0"
  sha256 "0fb67c78c1a77a9a6f766c3e50ca88240fad9927b4f9f27f18b0d0053e94ba05"
  url "http://[::1]:9/pubtap/url-ipv6.zip"
  name "PubTap URL IPv6 Loopback"
  desc "Literal local destination (synthetic fixture)"
  homepage "https://example.com/pubtap/pubtap-url-ipv6"
  on_macos do
    binary "pubtap-tool"
  end
  on_linux do
    binary "pubtap-tool"
  end
end
