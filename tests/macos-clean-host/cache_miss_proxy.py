#!/usr/bin/env python3
"""Disposable VM only: force one cache miss; pass all other HTTPS bytes unchanged.

Use the VM's temporary test CA. This does not alter cache signing keys or
package metadata. See PUBLIC-04.md for setup, cleanup, and evidence limits.
"""
import argparse
import http.server
import ipaddress
import os
import re
import socket
import ssl
import subprocess
import urllib.error
import urllib.request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream-address", required=True, type=ipaddress.ip_address)
    parser.add_argument("--missing-hash", required=True)
    parser.add_argument("--certificate", required=True)
    parser.add_argument("--private-key", required=True)
    args = parser.parse_args()
    if os.geteuid() != 0 or not subprocess.check_output(
        ['/usr/sbin/sysctl', '-n', 'hw.model'], text=True
    ).startswith('VirtualMac'):
        parser.error('requires root inside a disposable macOS VM')
    if not re.fullmatch(r"[0123456789abcdfghijklmnpqrsvwxyz]{32}", args.missing_hash):
        parser.error("missing hash must be a Nix store hash")
    missing = f"/{args.missing_hash}.narinfo"
    # The VM maps cache.nixos.org to this listener. Only upstream connections
    # bypass that mapping; urllib still checks the real upstream TLS hostname.
    resolve = socket.getaddrinfo
    socket.getaddrinfo = lambda host, port, *a, **kw: resolve(
        str(args.upstream_address) if host == "cache.nixos.org" else host, port, *a, **kw
    )

    class Proxy(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            if self.path == missing:
                print(f"INJECTED CACHE MISS {missing}", flush=True)
                self.send_error(404)
                return
            try:
                response = urllib.request.urlopen("https://cache.nixos.org" + self.path, timeout=60)
            except urllib.error.HTTPError as error:
                response = error
            except urllib.error.URLError:
                self.send_error(502)
                return
            with response:
                self.send_response(response.status)
                for name in ("Content-Type", "Content-Length"):
                    if response.headers.get(name):
                        self.send_header(name, response.headers[name])
                self.end_headers()
                while chunk := response.read(1024 * 1024):
                    self.wfile.write(chunk)

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 443), Proxy)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(args.certificate, args.private_key)
    server.socket = context.wrap_socket(server.socket, server_side=True)
    print("Test cache proxy ready on loopback HTTPS", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
