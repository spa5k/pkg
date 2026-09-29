"""Offline unit tests for nix/casks/lib/safe-fetch.py.

All network interaction is mocked with unittest.mock; no test touches
the internet. Run from the repository root:
    python3 -m unittest tools.cask-catalog.tests.test_safe_fetch
or directly:
    python3 tools/cask-catalog/tests/test_safe_fetch.py
"""

import hashlib
import io
import socket
import ssl
import subprocess
import sys
import threading
import time
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory
from typing import ClassVar
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "nix" / "casks" / "lib"))

import importlib.util

LIB_DIR = Path(__file__).resolve().parents[3] / "nix" / "casks" / "lib"

_spec = importlib.util.spec_from_file_location("safe_fetch", LIB_DIR / "safe-fetch.py")
safe_fetch = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(safe_fetch)

PUBLIC_V4 = "93.184.216.34"
PUBLIC_V6 = "2606:2800:220:1:248:1893:25c8:1946"

DATA = b"vendor payload bytes" * 100
DATA_SHA = hashlib.sha256(DATA).hexdigest()


class FakeResponse:
    def __init__(self, status, headers=None, body=b""):
        self.status = status
        self.headers = {k.lower(): v for k, v in (headers or {}).items()}
        self._body = body

    def getheader(self, name, default=None):
        return self.headers.get(name.lower(), default)

    def read(self, n=-1):
        if n < 0:
            n = len(self._body)
        chunk, self._body = self._body[:n], self._body[n:]
        return chunk

    def read1(self, n=-1):
        return self.read(n)

    def close(self):
        pass


class ScriptedConnection:
    """Stands in for safe_fetch.PinnedHTTPSConnection; records hops."""

    script: ClassVar[list] = []
    calls: ClassVar[list] = []
    closed: ClassVar[list] = []
    fail_ips: ClassVar[set] = set()

    def __init__(self, host, resolved_ip, port, timeout, context):
        self.host = host
        self.resolved_ip = resolved_ip
        self.port = port
        self.timeout = timeout
        self.context = context

    def connect(self):
        if self.resolved_ip in type(self).fail_ips:
            raise OSError(f"connect refused to {self.resolved_ip}")

    def request(self, method, path, headers=None):
        type(self).calls.append(
            {
                "host": self.host,
                "ip": self.resolved_ip,
                "port": self.port,
                "method": method,
                "path": path,
                "headers": headers or {},
            }
        )

    def getresponse(self):
        return type(self).script.pop(0)

    def close(self):
        type(self).closed.append(self)


class IpTestCase(unittest.TestCase):
    def assert_public(self, addr):
        self.assertTrue(safe_fetch.is_public_ip(addr), addr)

    def assert_rejected(self, addr):
        self.assertFalse(safe_fetch.is_public_ip(addr), addr)

    def test_ipv4_strict_cases(self):
        for addr in [
            "0.0.0.0",
            "0.1.2.3",  # 0/8
            "10.0.0.1",
            "100.64.0.1",
            "100.127.255.254",  # shared 100.64/10
            "127.0.0.1",
            "169.254.1.1",
            "172.16.0.1",
            "172.31.255.255",
            "192.0.2.1",  # documentation
            "192.168.1.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "239.255.255.250",  # multicast
            "240.0.0.1",
            "255.255.255.255",  # reserved
        ]:
            self.assert_rejected(str(addr))

    def test_ipv6_strict_cases(self):
        for addr in [
            "::",  # unspecified
            "::1",  # loopback
            "::ffff:10.0.0.1",
            "::ffff:127.0.0.1",  # mapped private
            "fe80::1",  # link-local
            "fc00::1",
            "fd12:3456::1",  # ULA fc00::/7
            "2001::1",  # Teredo
            "2001:db8::1",  # documentation
            "2002:0102:0304::1",  # 6to4
            "64:ff9b::192.0.2.1",  # NAT64 well-known
            "ff0e::1",
            "ff02::1",  # multicast (is_global may allow; must reject)
            "fec0::1",  # deprecated site-local
            "2001:0:0::1",  # inside 2001::/23
            "2001:1ff:ffff::1",  # top edge of 2001::/23
            "2001:2::1",  # benchmarking (subset of 2001::/23)
            "3fff:0fff::1",  # inside 3fff::/20
            "3fff:1::1",
            "4000::1",  # outside 2000::/3
            "64:ff9b:1::1",
            "64:ff9b::192.0.2.1",  # NAT64 well-known
        ]:
            self.assert_rejected(addr)

    def test_public_addresses_accepted(self):
        for addr in [
            PUBLIC_V4,
            "1.1.1.1",
            PUBLIC_V6,
            "2a00:1450:4001:81::200e",
            "2001:4860:4860::8888",  # above 2001::/23, must stay allowed
            "::ffff:93.184.216.34",
        ]:
            self.assert_public(addr)

    def test_garbage_rejected(self):
        for text in ["", "not-an-ip", "999.1.1.1", "fe80::1%eth0"]:
            self.assert_rejected(text)


