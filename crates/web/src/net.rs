//! Fetching pages from the open web – and only from there.
//!
//! A search hit may point anywhere, also to `127.0.0.1` (Ancilo's own API) or
//! a device at home. So pages are fetched by a client that connects only to
//! public addresses: its resolver checks every address a name resolves to and
//! the connection uses exactly those (no second lookup a rebinding name could
//! change); addresses written into a URL and every redirect are checked too;
//! no proxy, no cookies, no referer. Bodies are read in pieces and cut off at
//! a limit (decision `2026-10-02-websuche`).

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::{Client, Url, redirect};

/// At most this much of a page is read.
pub const MAX_PAGE_BYTES: usize = 2 << 20;
/// At most this many redirects.
pub const MAX_REDIRECTS: usize = 3;
/// One page may take this long.
pub const PAGE_TIMEOUT: Duration = Duration::from_secs(8);

/// Whether an IPv4 address belongs to the public internet.
fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        // documentation
        || (a == 192 && b == 0 && c == 2)
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || a == 0
        // shared address space (carrier-grade NAT)
        || (a == 100 && (64..128).contains(&b))
        // IETF protocol assignments
        || (a == 192 && b == 0 && c == 0)
        // benchmarking
        || (a == 198 && (b == 18 || b == 19))
        // reserved, including 255.255.255.255
        || a >= 240)
}

/// Whether an address belongs to the public internet. IPv6 addresses that
/// carry an IPv4 address (mapped, compatible, NAT64, 6to4) count as that one.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => public_v4(v4),
        IpAddr::V6(v6) => public_v6(v6),
    }
}

fn public_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return public_v4(v4);
    }
    let s = ip.segments();
    // IPv4-compatible (::a.b.c.d, deprecated) and NAT64 (64:ff9b::/96).
    if s[..6] == [0; 6] || s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        let v4 = Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
        return !ip.is_unspecified() && !ip.is_loopback() && public_v4(v4);
    }
    // 6to4 (2002::/16): the IPv4 address follows the prefix.
    if s[0] == 0x2002 {
        let v4 = Ipv4Addr::new((s[1] >> 8) as u8, s[1] as u8, (s[2] >> 8) as u8, s[2] as u8);
        return public_v4(v4);
    }
    // Only global unicast (2000::/3) – without Teredo (2001::/32), the
    // documentation prefix (2001:db8::/32) and ORCHID/benchmarking (2001:2::/48, 2001:10::/28).
    (s[0] & 0xe000) == 0x2000
        && !(s[0] == 0x2001 && s[1] == 0)
        && !(s[0] == 0x2001 && s[1] == 0x0db8)
        && !(s[0] == 0x2001 && s[1] == 0x0002 && s[2] == 0)
        && !(s[0] == 0x2001 && (0x0010..0x0020).contains(&s[1]))
}

/// Resolves names to public addresses only. `hosts`: names the local
/// configuration maps to an address on purpose (tests) – the only way past the check.
#[derive(Debug, Clone, Default)]
pub struct PublicResolver {
    hosts: Arc<HashMap<String, IpAddr>>,
}

impl PublicResolver {
    pub fn new(hosts: HashMap<String, IpAddr>) -> Self {
        Self {
            hosts: Arc::new(hosts),
        }
    }

    fn mapped(&self, host: &str) -> Option<IpAddr> {
        self.hosts.get(&host.to_ascii_lowercase()).copied()
    }
}

impl Resolve for PublicResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        let mapped = self.mapped(&host);
        Box::pin(async move {
            if let Some(ip) = mapped {
                let addrs: Addrs = Box::new(std::iter::once(SocketAddr::new(ip, 0)));
                return Ok(addrs);
            }
            let found: Vec<SocketAddr> =
                tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            if found.is_empty() {
                return Err(format!("{host} has no address").into());
            }
            // All of them public, or none is used.
            if let Some(bad) = found.iter().find(|a| !is_public(a.ip())) {
                return Err(format!("{host} points to a non-public address ({})", bad.ip()).into());
            }
            let addrs: Addrs = Box::new(found.into_iter());
            Ok(addrs)
        })
    }
}

/// Why a URL is not fetched (None: it may be).
pub fn refuse(url: &Url, resolver: &PublicResolver) -> Option<String> {
    if !matches!(url.scheme(), "http" | "https") {
        return Some(format!("only web pages are fetched, not {}", url.scheme()));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Some("addresses with a user name are not fetched".into());
    }
    match url.host() {
        None => Some("the address has no host".into()),
        Some(url::Host::Domain(d)) if resolver.mapped(d).is_some() => None,
        Some(url::Host::Domain(d))
            if d.eq_ignore_ascii_case("localhost") || d.ends_with(".localhost") =>
        {
            Some("local addresses are not fetched".into())
        }
        Some(url::Host::Domain(_)) => None,
        Some(url::Host::Ipv4(ip)) if !public_v4(ip) => {
            Some(format!("{ip} is not a public address"))
        }
        Some(url::Host::Ipv6(ip)) if !public_v6(ip) => {
            Some(format!("{ip} is not a public address"))
        }
        Some(_) => None,
    }
}

