// html_fetch.rs

//! One HTTP policy for links, URL commands, mutations, and redirects.

use reqwest::{
    Client,
    dns::{Addrs, Name, Resolve, Resolving},
    header,
};
use url::{Host, Url};

use crate::*;

pub const MAX_URL_BYTES: usize = 4096;
const BODY_LIMIT: usize = 1024 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_REDIRECTS: usize = 5;

#[derive(Clone, Debug)]
pub struct HttpFetcher {
    client: Client,
}

#[derive(Debug)]
pub struct FetchedText {
    pub body: String,
    pub truncated: bool,
}

#[derive(Debug)]
struct PublicResolver;

impl Resolve for PublicResolver {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let addresses: Vec<_> = lookup_host((name.as_str(), 0)).await?.take(65).collect();
            if addresses.is_empty()
                || addresses.len() > 64
                || addresses.iter().any(|addr| !is_public_address(addr.ip()))
            {
                return Err("HTTP destination has no permitted public addresses".into());
            }
            Ok(Box::new(addresses.into_iter()) as Addrs)
        })
    }
}

impl HttpFetcher {
    pub fn new() -> anyhow::Result<Self> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(FETCH_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .dns_resolver(Arc::new(PublicResolver))
            .pool_max_idle_per_host(1)
            .user_agent(concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self { client })
    }

    pub async fn get_text(&self, input: &str) -> anyhow::Result<Option<FetchedText>> {
        timeout(FETCH_TIMEOUT, self.get_text_inner(input))
            .await
            .context("HTTP fetch timed out")?
    }

    async fn get_text_inner(&self, input: &str) -> anyhow::Result<Option<FetchedText>> {
        if input.len() > MAX_URL_BYTES {
            bail!("URL exceeds {MAX_URL_BYTES} bytes");
        }
        let mut url = Url::parse(input)?;
        for redirects in 0..=MAX_REDIRECTS {
            validate_url(&url)?;
            let mut response = self.client.get(url.clone()).send().await?;
            if matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
                if redirects == MAX_REDIRECTS {
                    bail!("too many HTTP redirects");
                }
                let location = response
                    .headers()
                    .get(header::LOCATION)
                    .context("redirect without Location")?
                    .to_str()?;
                if location.len() > MAX_URL_BYTES {
                    bail!("redirect URL too long");
                }
                url = url.join(location)?;
                continue;
            }
            response = response.error_for_status()?;
            let content_type = response
                .headers()
                .get(header::CONTENT_TYPE)
                .context("No content-type in response")?
                .to_str()?
                .to_owned();
            let media = content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
            if !media.starts_with("text/") && media != "application/xhtml+xml" {
                return Ok(None);
            }
            let mut bytes = Vec::new();
            let mut truncated = false;
            while let Some(chunk) = response.chunk().await? {
                let remaining = BODY_LIMIT - bytes.len();
                let count = chunk.len().min(remaining);
                bytes
                    .try_reserve(count)
                    .context("could not allocate HTTP body buffer")?;
                bytes.extend_from_slice(&chunk[..count]);
                if bytes.len() == BODY_LIMIT {
                    // Stop reading immediately, including when Content-Length is absent or compressed.
                    truncated = true;
                    break;
                }
            }
            let mut body = decode_text(&bytes, &content_type);
            if body.len() > BODY_LIMIT {
                body.truncate(body.floor_char_boundary(BODY_LIMIT));
                truncated = true;
            }
            if truncated {
                debug!("Using truncated HTTP response from {url}");
            }
            return Ok(Some(FetchedText { body, truncated }));
        }
        Err(anyhow!("HTTP redirect limit exceeded"))
    }
}

fn decode_text(bytes: &[u8], content_type: &str) -> String {
    let encoding = content_type
        .split(';')
        .skip(1)
        .find_map(|part| {
            let (name, value) = part.trim().split_once('=')?;
            name.trim()
                .eq_ignore_ascii_case("charset")
                .then(|| encoding_rs::Encoding::for_label(value.trim().trim_matches(['\'', '"']).as_bytes()))
                .flatten()
        })
        .unwrap_or(encoding_rs::UTF_8);
    encoding.decode(bytes).0.into_owned()
}

fn validate_url(url: &Url) -> anyhow::Result<()> {
    if url.as_str().len() > MAX_URL_BYTES || !matches!(url.scheme(), "http" | "https") {
        bail!("unsupported or oversized HTTP URL");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("credentials in HTTP URLs are not allowed");
    }
    match url.host().context("HTTP URL without host")? {
        Host::Ipv4(ip) if !is_public_address(ip.into()) => bail!("non-public HTTP destination"),
        Host::Ipv6(ip) if !is_public_address(ip.into()) => bail!("non-public HTTP destination"),
        _ => Ok(()),
    }
}

