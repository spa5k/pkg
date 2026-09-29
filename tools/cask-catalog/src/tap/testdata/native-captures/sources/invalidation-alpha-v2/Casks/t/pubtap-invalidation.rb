# Cask file is byte-identical in v1 and v2; only the helper differs.
require_relative "../../lib/pubtap_helper"
cask "pubtap-invalidation" do
  version PubtapHelper.version
  sha256 "5c0285bbc0ccae8dd62562d97dc47ec8b61f4c11e4b2f3639f8296e56efcb677"
  url "https://example.com/pubtap/pubtap-invalidation/pubtap-invalidation-#{PubtapHelper.version}.zip"
  name "PubTap Invalidation"
  desc "Helper-only invalidation fixture (synthetic)"
  homepage "https://example.com/pubtap/pubtap-invalidation"
  app "InvalidationTool.app"
end