/// The client for pages: public addresses only, no proxy, no cookies, no referer.
pub fn page_client(resolver: PublicResolver, user_agent: &str) -> Client {
    let check = resolver.clone();
    Client::builder()
        .user_agent(user_agent)
        .dns_resolver(Arc::new(resolver))
        .no_proxy()
        .referer(false)
        .connect_timeout(Duration::from_secs(4))
        .timeout(PAGE_TIMEOUT)
        .redirect(redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                return attempt.error("too many redirects");
            }
            match refuse(attempt.url(), &check) {
                Some(why) => attempt.error(why),
                None => attempt.follow(),
            }
        }))
        .build()
        .expect("page client")
}

/// A fetched page: its final address and (up to the limit) its text.
#[derive(Debug, Clone)]
pub struct Fetched {
    pub url: Url,
    pub html: bool,
    pub body: String,
}

/// Fetches a page – HTML or plain text, at most [`MAX_PAGE_BYTES`].
pub async fn fetch(
    client: &ancilo_net::Net,
    resolver: &PublicResolver,
    url: &str,
    subject: &str,
) -> Result<Fetched, String> {
    let url = Url::parse(url).map_err(|e| format!("not an address: {e}"))?;
    if let Some(why) = refuse(&url, resolver) {
        return Err(why);
    }
    let mut res = client
        .send(
            client
                .get(url)
                .header(reqwest::header::ACCEPT, "text/html,text/plain;q=0.9"),
            ancilo_net::Note::new(ancilo_net::Purpose::WebPage, subject, ancilo_net::By::You),
        )
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(format!("HTTP {}", res.status().as_u16()));
    }
    let header = |name| {
        res.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase()
    };
    let kind = header(reqwest::header::CONTENT_TYPE);
    let html = kind.contains("text/html") || kind.contains("application/xhtml");
    if !html && !kind.contains("text/plain") {
        return Err(format!("not a web page ({kind})"));
    }
    // Only what can be read as it comes (no compression is asked for).
    let encoding = header(reqwest::header::CONTENT_ENCODING);
    if !encoding.is_empty() && encoding != "identity" {
        return Err(format!("compressed page ({encoding})"));
    }
    let final_url = res.url().clone();
    let mut bytes = Vec::new();
    while let Some(chunk) = res.chunk().await.map_err(|e| e.to_string())? {
        let room = MAX_PAGE_BYTES - bytes.len();
        bytes.extend_from_slice(&chunk[..chunk.len().min(room)]);
        if bytes.len() >= MAX_PAGE_BYTES {
            break;
        }
    }
    Ok(Fetched {
        url: final_url,
        html,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn only_public_addresses_count() {
        for public in [
            "93.184.215.14",
            "8.8.8.8",
            "2606:4700::1111",
            "2a00:1450:4001::200e",
            "::ffff:8.8.8.8",
        ] {
            assert!(is_public(ip(public)), "{public}");
        }
        for private in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.10",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
            "192.0.2.1",
            "198.18.0.1",
            "240.0.0.1",
            "::1",
            "::",
            "fe80::1",
            "fc00::1",
            "fd12:3456::1",
            "ff02::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
            "::ffff:192.168.0.1",
            "::127.0.0.1",
            "64:ff9b::7f00:1",
            "2002:7f00:1::1",
            "2002:c0a8:101::1",
            "2001::1",
            "fec0::1",
        ] {
            assert!(!is_public(ip(private)), "{private}");
        }
    }

    #[test]
    fn addresses_are_checked_before_anything_is_sent() {
        let r = PublicResolver::new(HashMap::from([("pages.test".to_string(), ip("127.0.0.1"))]));
        let refused = |u: &str| refuse(&Url::parse(u).unwrap(), &r);
        assert!(refused("https://example.org/page").is_none());
        assert!(
            refused("http://pages.test:8080/a").is_none(),
            "mapped on purpose"
        );
        for bad in [
            "http://127.0.0.1:7425/api/v1/ops/daemon_shutdown",
            "http://[::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://localhost/",
            "http://app.localhost/",
            "http://192.168.1.1/admin",
            "http://169.254.169.254/latest/meta-data",
            "file:///etc/passwd",
            "ftp://example.org/",
            "http://user:pw@example.org/",
        ] {
            assert!(refused(bad).is_some(), "{bad}");
        }
    }

    #[tokio::test]
    async fn names_that_point_inside_are_refused() {
        let r = PublicResolver::default();
        // "localhost" resolves to the loopback address.
        let err = match r.resolve("localhost".parse().unwrap()).await {
            Ok(_) => panic!("resolved"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("non-public"), "{err}");
        let mapped = PublicResolver::new(HashMap::from([("pages.test".into(), ip("127.0.0.1"))]));
        let addrs: Vec<SocketAddr> = mapped
            .resolve("pages.test".parse().unwrap())
            .await
            .unwrap()
            .collect();
        assert_eq!(addrs, [SocketAddr::new(ip("127.0.0.1"), 0)]);
    }
}
