# Synthetic public-tap-check fixture. Data only; never executed here.
# Expected result: see tools/public-tap-check/matrix.json "pubtap-raw-binary".
cask "pubtap-raw-binary" do
  version "0.9.0"
  sha256 "cba9b3b8cb9cab89f2a127da96a343f65dcd13bef8a7f9d0179bd2174281ca21"
  url "https://example.com/pubtap/pubtap-raw-binary/pubtap-raw-binary-0.9.0.bin"
  name "PubTap Raw Binary"
  desc "Raw binary downloaded under its own basename, renamed on install (synthetic fixture)"
  homepage "https://example.com/pubtap/pubtap-raw-binary"

  # the binary source is the download basename; the target is a deliberate rename
  on_macos do
    binary "pubtap-raw-binary-0.9.0.bin", target: "pubtap-tool"
  end
  on_linux do
    binary "pubtap-raw-binary-0.9.0.bin", target: "pubtap-tool"
  end
end