class ParseUrlTestCase(unittest.TestCase):
    def test_valid_https(self):
        parsed = safe_fetch.parse_url("https://vendor.example.com/files/app.dmg?v=2")
        self.assertEqual(parsed.host, "vendor.example.com")
        self.assertEqual(parsed.port, 443)
        self.assertEqual(parsed.path, "/files/app.dmg?v=2")
        self.assertEqual(parsed.host_header, "vendor.example.com")

    def test_non_443_port_rejected(self):
        for url in [
            "https://vendor.example.com:8443/f",
            "https://vendor.example.com:0/f",
            "https://vendor.example.com:444/f",
        ]:
            with self.assertRaises(safe_fetch.FetchError, msg=url):
                safe_fetch.parse_url(url)

    def test_idna_normalized(self):
        parsed = safe_fetch.parse_url("https://b\u00fccher.example/f")
        self.assertEqual(parsed.host, "xn--bcher-kva.example")
        self.assertEqual(parsed.host_header, "xn--bcher-kva.example")

    def test_invalid_idna_rejected(self):
        bad = "https://" + "\u00e4" * 100 + ".example/f"
        with self.assertRaises(safe_fetch.FetchError):
            safe_fetch.parse_url(bad)

    def test_ipv6_literal_host_header_bracketed(self):
        parsed = safe_fetch.parse_url(f"https://[{PUBLIC_V6}]/f")
        self.assertEqual(parsed.host, PUBLIC_V6)
        self.assertEqual(parsed.host_header, f"[{PUBLIC_V6}]")

    def test_ambiguous_url_strings_rejected(self):
        for url in [
            " https://vendor.example.com/f",
            "https://vendor.example.com/f ",
            "https://vendor.example.com/a b",
            "https://vendor.example.com/f\ttab",
            "https://vendor.example.com\\@evil.example/f",
            "https://ev%69l.example/f",  # percent-encoded host
            "https://[fe80::1%25eth0]/f",  # zone identifier
            "https://[fe80::1%eth0]/f",
        ]:
            with self.assertRaises(safe_fetch.FetchError, msg=url):
                safe_fetch.parse_url(url)

    def test_rejected(self):
        for url in [
            "",
            "http://vendor.example.com/f",
            "ftp://vendor.example.com/f",
            "https://user:pass@vendor.example.com/f",
            "https:///f",  # empty host
            "https://vendor.example.com:99999/f",
            "https://vendor.example.com:0/f",
            "https://localhost/f",
            "https://host.local/f",
            "https://host.internal/f",
            "https://singlelabel/f",
            "https://10.0.0.1/f",
            "https://127.0.0.1/f",
            "https://[::1]/f",
            "https://[fe80::1]/f",
            "https://192.168.1.1:8443/f",
            "https://vendor.example.com/ok\x00.bin",
            "https://vendor.example.com/ok\r\nX: y",
            "relative/path",
        ]:
            with self.assertRaises(safe_fetch.FetchError, msg=url):
                safe_fetch.parse_url(url)

    def test_public_ip_literal_allowed(self):
        parsed = safe_fetch.parse_url(f"https://{PUBLIC_V4}/f")
        self.assertEqual(parsed.host, PUBLIC_V4)


