//! Guarded HTTPS fetching for public GitHub source material.
//!
//! Parse URLs and allow only GitHub source hosts. Disable inherited proxies
//! and credentials. Resolve DNS before connecting, require public addresses,
//! and pin them for the connection. Refuse redirects and limit body size and time.
//! Nix fixed-output derivations fetch vendor archives at install time.

use std::io::Read as _;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::sync::mpsc;
use std::time::Duration;
use url::Url;

/// Hosts the source fetcher may contact (HTTPS only).
///
/// `SafeClient::new` resolves every host, so list only hosts that a tap
/// import contacts.
pub const GITHUB_HOSTS: [&str; 2] = ["api.github.com", "codeload.github.com"];

/// Default body size limit for source material (tap tarballs, API JSON).
pub const MAX_BODY_BYTES: usize = 96 * 1024 * 1024;
/// Default total time limit per request.
pub const REQUEST_TIMEOUT: Duration = Duration::from_mins(3);
/// Connect timeout.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// Refuse all source-fetch redirects to preserve the approved repository.
/// Vendor downloads use the separate safe-fetch derivation, which verifies redirects.
fn redirect_refusal(url: &Url) -> String {
    format!(
        "source fetch redirect to {url} refused; canonical GitHub endpoints \
         never redirect"
    )
}

/// Check one URL against the transport policy.
///
/// Require HTTPS, an allowed host, port 443 if explicit, and no credentials or fragment.
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
/// Conservative allowlist: only global unicast `2000::/3` is fetchable,
/// minus the IETF special-purpose blocks inside it (`2001::/23`, which
/// covers Teredo, benchmarking, and the former OID space; `2001:db8::/32`
/// documentation; `2002::/16` 6to4 derived from arbitrary IPv4; `3fff::/20`
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
    allowed: &'static [&'static str],
}

impl SafeClient {
    /// Build a client pinned to the given hosts.
    ///
    /// Proxies from the environment are disabled (`no_proxy`), rustls is
    /// the only TLS backend, DNS answers for every allowed host are
    /// resolved and validated now and pinned for the connection.
    /// Source redirects are refused to preserve the approved repository.
    pub fn new(allowed: &'static [&'static str]) -> Result<Self, String> {
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
        for host in allowed {
            let addrs = resolve_global(host)?;
            // Pin every validated address: the connection can only pick a
            // target this client already checked, never a re-resolved one.
            builder = builder.resolve_to_addrs(host, addrs.as_slice());
        }
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
        self.get_optional(url, accept)?
            .ok_or_else(|| format!("HTTP 404 Not Found fetching {url}"))
    }

    /// GET one URL like [`Self::get`], but return `None` for 404 Not Found.
    pub fn get_optional(&self, url: &str, accept: Option<&str>) -> Result<Option<Vec<u8>>, String> {
        let parsed = Url::parse(url).map_err(|e| format!("malformed URL {url:?}: {e}"))?;
        check_url(&parsed, self.allowed)?;
        let mut request = self.inner.get(parsed);
        if let Some(accept) = accept {
            request = request.header("Accept", accept);
        }
        let mut response = request
            .send()
            .map_err(|e| format!("request failed: {}", causes(&e)))?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs());
            return Err(status_error(status, response.headers(), url, now));
        }
        let mut body = Vec::new();
        let mut chunk = vec![0u8; 64 * 1024].into_boxed_slice();
        loop {
            let read = response
                .read(&mut chunk)
                .map_err(|e| format!("body read failed: {}", causes(&e)))?;
            if read == 0 {
                break;
            }
            if body.len() + read > MAX_BODY_BYTES {
                return Err(format!(
                    "body exceeds the {MAX_BODY_BYTES} byte limit: {url}"
                ));
            }
            body.extend_from_slice(&chunk[..read]);
        }
        Ok(Some(body))
    }
}

/// `error` and each cause below it, for example the DNS or TLS failure
/// under a reqwest "error sending request".
fn causes(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let cause_text = cause.to_string();
        if !text.contains(&cause_text) {
            text.push_str(": ");
            text.push_str(&cause_text);
        }
        source = cause.source();
    }
    text
}

/// The error for an unsuccessful `status`. GitHub reports a used-up rate
/// limit as 403 or 429 with `x-ratelimit-remaining: 0`, and `now` (Unix
/// seconds) gives the wait until `x-ratelimit-reset`.
fn status_error(
    status: reqwest::StatusCode,
    headers: &reqwest::header::HeaderMap,
    url: &str,
    now: u64,
) -> String {
    let header = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
    let limited = matches!(status.as_u16(), 403 | 429)
        && (header("x-ratelimit-remaining") == Some("0") || header("retry-after").is_some());
    if !limited {
        return format!("HTTP {status} fetching {url}");
    }
    let wait = header("retry-after")
        .and_then(|value| value.parse::<u64>().ok())
        .or_else(|| {
            header("x-ratelimit-reset")
                .and_then(|value| value.parse::<u64>().ok())
                .map(|reset| reset.saturating_sub(now))
        })
        .map(|seconds| seconds.div_ceil(60).max(1))
        .map_or_else(
            || String::from("later"),
            |minutes| format!("in {minutes} minute{}", if minutes == 1 { "" } else { "s" }),
        );
    format!(
        "GitHub refused {url} because of its rate limit for requests without sign-in \
         (HTTP {status}). Try again {wait}."
    )
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
    fn errors_name_the_rate_limit() {
        use reqwest::StatusCode;
        use reqwest::header::{HeaderMap, HeaderValue};
        let url = "https://api.github.com/repos/a/b";
        let mut headers = HeaderMap::new();
        assert_eq!(
            status_error(StatusCode::FORBIDDEN, &headers, url, 0),
            format!("HTTP 403 Forbidden fetching {url}")
        );
        headers.insert("x-ratelimit-remaining", HeaderValue::from_static("0"));
        headers.insert("x-ratelimit-reset", HeaderValue::from_static("1000"));
        let error = status_error(StatusCode::FORBIDDEN, &headers, url, 100);
        assert!(error.contains("rate limit"), "{error}");
        assert!(error.ends_with("Try again in 15 minutes."), "{error}");
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_static("30"));
        let error = status_error(StatusCode::TOO_MANY_REQUESTS, &headers, url, 0);
        assert!(error.ends_with("Try again in 1 minute."), "{error}");
    }

    /// A reqwest-like error whose text omits its cause.
    #[derive(Debug)]
    struct Send(std::io::Error);

    impl std::fmt::Display for Send {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("error sending request")
        }
    }

    impl std::error::Error for Send {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }

    #[test]
    fn request_errors_include_their_causes() {
        let error = Send(std::io::Error::other("dns error"));
        assert_eq!(causes(&error), "error sending request: dns error");
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