/// Conservative policy based on the IANA special-purpose registries.
/// IPv6 translation/tunnelling ranges are excluded to avoid embedded-address bypasses.
fn is_public_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => public_v4(ip),
        IpAddr::V6(ip) => public_v6(ip),
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let n = u32::from(ip);
    let blocked = [
        (Ipv4Addr::new(0, 0, 0, 0), 8),       // This network
        (Ipv4Addr::new(10, 0, 0, 0), 8),      // Private
        (Ipv4Addr::new(100, 64, 0, 0), 10),   // Shared address space (CGNAT)
        (Ipv4Addr::new(127, 0, 0, 0), 8),     // Loopback
        (Ipv4Addr::new(169, 254, 0, 0), 16),  // Link-local
        (Ipv4Addr::new(172, 16, 0, 0), 12),   // Private
        (Ipv4Addr::new(192, 0, 0, 0), 24),    // IETF protocol assignments
        (Ipv4Addr::new(192, 0, 2, 0), 24),    // Documentation (TEST-NET-1)
        (Ipv4Addr::new(192, 88, 99, 0), 24),  // Deprecated 6to4 relay
        (Ipv4Addr::new(192, 168, 0, 0), 16),  // Private
        (Ipv4Addr::new(198, 18, 0, 0), 15),   // Benchmarking
        (Ipv4Addr::new(198, 51, 100, 0), 24), // Documentation (TEST-NET-2)
        (Ipv4Addr::new(203, 0, 113, 0), 24),  // Documentation (TEST-NET-3)
        (Ipv4Addr::new(224, 0, 0, 0), 4),     // Multicast
        (Ipv4Addr::new(240, 0, 0, 0), 4),     // Reserved, including limited broadcast
    ];
    !blocked
        .iter()
        .any(|&(base, prefix)| n & (u32::MAX << (32 - prefix)) == u32::from(base))
}

fn public_v6(ip: Ipv6Addr) -> bool {
    // Only native global unicast; excludes unspecified, loopback, ULA, link-local,
    // multicast, IPv4-mapped/compatible addresses and NAT64 prefixes.
    let n = u128::from(ip);
    let mask = |bits| u128::MAX << (128 - bits);
    if n & mask(3) != u128::from(Ipv6Addr::new(0x2000, 0, 0, 0, 0, 0, 0, 0)) {
        return false;
    }
    let blocked = [
        (Ipv6Addr::new(0x2001, 0, 0, 0, 0, 0, 0, 0), 23),
        (Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 0), 32),
        (Ipv6Addr::new(0x2002, 0, 0, 0, 0, 0, 0, 0), 16),
        (Ipv6Addr::new(0x3fff, 0, 0, 0, 0, 0, 0, 0), 20),
    ];
    !blocked.iter().any(|&(base, bits)| n & mask(bits) == u128::from(base))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The test client explicitly maps a fake public hostname to the local fixture.
    // Production clients always use PublicResolver and cannot make this override.
    async fn fixture(headers: &str, body: Option<Vec<u8>>) -> (HttpFetcher, String, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let headers = format!("{headers}\r\nConnection: close\r\n\r\n");
        let server = spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            loop {
                line.clear();
                if stream.read_line(&mut line).await.unwrap() == 0 || line == "\r\n" {
                    break;
                }
            }
            stream.get_mut().write_all(headers.as_bytes()).await.unwrap();
            if let Some(body) = body {
                // A truncated response is intentionally closed before the fixture finishes writing.
                let _ = stream.get_mut().write_all(&body).await;
            } else {
                pending::<()>().await;
            }
        });
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .resolve("public.test", address)
            .build()
            .unwrap();
        (
            HttpFetcher { client },
            format!("http://public.test:{}/", address.port()),
            server,
        )
    }

    #[tokio::test]
    async fn keeps_prefix_of_large_compressed_body() {
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gzip.write_all(&vec![b'a'; BODY_LIMIT * 2]).unwrap();
        let body = gzip.finish().unwrap();
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Encoding: gzip\r\nContent-Length: {}",
            body.len()
        );
        let (fetcher, url, server) = fixture(&headers, Some(body)).await;
        let text = fetcher.get_text(&url).await.unwrap().unwrap();
        assert_eq!(text.body.len(), BODY_LIMIT);
        assert!(text.truncated);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn skips_binary_body_without_waiting_for_it() {
        let (fetcher, url, server) = fixture(
            "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: 1000000",
            None,
        )
        .await;
        let result = timeout(Duration::from_secs(1), fetcher.get_text(&url))
            .await
            .unwrap()
            .unwrap();
        server.abort();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn validates_redirect_destination_before_connecting() {
        let (fetcher, url, server) = fixture(
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1/secret\r\nContent-Length: 0",
            Some(Vec::new()),
        )
        .await;
        let error = fetcher.get_text(&url).await.unwrap_err();
        assert!(error.to_string().contains("non-public"));
        server.await.unwrap();
    }

    #[test]
    fn rejects_non_public_addresses_in_both_families() {
        for value in [
            "0.1.2.3",
            "10.0.0.1",
            "100.64.0.1",
            "127.9.8.7",
            "169.254.169.254",
            "172.31.255.255",
            "192.168.1.1",
            "192.0.2.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "::ffff:8.8.8.8",
            "64:ff9b::a00:1",
            "fc00::1",
            "fe80::1",
            "ff02::1",
            "2001:db8::1",
            "2002:7f00:1::",
            "3fff::1",
        ] {
            assert!(!is_public_address(value.parse().unwrap()), "{value}");
        }
        for value in [
            "8.8.8.8",
            "1.1.1.1",
            "100.63.255.255",
            "100.128.0.0",
            "172.32.0.1",
            "2606:4700:4700::1111",
        ] {
            assert!(is_public_address(value.parse().unwrap()), "{value}");
        }
    }

    #[test]
    fn rejects_url_bypasses_and_credentials() {
        for value in [
            "http://2130706433/",
            "http://0x7f000001/",
            "http://[::ffff:127.0.0.1]/",
            "http://user:pass@example.com/",
            "file:///etc/passwd",
        ] {
            assert!(validate_url(&Url::parse(value).unwrap()).is_err(), "{value}");
        }
    }

    #[test]
    fn decodes_charsets_and_incomplete_utf8_prefixes() {
        assert_eq!(decode_text(b"caf\xe9", "text/plain; charset=iso-8859-1"), "café");
        assert_eq!(decode_text(b"hello\xc3", "text/html"), "hello\u{fffd}");
    }
}
// EOF