class ResolvePublicTestCase(unittest.TestCase):
    def _answers(self, *ips):
        out = []
        for ip in ips:
            family = socket.AF_INET6 if ":" in ip else socket.AF_INET
            out.append((family, 1, 6, "", (ip, 443)))
        return out

    def test_all_public_answers(self):
        with mock.patch.object(
            safe_fetch.socket,
            "getaddrinfo",
            return_value=self._answers(PUBLIC_V4, PUBLIC_V6),
        ):
            ips = safe_fetch.resolve_public("vendor.example.com", time_deadline())
        self.assertEqual(ips, [PUBLIC_V4, PUBLIC_V6])

    def test_mixed_answer_refused(self):
        answers = self._answers(PUBLIC_V4, "10.0.0.5")
        with (
            mock.patch.object(safe_fetch.socket, "getaddrinfo", return_value=answers),
            self.assertRaises(safe_fetch.FetchError),
        ):
            safe_fetch.resolve_public("vendor.example.com", time_deadline())

    def test_private_answer_refused(self):
        with (
            mock.patch.object(
                safe_fetch.socket,
                "getaddrinfo",
                return_value=self._answers("192.168.0.9"),
            ),
            self.assertRaises(safe_fetch.FetchError),
        ):
            safe_fetch.resolve_public("vendor.example.com", time_deadline())

    def test_empty_answer_refused(self):
        with (
            mock.patch.object(safe_fetch.socket, "getaddrinfo", return_value=[]),
            self.assertRaises(safe_fetch.FetchError),
        ):
            safe_fetch.resolve_public("vendor.example.com", time_deadline())

    def test_expired_deadline(self):
        with self.assertRaises(safe_fetch.FetchError):
            safe_fetch.resolve_public("vendor.example.com", -1.0)


def time_deadline():
    return time.monotonic() + 10.0


class HungResolverTestCase(unittest.TestCase):
    """The resolver thread must be a daemon: a hung getaddrinfo must
    neither out-live the fetch deadline nor block process exit."""

    def test_hung_getaddrinfo_times_out_at_deadline(self):
        started = threading.Event()
        release = threading.Event()
        resolver_thread = {}

        def hang(*args, **kwargs):
            started.set()
            resolver_thread["t"] = threading.current_thread()
            release.wait(60)
            return []

        with mock.patch.object(safe_fetch.socket, "getaddrinfo", side_effect=hang):
            begin = time.monotonic()
            with self.assertRaises(safe_fetch.FetchError):
                safe_fetch.resolve_public("x.example", begin + 0.5)
            elapsed = time.monotonic() - begin
        self.assertLess(elapsed, 5.0, "deadline was not enforced")
        self.assertGreaterEqual(elapsed, 0.4, "returned before the deadline")
        thread = resolver_thread["t"]
        self.assertTrue(thread.daemon, "resolver thread must be a daemon")
        release.set()
        thread.join(5)

    def test_hung_resolver_does_not_block_process_exit(self):
        """Real subprocess: with a never-returning resolver, the process
        must still exit (bounded time) right after the deadline fires."""
        code = (
            "import socket, sys, time\n"
            "import importlib.util\n"
            f"spec = importlib.util.spec_from_file_location('safe_fetch', {str(LIB_DIR / 'safe-fetch.py')!r})\n"
            "mod = importlib.util.module_from_spec(spec)\n"
            "spec.loader.exec_module(mod)\n"
            "def hang(*a, **k):\n"
            "    time.sleep(10000)\n"
            "socket.getaddrinfo = hang\n"
            "try:\n"
            "    mod.resolve_public('x.example', time.monotonic() + 1)\n"
            "except mod.FetchError:\n"
            "    print('bounded', file=sys.stderr)\n"
            "    sys.exit(3)\n"
            "sys.exit(0)\n"
        )
        begin = time.monotonic()
        proc = subprocess.run(
            [sys.executable, "-c", code], capture_output=True, timeout=30, check=False
        )
        elapsed = time.monotonic() - begin
        self.assertEqual(proc.returncode, 3, proc.stderr)
        self.assertLess(elapsed, 20.0, "hung resolver blocked process exit")


