# tests/default.nix — OPTIONAL test runner for the bounded reader
# behavioral tests (tests/reader_test.rb). Not part of the production
# raw-export derivation: build it only when you want the in-Nix proof:
#
#   nix-build nix/casks/lib/tap/tests/default.nix
#
# Runs under the same pinned nixpkgs (Ruby 4.0 closure) as the
# exporter, with LC_ALL=C (the reader pipe protocol is binary).
{
  pkgs ? import (builtins.fetchTarball {
    url = "https://github.com/NixOS/nixpkgs/archive/567a49d1913ce81ac6e9582e3553dd90a955875f.tar.gz";
    sha256 = "1vq77hlx8mi3z03pw2nf6r5h7473r1p9yxyf58ym3fh01zppmfln";
  }) { },
}:
pkgs.runCommand "pkg-tap-reader-test" { nativeBuildInputs = [ pkgs.ruby_4_0 ]; } ''
  # Stage the test NEXT TO reader.rb so require_relative "../reader"
  # resolves exactly as in the source tree.
  mkdir -p "$NIX_BUILD_TOP/t/tests"
  cp ${../reader.rb} "$NIX_BUILD_TOP/t/reader.rb"
  cp ${./reader_test.rb} "$NIX_BUILD_TOP/t/tests/reader_test.rb"
  LC_ALL=C ruby "$NIX_BUILD_TOP/t/tests/reader_test.rb"
  touch "$out"
''
