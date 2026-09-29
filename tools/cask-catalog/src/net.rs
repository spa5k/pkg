//! Guarded HTTPS fetching for public GitHub source material.
//!
//! Policy (contract): a real URL parser decides every URL's shape; the
//! client refuses inherited proxies and credentials; DNS is resolved
//! first and every address must be global (no localhost, no private
//! networks, no link-local rebind), the resolved address is pinned for
//! the connection, and every redirect hop is re-checked against the
//! destination allowlist. Body size and total time are bounded. The only
//! hosts this module may talk to are GitHub source hosts; payload
//! (vendor archive) fetching happens inside Nix fixed-output derivations
//! at install time, never here.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::sync::mpsc;
use std::time::Duration;
use url::Url;

/// Hosts the source fetcher may contact (HTTPS only).
pub const GITHUB_HOSTS: [&str; 4] = [
    "github.com",
    "api.github.com",
    "codeload.github.com",
    "raw.githubusercontent.com",
];

/// Default body size limit for source material (tap tarballs, API JSON).
pub const MAX_BODY_BYTES: usize = 96 * 1024 * 1024;
/// Default total time limit per request.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);
/// Connect timeout.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// Source fetch redirect decision (pure, unit-testable): ALL redirects
/// are refused. The source API and codeload endpoints are canonical and
/// direct; a hop could silently change the approved repository between
/// requests (transfer/rename), so none is ever followed. The vendored
/// safe-fetch FOD is a separate component that follows redirects with
/// its own verification; it is not affected by this rule.
fn redirect_refusal(url: &Url) -> String {
    format!(
        "source fetch redirect to {url} refused; canonical GitHub endpoints \
         never redirect"
    )
}

/// Check one URL against the transport policy.
///
/// Exactly HTTPS, no embedded credentials, no explicit non-443 port, no
/// fragment games, and a host inside the allowlist. This is a pure
/// function so every rule is unit-testable without network access.
pub fn check_url(url: &Url, allowed_hosts: &[&str]) -> Result<(), String> {
    if url.scheme() != "https" {
        return Err(format!("refusing non-https URL: {url}"));
    }
    let Some(host) = url.host_str() else {
        return Err(format!("URL has no host: {url}"));
    };
    if !allowed_hosts.contains(&host) {
        return Err(format!(
            "host {host} is outside the allowed source hosts {}",
            allowed_hosts.join(", ")
        ));
    }
    if url.port().is_some() {
        return Err(format!("refusing explicit port in URL: {url}"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(format!("refusing URL with embedded credentials: {url}"));
    }
    if url.fragment().is_some() {
        return Err(format!("refusing URL with a fragment: {url}"));
    }
    Ok(())
}

/// Whether one IPv4 address is public (not local, private, or special).
///
/// Explicit table so the rule never drifts with std stability changes:
/// loopback, private, link-local, shared (CGNAT), multicast, reserved,
/// broadcast, documentation, and benchmarking ranges are all refused.
fn ipv4_public(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_documentation()
        || ip.is_multicast()
        // 0.0.0.0/8 "this network" (the whole block, not just 0.0.0.0)
        || o[0] == 0
        // 100.64.0.0/10 shared address space (RFC 6598)
        || (o[0] == 100 && (o[1] & 0b1100_0000) == 64)
        // 192.0.0.0/24 IETF protocol assignments and 192.0.2.0/24 TEST-NET-1
        || (o[0] == 192 && o[1] == 0)
        // 198.18.0.0/15 benchmarking
        || (o[0] == 198 && (o[1] & 0xfe) == 18)
        // 240.0.0.0/4 reserved for future use
        || (o[0] & 0xf0) == 240)
}

/// Whether one IPv6 address is public.
///
/// Conservative allowlist: only global unicast 2000::/3 is fetchable,
/// minus the IETF special-purpose blocks inside it (2001::/23, which
/// covers Teredo, benchmarking, and the former OID space; 2001:db8::/32
/// documentation; 2002::/16 6to4 derived from arbitrary IPv4; 3fff::/20
/// documentation). IPv4-mapped addresses follow the IPv4 table. Every
/// other IPv6 shape (reserved, unique-local, link-local, multicast,
/// loopback, discard-only, translation) is refused.
fn ipv6_public(ip: Ipv6Addr) -> bool {
    let seg = ip.segments();
    if let Some(v4) = ip.to_ipv4_mapped() {
        return ipv4_public(v4);
    }
    // Only global unicast 2000::/3 is in fetch scope at all.
    if seg[0] & 0xe000 != 0x2000 {
        return false;
    }
    !(
        // 2001::/23 IETF special-purpose (Teredo, benchmarking, ...)
        // plus 2001:db8::/32 documentation.
        (seg[0] == 0x2001 && ((seg[1] & 0xfe00) == 0 || seg[1] == 0x0db8))
        // 2002::/16 6to4 (derived from arbitrary IPv4)
        || seg[0] == 0x2002
        // 3fff::/20 documentation (RFC 9637)
        || (seg[0] == 0x3fff && (seg[1] & 0xf000) == 0)
    )
}

/// Whether one IP address is public unicast, explicit on every platform.
#[must_use = "the result states whether the address is fetchable"]
pub fn ip_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ipv4_public(ip),
        IpAddr::V6(ip) => ipv6_public(ip),
    }
}