class WriteStreamTestCase(unittest.TestCase):
    class SlowBody:
        """Yields one byte per read1 so several loop iterations run."""

        def __init__(self, data):
            self._data = data

        def read(self, n=-1):
            chunk, self._data = self._data[:1], self._data[1:]
            return chunk

        read1 = read

    def test_socket_timeout_reset_each_read(self):
        sock = mock.MagicMock(name="sock")
        conn = mock.MagicMock(name="conn")
        conn.sock = sock
        body = b"abcde"
        resp = self.SlowBody(body)
        resp.getheader = lambda name, default=None: None
        out = []
        deadline = time.monotonic() + 30.0
        total = safe_fetch._write_stream(resp, out.append, 100, deadline, conn)
        self.assertEqual(b"".join(out), body)
        self.assertEqual(total, len(body))
        timeouts = [c.args[0] for c in sock.settimeout.call_args_list]
        self.assertGreaterEqual(len(timeouts), len(body), "timeout not reset per read")
        self.assertTrue(all(0 < t <= safe_fetch.CONNECT_TIMEOUT for t in timeouts))
        self.assertEqual(
            timeouts, sorted(timeouts, reverse=True), "timeout must shrink"
        )

    def test_eof_after_deadline_rejected(self):
        sock = mock.MagicMock()
        conn = mock.MagicMock()
        conn.sock = sock
        t0 = 1000.0
        # clock values per loop iteration: before read, after read,
        # and at EOF the clock jumps past the deadline.
        clock = iter([t0, t0, t0 + 2000.0])
        resp = self.SlowBody(b"ab")
        resp.getheader = lambda name, default=None: None
        with self.assertRaises(safe_fetch.FetchError):
            safe_fetch._write_stream(
                resp,
                lambda c: None,
                100,
                t0 + 10.0,
                conn,
                clock=lambda: next(clock),
            )

    def test_slow_drip_peer_fails_at_overall_deadline(self):
        """A peer dripping one byte per socket read must not stretch the
        per-read timeout into an unbounded total: the deadline is
        checked between read1 chunks and the fetch fails."""
        sock = mock.MagicMock()
        conn = mock.MagicMock()
        conn.sock = sock
        t0 = 1000.0
        deadline = t0 + 30.0

        class FakeMonotonic:
            def __init__(self, start):
                self.now = start

            def __call__(self):
                return self.now

        clock = FakeMonotonic(t0)

        class DrippingBody:
            """Infinite drip: every read1 returns one byte, forever."""

            def read1(self, n=-1):
                clock.now += 1.0  # each drip eats one second of budget
                return b"x"

            def getheader(self, name, default=None):
                return None

        with self.assertRaises(safe_fetch.FetchError) as ctx:
            safe_fetch._write_stream(
                DrippingBody(), lambda c: None, 10**9, deadline, conn, clock=clock
            )
        self.assertIn("deadline", str(ctx.exception))
        # The failure must come from the deadline, not the byte limit.
        self.assertNotIn("byte limit", str(ctx.exception))

    def test_content_length_mismatch_at_eof_rejected(self):
        sock = mock.MagicMock()
        conn = mock.MagicMock()
        conn.sock = sock
        resp = FakeResponse(200, {"Content-Length": "10"}, b"abc")
        conn_dummy = conn
        deadline = time.monotonic() + 30.0
        with self.assertRaises(safe_fetch.FetchError) as ctx:
            safe_fetch._write_stream(resp, lambda c: None, 100, deadline, conn_dummy)
        self.assertIn("content-length mismatch", str(ctx.exception))
        self.assertIn("declared 10", str(ctx.exception))


