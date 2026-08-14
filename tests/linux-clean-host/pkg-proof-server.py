#!/usr/bin/env python3

import http.server
import ssl


server = http.server.ThreadingHTTPServer(
    ("127.0.0.1", 8443),
    lambda *args, **kwargs: http.server.SimpleHTTPRequestHandler(
        *args, directory="/srv/pkg-release", **kwargs
    ),
)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain("/etc/pkg-proof/server.crt", "/etc/pkg-proof/server.key")
server.socket = context.wrap_socket(server.socket, server_side=True)
server.serve_forever()