/// Whether every resolved address is a public address.
///
/// Loopback, private, link-local, multicast, and unspecified addresses
/// are refused: untrusted data must never aim the client at local or
/// private network services (SSRF).
#[must_use = "the result states whether every address is fetchable"]
pub fn all_global(addrs: &[SocketAddr]) -> bool {
    !addrs.is_empty() && addrs.iter().all(|a| ip_public(a.ip()))
}

/// Resolve a host to port 443, requiring every address to be public.
///
/// Resolution itself is bounded: std's resolver has no timeout of its
/// own, so the blocking lookup runs on a worker thread and is abandoned
/// after [`CONNECT_TIMEOUT`].
pub fn resolve_global(host: &str) -> Result<Vec<SocketAddr>, String> {
    let host = host.to_string();
    let host_for_error = host.clone();
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::Builder::new()
        .name("pkg-dns".into())
        .spawn(move || {
            let answer = (host.as_str(), 443u16)
                .to_socket_addrs()
                .map_err(|e| format!("cannot resolve {host}: {e}"))
                .map(std::iter::Iterator::collect::<Vec<SocketAddr>>);
            let _ = tx.send(answer);
        })
        .map_err(|e| format!("cannot start the DNS worker: {e}"))?;
    let answer = rx.recv_timeout(CONNECT_TIMEOUT).map_err(|_| {
        format!(
            "resolving {host_for_error} exceeded the {} s DNS limit",
            CONNECT_TIMEOUT.as_secs()
        )
    })?;
    // The worker thread owns no borrowed state; if it is still stuck in
    // the resolver it simply exits after its send once the OS answers.
    drop(worker.join());
    let addrs = answer?;
    if addrs.is_empty() {
        return Err(format!("host {host_for_error} resolved to no addresses"));
    }
    if !all_global(&addrs) {
        return Err(format!(
            "host {host_for_error} resolved to a non-public address; refusing private/local fetch"
        ));
    }
    Ok(addrs)
}

/// A guarded blocking HTTPS client for one fetch task.
pub struct SafeClient {
    inner: reqwest::blocking::Client,
    allowed: BTreeSet<&'static str>,
}