class PinnedConnectionTestCase(unittest.TestCase):
    def test_connect_pins_ip_and_keeps_sni_hostname(self):
        conn = safe_fetch.PinnedHTTPSConnection(
            "vendor.example.com", PUBLIC_V4, 443, 5.0, mock.MagicMock()
        )
        raw = mock.MagicMock(name="raw-socket")
        wrapped = mock.MagicMock(name="wrapped-socket")
        conn._context.wrap_socket = mock.MagicMock(return_value=wrapped)
        with mock.patch.object(
            safe_fetch.socket, "create_connection", return_value=raw
        ) as cc:
            conn.connect()
        cc.assert_called_once_with((PUBLIC_V4, 443), 5.0)
        conn._context.wrap_socket.assert_called_once_with(
            raw, server_hostname="vendor.example.com"
        )
        self.assertIs(conn.sock, wrapped)

    def test_tls_wrap_failure_closes_raw_socket(self):
        conn = safe_fetch.PinnedHTTPSConnection(
            "vendor.example.com", PUBLIC_V4, 443, 5.0, mock.MagicMock()
        )
        raw = mock.MagicMock(name="raw-socket")
        conn._context.wrap_socket = mock.MagicMock(
            side_effect=ssl.SSLError("handshake failed")
        )
        with (
            mock.patch.object(safe_fetch.socket, "create_connection", return_value=raw),
            self.assertRaises(ssl.SSLError),
        ):
            conn.connect()
        raw.close.assert_called_once()
        self.assertIsNone(conn.sock)

    def test_host_header_uses_original_hostname(self):
        conn = safe_fetch.PinnedHTTPSConnection(
            "vendor.example.com", PUBLIC_V4, 443, 5.0, mock.MagicMock()
        )
        conn.sock = mock.MagicMock()
        conn.request("GET", "/file.bin", headers={"Host": "vendor.example.com"})
        sent = b"".join(c.args[0] for c in conn.sock.sendall.call_args_list if c.args)
        self.assertIn(b"Host: vendor.example.com", sent)
        self.assertNotIn(PUBLIC_V4.encode(), sent)


