# Synthetic public-tap-check fixture. Data only; never executed here.
# Expected result: see tools/public-tap-check/matrix.json "pubtap-release-archive".
cask "pubtap-release-archive" do
  version "1.2.3"
  sha256 "e631450fc7ff7d48518eb85a31b20925e96fb993cb5ad6320cce88b52f5c983a"
  url "https://example.com/pubtap/pubtap-release-archive/pubtap-release-archive-1.2.3.zip"
  name "PubTap Release Archive"
  desc "Release archive with per-OS artifacts (synthetic fixture)"
  homepage "https://example.com/pubtap/pubtap-release-archive"

  on_macos do
    app "PubTapReleaseArchive.app"
  end
  on_linux do
    binary "bin/pubtap-release-archive"
  end
  zap trash: "~/.pubtap/pubtap-release-archive"
end