impl SafeClient {
    /// Build a client pinned to the given hosts.
    ///
    /// Proxies from the environment are disabled (`no_proxy`), rustls is
    /// the only TLS backend, DNS answers for every allowed host are
    /// resolved and validated now and pinned for the connection.
    /// Source redirects are refused to preserve the approved repository.
    pub fn new(allowed_hosts: &[&'static str]) -> Result<Self, String> {
        let mut builder = reqwest::blocking::Client::builder()
            .user_agent(concat!(
                "pkg-",
                env!("CARGO_PKG_NAME"),
                "/",
                env!("CARGO_PKG_VERSION")
            ))
            .no_proxy()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT);
        for host in allowed_hosts {
            let addrs = resolve_global(host)?;
            // Pin every validated address: the connection can only pick a
            // target this client already checked, never a re-resolved one.
            builder = builder.resolve_to_addrs(host, addrs.as_slice());
        }
        let allowed: BTreeSet<&'static str> = allowed_hosts.iter().copied().collect();
        let inner = builder
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                let reason = redirect_refusal(attempt.url());
                attempt.error(reason)
            }))
            .build()
            .map_err(|e| format!("cannot build the HTTPS client: {e}"))?;
        Ok(Self { inner, allowed })
    }

    /// GET one URL (re-checked against policy) and return its body,
    /// enforcing the body size limit while streaming.
    pub fn get(&self, url: &str, accept: Option<&str>) -> Result<Vec<u8>, String> {
        let parsed = Url::parse(url).map_err(|e| format!("malformed URL {url:?}: {e}"))?;
        let allowed: Vec<&str> = self.allowed.iter().copied().collect();
        check_url(&parsed, &allowed)?;
        let mut request = self.inner.get(parsed);
        if let Some(accept) = accept {
            request = request.header("Accept", accept);
        }
        let mut response = request.send().map_err(|e| format!("request failed: {e}"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("HTTP {status} fetching {url}"));
        }
        let mut body = Vec::new();
        let mut chunk = [0u8; 64 * 1024];
        use std::io::Read as _;
        loop {
            let read = response
                .read(&mut chunk)
                .map_err(|e| format!("body read failed: {e}"))?;
            if read == 0 {
                break;
            }
            if body.len() + read > MAX_BODY_BYTES {
                return Err(format!(
                    "body exceeds the {} byte limit: {url}",
                    MAX_BODY_BYTES
                ));
            }
            body.extend_from_slice(&chunk[..read]);
        }
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> Url {
        Url::parse(text).expect("parse")
    }

    #[test]
    fn policy_accepts_only_allowed_https_hosts() {
        assert!(check_url(&url("https://api.github.com/repos/a/b"), &GITHUB_HOSTS).is_ok());
        assert!(
            check_url(
                &url("https://codeload.github.com/a/b/tar.gz/deadbeef"),
                &GITHUB_HOSTS
            )
            .is_ok()
        );
        for bad in [
            "http://api.github.com/x",
            "https://evil.example/x",
            "https://api.github.com:8443/x",
            "https://user@api.github.com/x",
            "https://user:pw@api.github.com/x",
            "file:///etc/passwd",
        ] {
            let parsed = Url::parse(bad).expect("parse");
            assert!(
                check_url(&parsed, &GITHUB_HOSTS).is_err(),
                "{bad} must be refused"
            );
        }
    }

    #[test]
    fn policy_refuses_url_fragments() {
        for bad in [
            "https://api.github.com/repos/a/b#frag",
            "https://codeload.github.com/a/b/tar.gz/rev#x",
        ] {
            let parsed = Url::parse(bad).expect("parse");
            let err = check_url(&parsed, &GITHUB_HOSTS).expect_err("fragment refused");
            assert!(err.contains("fragment"), "{bad}: {err}");
        }
    }

    #[test]
    fn source_redirects_are_always_refused() {
        let reason = redirect_refusal(&url("https://github.com/other/repo"));
        assert!(reason.contains("refused"));
        assert!(reason.contains("https://github.com/other/repo"));
    }

    #[test]
    fn ipv6_literal_reserved_ranges_are_refused() {
        for bad in [
            "[fec0::1]:443",
            "[::1:2:3]:443",
            "[100::1:2:3]:443",
            "[2001:2::]:443",
            "[2001:10::1]:443",
            "[2001:db8::1]:443",
            "[2001:0:1:2:3:4:5:6]:443",
            "[2002:101:101::1]:443",
            "[3fff::1]:443",
            "[64:ff9b::1.2.3.4]:443",
            "[fe80::1]:443",
            "[fc00::1]:443",
            "[ff02::1]:443",
            "[::]:443",
        ] {
            let addr: SocketAddr = bad.parse().expect("addr");
            assert!(!all_global(&[addr]), "{bad} must not be global");
        }
        // 2000::/3 outside the excluded blocks stays fetchable.
        for good in [
            "[2001:4860::1]:443",
            "[2001:200::1]:443",
            "[3fff:1000::1]:443",
            "[3ffe::1]:443",
            "[2607:f8b0:400a::9]:443",
            "[2001:8000::1]:443",
            "[2620:0:9::1]:443",
        ] {
            let addr: SocketAddr = good.parse().expect("addr");
            assert!(all_global(&[addr]), "{good} must be global");
        }
        // Boundary cases for the /23 and /20 masks stay excluded.
        for bad in [
            "[2001:1ff::1]:443",
            "[2001:db8::1]:443",
            "[3fff:fff::1]:443",
        ] {
            let addr: SocketAddr = bad.parse().expect("addr");
            assert!(!all_global(&[addr]), "{bad} must not be global");
        }
        // Mapped IPv4 follows the IPv4 table: public mapped passes,
        // loopback mapped fails.
        let mapped_public: SocketAddr = "[::ffff:140.82.112.3]:443".parse().unwrap();
        assert!(all_global(&[mapped_public]));
        let mapped_loopback: SocketAddr = "[::ffff:127.0.0.1]:443".parse().unwrap();
        assert!(!all_global(&[mapped_loopback]));
    }

    #[test]
    fn mixed_dns_resolution_to_local_addresses_is_refused() {
        // `localhost` resolves (without external network) to a mixed
        // family set that always includes loopback; every family must
        // pass the public-address rule or the resolution is refused.
        match resolve_global("localhost") {
            Err(e) => assert!(e.contains("non-public"), "unexpected error: {e}"),
            Ok(addrs) => panic!("localhost must not resolve to fetchable addrs: {addrs:?}"),
        }
    }

    #[test]
    fn global_address_check_refuses_private_and_local() {
        let global: Vec<SocketAddr> = ["140.82.112.3:443", "[2607:f8b0:400a::9]:443"]
            .iter()
            .map(|s| s.parse().expect("addr"))
            .collect();
        assert!(all_global(&global));
        for bad in [
            "127.0.0.1:443",
            "10.1.2.3:443",
            "172.16.1.2:443",
            "192.168.1.1:443",
            "169.254.169.254:443",
            "100.64.0.1:443",
            "192.0.2.1:443",
            "198.18.0.1:443",
            "240.0.0.1:443",
            "224.0.0.1:443",
            "239.255.255.250:443",
            "255.255.255.255:443",
            "0.0.0.0:443",
            "0.1.2.3:443",
            "[100::1]:443",
            "[::1]:443",
            "[::]:443",
            "[fe80::1]:443",
            "[fc00::1]:443",
            "[fd12:3456:789a::1]:443",
            "[ff02::1]:443",
            "[ff00::42]:443",
            "[2001:db8::1]:443",
            "[2001:0000::1]:443",
            "[2002:101:101::1]:443",
            "[::ffff:127.0.0.1]:443",
        ] {
            let addr: SocketAddr = bad.parse().expect("addr");
            assert!(!all_global(&[addr]), "{bad} must not be global");
        }
        assert!(!all_global(&[]));
    }
}