class FetchTestCase(unittest.TestCase):
    def setUp(self):
        self.tmp = TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.out = str(Path(self.tmp.name) / "payload.bin")
        patchers = [
            mock.patch.object(safe_fetch, "PinnedHTTPSConnection", ScriptedConnection),
            mock.patch.object(safe_fetch, "resolve_public", return_value=[PUBLIC_V4]),
            mock.patch.object(
                safe_fetch.ssl, "create_default_context", return_value=object()
            ),
        ]
        for patcher in patchers:
            patcher.start()
            self.addCleanup(patcher.stop)
        ScriptedConnection.script = []
        ScriptedConnection.calls = []
        ScriptedConnection.closed = []
        ScriptedConnection.fail_ips = set()

    def _fetch(self, sha=DATA_SHA, **kw):
        kw.setdefault("overall_deadline", 30.0)
        safe_fetch.fetch("https://vendor.example.com/file.bin", self.out, sha, **kw)

    def _leftover_tmps(self):
        return [p for p in Path(self.tmp.name).iterdir() if p.name != "payload.bin"]

    def test_success_writes_file_and_no_temp_left(self):
        ScriptedConnection.script = [
            FakeResponse(200, {"Content-Length": str(len(DATA))}, DATA)
        ]
        self._fetch()
        self.assertEqual(Path(self.out).read_bytes(), DATA)
        self.assertEqual(self._leftover_tmps(), [])
        call = ScriptedConnection.calls[0]
        self.assertEqual(call["host"], "vendor.example.com")
        self.assertEqual(call["ip"], PUBLIC_V4)  # connect pinned to resolved IP
        self.assertEqual(call["headers"]["Host"], "vendor.example.com")

    def test_hash_mismatch_cleans_up(self):
        ScriptedConnection.script = [FakeResponse(200, None, DATA)]
        with self.assertRaises(safe_fetch.FetchError):
            self._fetch(sha="0" * 64)
        self.assertFalse(Path(self.out).exists())
        self.assertEqual(self._leftover_tmps(), [])

    def test_bad_hash_format_rejected(self):
        for bad in ["", "abc", "z" * 64, DATA_SHA + "0"]:
            with self.assertRaises(safe_fetch.FetchError):
                self._fetch(sha=bad)
        self.assertEqual(ScriptedConnection.calls, [])

    def test_redirects_revalidated_each_hop(self):
        ScriptedConnection.script = [
            FakeResponse(302, {"Location": "https://cdn.example.net/file.bin"}),
            FakeResponse(200, None, DATA),
        ]
        safe_fetch.resolve_public = lambda host, deadline: (
            [PUBLIC_V6] if host == "cdn.example.net" else [PUBLIC_V4]
        )
        self._fetch()
        self.assertEqual(
            [c["host"] for c in ScriptedConnection.calls],
            ["vendor.example.com", "cdn.example.net"],
        )
        self.assertEqual(ScriptedConnection.calls[1]["ip"], PUBLIC_V6)

    def test_connect_failure_tries_all_addresses(self):
        ScriptedConnection.fail_ips = {PUBLIC_V4}
        ScriptedConnection.script = [FakeResponse(200, None, DATA)]
        safe_fetch.resolve_public = lambda host, deadline: [PUBLIC_V4, PUBLIC_V6]
        self._fetch()
        # First address failed at connect; request went out on the second.
        self.assertEqual([c["ip"] for c in ScriptedConnection.calls], [PUBLIC_V6])
        self.assertEqual(Path(self.out).read_bytes(), DATA)

    def test_connect_failure_on_all_addresses_rejected(self):
        ScriptedConnection.fail_ips = {PUBLIC_V4, PUBLIC_V6}
        ScriptedConnection.script = [FakeResponse(200, None, DATA)]
        safe_fetch.resolve_public = lambda host, deadline: [PUBLIC_V4, PUBLIC_V6]
        with self.assertRaises(safe_fetch.FetchError):
            self._fetch()
        self.assertEqual(ScriptedConnection.calls, [])

    def test_relative_redirect_resolved_against_current_url(self):
        ScriptedConnection.script = [
            FakeResponse(
                302,
                {"Location": "../../cdn/other.bin"},
            ),
            FakeResponse(200, None, DATA),
        ]
        self._fetch_url("https://vendor.example.com/a/b/file.bin")
        self.assertEqual(
            [(c["host"], c["path"]) for c in ScriptedConnection.calls],
            [
                ("vendor.example.com", "/a/b/file.bin"),
                ("vendor.example.com", "/cdn/other.bin"),
            ],
        )

    def test_network_path_relative_redirect_to_private_ip_rejected(self):
        ScriptedConnection.script = [
            FakeResponse(302, {"Location": "//10.0.0.7/file.bin"})
        ]
        with self.assertRaises(safe_fetch.FetchError):
            self._fetch()

    def test_relative_redirect_same_directory(self):
        ScriptedConnection.script = [
            FakeResponse(302, {"Location": "other.bin"}),
            FakeResponse(200, None, DATA),
        ]
        self._fetch()
        self.assertEqual(
            [(c["host"], c["path"]) for c in ScriptedConnection.calls],
            [("vendor.example.com", "/file.bin"), ("vendor.example.com", "/other.bin")],
        )

    def test_empty_or_whitespace_location_rejected(self):
        for location in ["", "   ", "\t"]:
            ScriptedConnection.script = [FakeResponse(302, {"Location": location})]
            with self.assertRaises(safe_fetch.FetchError):
                self._fetch()

    def test_absolute_malformed_location_rejected(self):
        ScriptedConnection.script = [
            FakeResponse(302, {"Location": "https://[::1:bad/x"})
        ]
        with self.assertRaises(safe_fetch.FetchError):
            self._fetch()

    def test_redirect_downgrade_to_http_rejected(self):
        ScriptedConnection.script = [
            FakeResponse(302, {"Location": "http://cdn.example.net/file.bin"})
        ]
        with self.assertRaises(safe_fetch.FetchError):
            self._fetch()

    def test_redirect_to_private_literal_rejected(self):
        ScriptedConnection.script = [
            FakeResponse(302, {"Location": "https://10.0.0.7/file.bin"})
        ]
        with self.assertRaises(safe_fetch.FetchError):
            self._fetch()

    def test_redirect_with_credentials_rejected(self):
        ScriptedConnection.script = [
            FakeResponse(302, {"Location": "https://u:p@cdn.example.net/file.bin"})
        ]
        with self.assertRaises(safe_fetch.FetchError):
            self._fetch()

    def _fetch_url(self, url, sha=DATA_SHA, **kw):
        kw.setdefault("overall_deadline", 30.0)
        safe_fetch.fetch(url, self.out, sha, **kw)

    def test_redirect_without_location_rejected(self):
        ScriptedConnection.script = [FakeResponse(301)]
        with self.assertRaises(safe_fetch.FetchError):
            self._fetch()

    def test_too_many_redirects_rejected(self):
        ScriptedConnection.script = [
            FakeResponse(302, {"Location": f"https://hop{i}.example.com/f"})
            for i in range(7)
        ]
        with self.assertRaises(safe_fetch.FetchError):
            self._fetch()

    def test_non_2xx_rejected(self):
        for status in [404, 500, 403, 204]:
            ScriptedConnection.script = [FakeResponse(status)]
            with self.assertRaises(safe_fetch.FetchError):
                self._fetch()

    def test_content_length_over_limit_rejected_before_read(self):
        ScriptedConnection.script = [
            FakeResponse(200, {"Content-Length": str(1024**3)}, DATA)
        ]
        with self.assertRaises(safe_fetch.FetchError):
            self._fetch(limit=1024)
        self.assertFalse(Path(self.out).exists())

    def test_actual_bytes_over_limit_rejected(self):
        ScriptedConnection.script = [FakeResponse(200, None, DATA)]
        with self.assertRaises(safe_fetch.FetchError):
            self._fetch(limit=10)
        self.assertEqual(self._leftover_tmps(), [])

    def test_multipart_rejected(self):
        ScriptedConnection.script = [
            FakeResponse(200, {"Content-Type": "multipart/form-data"}, DATA)
        ]
        with self.assertRaises(safe_fetch.FetchError):
            self._fetch()

    def test_non_identity_encoding_rejected(self):
        ScriptedConnection.script = [
            FakeResponse(200, {"Content-Encoding": "gzip"}, DATA)
        ]
        with self.assertRaises(safe_fetch.FetchError):
            self._fetch()

    def test_no_auth_or_referer_headers_sent(self):
        ScriptedConnection.script = [FakeResponse(200, None, DATA)]
        self._fetch()
        headers = ScriptedConnection.calls[0]["headers"]
        for banned in ["Authorization", "Cookie", "Referer"]:
            self.assertNotIn(banned, headers)
        self.assertEqual(headers["Accept-Encoding"], "identity")

    def test_read_timeout_cleans_up(self):
        class TimeoutResponse(FakeResponse):
            def read1(self, n=-1):
                raise TimeoutError("read timed out")

        ScriptedConnection.script = [TimeoutResponse(200)]
        with self.assertRaises((safe_fetch.FetchError, socket.timeout)):
            self._fetch()
        self.assertFalse(Path(self.out).exists())
        self.assertEqual(self._leftover_tmps(), [])

    def test_overall_deadline_exceeded_cleans_up(self):
        real_monotonic = safe_fetch.time.monotonic
        ticks = iter([real_monotonic()])

        def fast_clock():
            try:
                return next(ticks)
            except StopIteration:
                return real_monotonic() + 2000.0

        ScriptedConnection.script = [FakeResponse(200, None, DATA)]
        with (
            mock.patch.object(safe_fetch.time, "monotonic", side_effect=fast_clock),
            self.assertRaises(safe_fetch.FetchError),
        ):
            self._fetch(overall_deadline=30.0)
        self.assertEqual(self._leftover_tmps(), [])


