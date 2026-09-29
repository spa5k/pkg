#!/usr/bin/env python3
"""Safe vendor payload fetcher (build-time, stdlib only).

Fetches one flat file over verified HTTPS with SSRF protections:
public-only destinations, DNS answers fully validated before connect,
pinned connect address while SNI/Host keep the original hostname,
bounded redirect chain and byte count, streamed SHA-256, and an atomic
rename that happens only after the hash matches.

Invoked by ./public-fetch.nix as:
    python safe-fetch.py --url <url> --output <out> --sha256 <hash>

No proxy support and no environment credential behavior is used:
http.client reads no proxy or credential variables and no auth,
cookie, or referer headers are ever sent. Requests use
Accept-Encoding: identity, so response bytes are payload bytes.
"""

import argparse
import contextlib
import hashlib
import http.client
import ipaddress
import os
import socket
import ssl
import sys
import tempfile
import threading
import time
import urllib.parse

MAX_REDIRECTS = 5
MAX_BYTES = 4 * 1024**3  # 4 GiB: generous vendor payload ceiling (documented).
CONNECT_TIMEOUT = 30.0
OVERALL_DEADLINE = 900.0
CHUNK = 256 * 1024
USER_AGENT = "cask-safe-fetch/1"
REDIRECT_STATUSES = frozenset({301, 302, 303, 307, 308})

# IPv6 policy is explicit and conservative because ipaddress .is_global
# semantics differ across Python versions. Only 2000::/3 (global
# unicast) is eligible, then these ranges are subtracted from it:
# 2001::/23 (Teredo and IETF protocol assignments), 2001:db8::/32
# (documentation), 2002::/16 (6to4), 3fff::/20 (benchmarking/doc-new).
# Everything else -- ::, ::1, ipv4-compatible ::/96, site-local fec0::/10,
# ULA fc00::/7, link-local fe80::/10, multicast ff00::/8 -- falls outside
# 2000::/3 and is rejected without relying on is_global.
# IPv4-mapped ::ffff:0:0/96 is handled separately by mapping it to the
# embedded IPv4 address and applying the IPv4 policy.
_V6_ALLOWED = ipaddress.ip_network("2000::/3")
_V6_BLOCKED = [
    ipaddress.ip_network("2001::/23"),
    ipaddress.ip_network("2001:db8::/32"),
    ipaddress.ip_network("2002::/16"),
    ipaddress.ip_network("3fff::/20"),
]

# Single-label names never go to a vendor; these suffixes are local.
_LOCAL_SUFFIXES = frozenset(
    {"localhost", "local", "internal", "lan", "home", "corp", "localdomain"}
)


class FetchError(Exception):
    """Raised for every rejected URL, destination, response, or payload."""


def is_public_ip(text):
    """True only for conservative global unicast addresses."""
    try:
        addr = ipaddress.ip_address(text)
    except ValueError:
        return False
    if addr.version == 4:
        return _is_public_v4(addr)
    mapped = addr.ipv4_mapped
    if mapped is not None:
        return _is_public_v4(mapped)
    if addr not in _V6_ALLOWED:
        return False
    if (
        addr.is_multicast
        or addr.is_reserved
        or addr.is_loopback
        or addr.is_link_local
        or addr.is_private
        or addr.is_unspecified
    ):
        return False
    return not any(addr in net for net in _V6_BLOCKED)


def _is_public_v4(addr):
    if not addr.is_global:
        return False
    return not (
        addr.is_multicast
        or addr.is_reserved
        or addr.is_loopback
        or addr.is_link_local
        or addr.is_private
        or addr.is_unspecified
    )


def _has_control_characters(text):
    return any(ord(c) < 0x20 or ord(c) == 0x7F for c in text)


def _has_ambiguous_characters(text):
    """Whitespace or backslash can change how other URL parsers split
    scheme, host, and path; reject before any parser sees them."""
    return any(c.isspace() for c in text) or "\\" in text


def _normalize_host(host):
    """Lowercase (urlsplit did), drop the FQDN dot, IDNA-encode or fail."""
    if "%" in host:
        raise FetchError("percent-encoded host or zone identifier is not allowed")
    host = host.rstrip(".")
    if not host:
        raise FetchError("empty host")
    if not host.isascii():
        try:
            host = host.encode("idna").decode("ascii")
        except (UnicodeError, ValueError):
            raise FetchError("hostname is not valid idna") from None
    return host


