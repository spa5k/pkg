#!/usr/bin/env python3

import http.server
import os
import ssl


root = os.environ["PKG_PROOF_ROOT"]
certificate = os.environ["PKG_PROOF_CERTIFICATE"]
private_key = os.environ["PKG_PROOF_PRIVATE_KEY"]
server = http.server.ThreadingHTTPServer(
    ("127.0.0.1", 8443),
    lambda *args, **kwargs: http.server.SimpleHTTPRequestHandler(
        *args, directory=root, **kwargs
    ),
)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain(certificate, private_key)
server.socket = context.wrap_socket(server.socket, server_side=True)
server.serve_forever()