def _http_response(status, reason, headers, body):
    lines = [f"HTTP/1.1 {status} {reason}".encode()]
    for k, v in (headers or {}).items():
        lines.append(f"{k}: {v}".encode())
    return b"\r\n".join(lines) + b"\r\n\r\n" + body


class FakeTLSSocket:
    """Transport stub for the real PinnedHTTPSConnection: accepts sent
    bytes, serves scripted HTTP response bytes, records close()."""

    def __init__(self, response):
        self._response = response
        self.sent = bytearray()
        self.closed = False
        self.timeouts = []
        self.read_error = None

    def makefile(self, mode, *args, **kwargs):
        if "r" not in mode:
            raise ValueError("only rb mode supported")
        buf = io.BytesIO(self._response)

        class _Reader(io.BufferedReader):
            def read1(inner, n=-1):
                if self.read_error is not None:
                    raise self.read_error
                return super().read1(n)

        return _Reader(buf)

    def sendall(self, data):
        self.sent += data

    def settimeout(self, value):
        self.timeouts.append(value)

    def close(self):
        self.closed = True


class RealInterfaceTestCase(unittest.TestCase):
    """Offline fetch through the REAL PinnedHTTPSConnection class; only
    the socket transport (create_connection / TLS wrap) is stubbed. The
    production class must never gain __enter__/__exit__ for tests."""

    def setUp(self):
        self.tmp = TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.out = str(Path(self.tmp.name) / "payload.bin")
        self.sockets = []
        self.context = mock.MagicMock(name="ssl-context")
        patchers = [
            mock.patch.object(safe_fetch, "resolve_public", return_value=[PUBLIC_V4]),
            mock.patch.object(
                safe_fetch.ssl,
                "create_default_context",
                return_value=self.context,
            ),
        ]
        for patcher in patchers:
            patcher.start()
            self.addCleanup(patcher.stop)

    def _serve_connected(self, *response_bodies, read_error=None):
        """Stub create_connection to hand out raw sockets; wrap_socket
        returns the scripted FakeTLSSocket, as a real TLS wrap would."""
        ready = []
        for body in response_bodies:
            sock = FakeTLSSocket(body)
            sock.read_error = read_error
            ready.append(sock)
        it = iter(ready)
        self.context.wrap_socket = lambda raw, server_hostname: next(it)
        patcher = mock.patch.object(
            safe_fetch.socket,
            "create_connection",
            side_effect=lambda addr, timeout: mock.MagicMock(name="raw"),
        )
        patcher.start()
        self.addCleanup(patcher.stop)
        self.sockets = list(ready)
        return self.sockets

    def _fetch(self, sha=DATA_SHA, **kw):
        kw.setdefault("overall_deadline", 30.0)
        safe_fetch.fetch("https://vendor.example.com/file.bin", self.out, sha, **kw)

    def test_no_context_manager_protocol_on_production_class(self):
        self.assertFalse(hasattr(safe_fetch.PinnedHTTPSConnection, "__enter__"))
        self.assertFalse(hasattr(safe_fetch.PinnedHTTPSConnection, "__exit__"))

    def test_real_connection_success_closes(self):
        self._serve_connected(
            _http_response(200, "OK", {"Content-Length": str(len(DATA))}, DATA)
        )
        self._fetch()
        self.assertEqual(Path(self.out).read_bytes(), DATA)
        sock = self.sockets[0]
        self.assertTrue(sock.closed, "connection must close after success")
        sent = bytes(sock.sent)
        self.assertIn(b"GET /file.bin", sent)
        self.assertIn(b"Host: vendor.example.com", sent)
        self.assertIn(b"Accept-Encoding: identity", sent)

    def test_real_connection_redirects_close_every_hop(self):
        self._serve_connected(
            _http_response(
                302, "Found", {"Location": "https://cdn.example.net/file.bin"}, b""
            ),
            _http_response(200, "OK", {"Content-Length": str(len(DATA))}, DATA),
        )
        safe_fetch.resolve_public = lambda host, deadline: [PUBLIC_V4]
        self._fetch()
        self.assertEqual(len(self.sockets), 2)
        for i, sock in enumerate(self.sockets):
            self.assertTrue(sock.closed, f"hop {i} connection must close")
        self.assertIn(b"GET /file.bin", bytes(self.sockets[1].sent))

    def test_real_connection_rejected_response_closes(self):
        self._serve_connected(_http_response(404, "Not Found", {}, b""))
        with self.assertRaises(safe_fetch.FetchError):
            self._fetch()
        self.assertTrue(self.sockets[0].closed)

    def test_real_connection_read_timeout_closes(self):
        self._serve_connected(
            _http_response(200, "OK", {"Content-Length": str(len(DATA))}, DATA),
            read_error=TimeoutError("read timed out"),
        )
        with self.assertRaises((safe_fetch.FetchError, socket.timeout)):
            self._fetch()
        self.assertTrue(self.sockets[0].closed)
        self.assertFalse(Path(self.out).exists())

    def test_real_connection_hash_failure_closes(self):
        self._serve_connected(
            _http_response(200, "OK", {"Content-Length": str(len(DATA))}, DATA)
        )
        with self.assertRaises(safe_fetch.FetchError):
            self._fetch(sha="0" * 64)
        self.assertTrue(self.sockets[0].closed)
        self.assertFalse(Path(self.out).exists())
        leftovers = [
            p for p in Path(self.tmp.name).iterdir() if p.name != "payload.bin"
        ]
        self.assertEqual(leftovers, [])


class PolicyTestCase(unittest.TestCase):
    def test_no_allow_local_escape_hatch(self):
        for name in vars(safe_fetch):
            self.assertNotIn("allow_local", name.lower())


if __name__ == "__main__":
    unittest.main()
