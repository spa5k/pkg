# Synthetic public-tap-check fixture. Data only; never executed here.
# Expected result: see tools/public-tap-check/matrix.json "pubtap-flights".
cask "pubtap-flights" do
  version "2.0.0"
  sha256 "5984504c57dbc90f94f27de2a00e772860ceefdfd030dc9aa3ada143c44ff787"
  url "https://example.com/pubtap/pubtap-flights/pubtap-flights-2.0.0.zip"
  name "PubTap Flights"
  desc "Legacy preflight and postflight callbacks (synthetic fixture)"
  homepage "https://example.com/pubtap/pubtap-flights"

  app "FlightsTool.app"
  preflight do
    FileUtils.mkdir_p("/opt/pubtap-flights")
  end
  postflight do
    puts "pubtap-flights postflight"
  end
end