class ParsedURL:
    __slots__ = ("host", "path", "port")

    def __init__(self, host, port, path):
        self.host = host
        self.port = port
        self.path = path

    @property
    def host_header(self):
        if ":" in self.host:  # IPv6 literal needs brackets in Host
            return f"[{self.host}]"
        return self.host


def parse_url(url):
    """Parse one hop. Every fetch and every redirect hop calls this."""
    if not isinstance(url, str) or not url:
        raise FetchError("empty url")
    if _has_control_characters(url):
        raise FetchError("control characters in url")
    if _has_ambiguous_characters(url):
        raise FetchError("whitespace or backslash in url")
    parts = urllib.parse.urlsplit(url)
    if parts.scheme.lower() != "https":
        raise FetchError("only https is allowed")
    if parts.username is not None or parts.password is not None:
        raise FetchError("credentials in url are not allowed")
    host = parts.hostname
    if not host:
        raise FetchError("empty host")
    host = _normalize_host(host)
    try:
        port = parts.port
    except ValueError:
        raise FetchError("invalid port") from None
    if port is not None and port != 443:
        raise FetchError("only port 443 is allowed")
    port = 443
    try:
        ipaddress.ip_address(host)
        is_literal = True
    except ValueError:
        is_literal = False
    if is_literal:
        if not is_public_ip(host):
            raise FetchError("ip literal is not a public address")
    else:
        labels = host.split(".")
        if len(labels) < 2:
            raise FetchError("single-label host is not allowed")
        if labels[-1].lower() in _LOCAL_SUFFIXES:
            raise FetchError("local host name is not allowed")
    path = parts.path or "/"
    if parts.query:
        path = path + "?" + parts.query
    return ParsedURL(host, port, path)


def resolve_public(host, deadline):
    """Resolve host; refuse unless every answer is a public address.

    getaddrinfo runs in a daemon thread so a hung resolver cannot out-
    live the deadline: the fetch fails at the deadline and the abandoned
    thread never blocks process exit (unlike a ThreadPoolExecutor, whose
    __exit__ joins blocked workers)."""
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise FetchError("deadline exceeded before dns")
    result = {}

    def _resolve():
        try:
            result["answers"] = socket.getaddrinfo(host, 443, type=socket.SOCK_STREAM)
        except BaseException as exc:  # noqa: BLE001 - reported to caller
            result["error"] = exc

    worker = threading.Thread(target=_resolve, name="safe-fetch-dns", daemon=True)
    worker.start()
    worker.join(remaining)
    if worker.is_alive():
        raise FetchError("dns resolution timeout")
    if "error" in result:
        raise FetchError(f"dns resolution failed: {result['error']}")
    answers = result.get("answers") or []
    ips = []
    for answer in answers:
        sockaddr = answer[4]
        ip = sockaddr[0]
        if not is_public_ip(ip):
            raise FetchError(f"non-public dns answer for {host}")
        if ip not in ips:
            ips.append(ip)
    if not ips:
        raise FetchError(f"no usable dns answers for {host}")
    return ips


class PinnedHTTPSConnection(http.client.HTTPSConnection):
    """Connects to a pre-validated IP; TLS SNI and HTTP Host stay the
    original hostname, so no DNS rebinding can occur between check and
    connect. Uses no proxy and no environment credentials."""

    def __init__(self, host, resolved_ip, port, timeout, context):
        super().__init__(host, port=port, timeout=timeout, context=context)
        self.resolved_ip = resolved_ip

    def connect(self):
        sock = socket.create_connection((self.resolved_ip, self.port), self.timeout)
        try:
            self.sock = self._context.wrap_socket(sock, server_hostname=self.host)
        except BaseException:
            sock.close()  # never leak the raw socket on TLS failure
            raise


