# Safe vendor payload fetch (fixed-output derivation).
#
# Interface: { pkgs }: { url, sha256 }: derivation producing the fetched
# flat file, with outputHash = sha256, outputHashAlgo = "sha256",
# outputHashMode = "flat". Nix verifies the hash itself; the Python
# helper verifies it again before the atomic rename.
#
# Safety shape:
#   - URL and hash travel as environment variables into a plain argv
#     exec of ./safe-fetch.py under pinned pkgs.python3. No vendor
#     string is ever interpolated into shell syntax.
#   - pkgs.cacert supplies the trust roots; no host certificates.
#   - ./safe-fetch.py enforces public-only HTTPS destinations, port 443
#     only, pinned connects with original-hostname SNI/Host, bounded
#     redirects (relative Locations resolved then revalidated), bounded
#     size, streamed SHA-256, and atomic rename on success only.
#   - timeout(1) from coreutils is the outer deadline guard.
#
# These files are standalone: an importer copies them into generated
# flakes unchanged. Data-only source.file overrides in the parent
# builder are untouched by this helper.
{ pkgs }:
{
  url,
  sha256,
}:
pkgs.stdenvNoCC.mkDerivation {
  name = "vendor-payload";

  # Environment (never shell-interpolated vendor input).
  inherit url sha256;
  safeFetch = ./safe-fetch.py;
  fetchPython = pkgs.python3;
  inherit (pkgs) coreutils;
  SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
  # stdenv sets NIX_SSL_CERT_FILE=/no-cert-file.crt, which overrides
  # SSL_CERT_FILE and yields zero CAs. Pin it to the same bundle.
  NIX_SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";

  dontUnpack = true;
  dontConfigure = true;
  dontBuild = true;
  dontFixup = true; # flat byte-for-byte output; the hash is the contract.

  buildCommand = ''
    # Final guard: the helper enforces a strict 900s deadline itself;
    # timeout bounds it at 960s and SIGKILLs 30s after SIGTERM so a wedged
    # helper can never hang the build forever.
    "$coreutils/bin/timeout" --kill-after=30s 960s \
      "$fetchPython/bin/python3" "$safeFetch" \
        --url "$url" \
        --sha256 "$sha256" \
        --output "$TMPDIR/vendor-payload" &&
    # The helper writes beside its output via mkstemp; the Nix store
    # does not permit siblings beside $out, so move the verified flat
    # file into place only after the helper succeeded.
    "$coreutils/bin/mv" -- "$TMPDIR/vendor-payload" "$out"
  '';

  outputHash = sha256;
  outputHashAlgo = "sha256";
  outputHashMode = "flat";
}