def _write_stream(resp, write, limit, deadline, conn, clock=None):
    """Stream response bytes to write() with size and time bounds.

    Reads use resp.read1, so the underlying buffered reader returns at
    most one socket read's worth of bytes instead of blocking until the
    full chunk size arrives; the overall deadline is re-checked between
    chunks, so a peer dripping one byte at a time cannot reset a
    per-read timeout into an unbounded total."""
    length_header = resp.getheader("Content-Length")
    declared = None
    if length_header is not None:
        try:
            declared = int(length_header)
        except ValueError:
            raise FetchError("invalid content-length") from None
        if declared < 0 or declared > limit:
            raise FetchError("content-length exceeds limit")
    content_type = (resp.getheader("Content-Type") or "").strip().lower()
    if content_type.startswith("multipart/"):
        raise FetchError("multipart response is not a flat payload")
    encoding = (resp.getheader("Content-Encoding") or "identity").strip().lower()
    if encoding != "identity":
        raise FetchError("content-encoding must be identity")
    total = 0
    while True:
        now = clock() if clock is not None else time.monotonic()
        remaining = deadline - now
        if remaining <= 0:
            raise FetchError("read deadline exceeded")
        sock = getattr(conn, "sock", None)
        if sock is not None:
            try:
                sock.settimeout(min(remaining, CONNECT_TIMEOUT))
            except OSError:
                pass
        chunk = resp.read1(CHUNK)
        if not chunk:
            if now > deadline:
                raise FetchError("read deadline exceeded at eof")
            if declared is not None and total != declared:
                raise FetchError(
                    f"content-length mismatch: declared {declared}, received {total}"
                )
            break
        if now > deadline:
            raise FetchError("read deadline exceeded")
        total += len(chunk)
        if total > limit:
            raise FetchError("payload exceeds byte limit")
        write(chunk)
    return total


def fetch(
    url,
    output,
    sha256,
    *,
    limit=MAX_BYTES,
    connect_timeout=CONNECT_TIMEOUT,
    overall_deadline=OVERALL_DEADLINE,
):
    """Fetch url to output (atomic rename on verified hash only)."""
    sha256 = sha256.strip().lower()
    if len(sha256) != 64 or any(c not in "0123456789abcdef" for c in sha256):
        raise FetchError("sha256 must be exactly 64 hex characters")
    output = os.fspath(output)
    deadline = time.monotonic() + overall_deadline
    context = ssl.create_default_context()
    tmp_path = None
    current = url
    try:
        for _hop in range(MAX_REDIRECTS + 1):
            target = parse_url(current)
            ips = resolve_public(target.host, deadline)
            conn = None
            last_error = None
            for ip in ips:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise FetchError("deadline exceeded")
                candidate = PinnedHTTPSConnection(
                    target.host,
                    ip,
                    target.port,
                    min(connect_timeout, remaining),
                    context,
                )
                try:
                    candidate.connect()
                except (OSError, http.client.HTTPException) as exc:
                    last_error = exc
                    try:
                        candidate.close()
                    except OSError:
                        pass
                    continue
                conn = candidate
                break
            if conn is None:
                raise FetchError(
                    f"connect failed for all resolved addresses: {last_error}"
                )
            with contextlib.closing(conn):
                conn.request(
                    "GET",
                    target.path,
                    headers={
                        "Host": target.host_header,
                        "User-Agent": USER_AGENT,
                        "Accept": "application/octet-stream, */*",
                        "Accept-Encoding": "identity",
                    },
                )
                resp = conn.getresponse()
                status = resp.status
                if status in REDIRECT_STATUSES:
                    location = resp.getheader("Location")
                    resp.close()
                    if not location or not location.strip():
                        raise FetchError("empty redirect location")
                    try:
                        current = urllib.parse.urljoin(current, location)
                    except ValueError:
                        raise FetchError("malformed redirect location") from None
                    # parse_url re-checks https, port, credentials, host
                    continue
                if not 200 <= status < 300:
                    raise FetchError(f"http status {status}")
                digest = hashlib.sha256()
                if tmp_path is not None:
                    os.unlink(tmp_path)
                    tmp_path = None
                fd, tmp_path = tempfile.mkstemp(
                    dir=os.path.dirname(os.path.abspath(output)),
                    prefix="." + os.path.basename(output) + ".",
                    suffix=".tmp",
                )
                with open(fd, "wb") as tmp:

                    def write(chunk, _tmp=tmp, _digest=digest):
                        _digest.update(chunk)
                        _tmp.write(chunk)

                    _write_stream(resp, write, limit, deadline, conn)
                    resp.close()
                if digest.hexdigest() != sha256:
                    raise FetchError("sha256 mismatch")
                os.replace(tmp_path, output)
                tmp_path = None
                return
        raise FetchError("too many redirects")
    finally:
        if tmp_path is not None:
            try:
                os.unlink(tmp_path)
            except OSError:
                pass


def main(argv=None):
    parser = argparse.ArgumentParser(
        description="Fetch one vendor payload over verified HTTPS."
    )
    parser.add_argument("--url", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--sha256", required=True)
    args = parser.parse_args(argv)
    try:
        fetch(args.url, args.output, args.sha256)
    except FetchError as exc:
        print(f"safe-fetch: {exc}", file=sys.stderr)
        return 1
    except (TimeoutError, OSError, http.client.HTTPException) as exc:
        print(f"safe-fetch: network error: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
