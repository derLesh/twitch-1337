use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use eyre::{Result, WrapErr as _, bail};
use futures_util::StreamExt as _;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};

use crate::APP_USER_AGENT;
use crate::ai::content::detect::{Bucket, detect};
use crate::config::AiMediaConfig;
use crate::util::truncate_response;

static SSRF_BYPASS: AtomicBool = AtomicBool::new(false);

const BLOCKED_TARGET: &str = "blocked target host";

fn ssrf_active() -> bool {
    !SSRF_BYPASS.load(Ordering::Relaxed)
}

struct SsrfSafeResolver;

impl Resolve for SsrfSafeResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?
                .collect();
            if ssrf_active()
                && let Some(a) = addrs.iter().find(|a| is_blocked_ip(a.ip()))
            {
                return Err(Box::new(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("{BLOCKED_TARGET}: {}", a.ip()),
                ))
                    as Box<dyn std::error::Error + Send + Sync>);
            }
            let iter: Addrs = Box::new(addrs.into_iter());
            Ok(iter)
        })
    }
}

fn ssrf_safe_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if ssrf_active() && is_blocked_host_literal(attempt.url()) {
            attempt.error(BLOCKED_TARGET)
        } else if attempt.previous().len() >= 10 {
            attempt.stop()
        } else {
            attempt.follow()
        }
    })
}

fn build_ssrf_safe_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(APP_USER_AGENT)
        .dns_resolver(Arc::new(SsrfSafeResolver))
        .redirect(ssrf_safe_redirect_policy())
        .build()
        .wrap_err("Failed to build web-search HTTP client")
}

/// Enable the SSRF bypass for integration tests pointing at loopback
/// (e.g. wiremock). Always `false` in production.
#[doc(hidden)]
pub fn ssrf_bypass_for_tests(enabled: bool) {
    SSRF_BYPASS.store(enabled, Ordering::Relaxed);
}

const SEARX_RESPONSE_LIMIT: usize = 10;

#[derive(Debug, Clone)]
pub enum Payload {
    Text(String),
    Bytes(Vec<u8>),
}

#[derive(Debug, Clone)]
pub struct FetchedContent {
    pub bucket: Bucket,
    pub content_type: String,
    pub payload: Payload,
}

fn overall_cap(caps: &AiMediaConfig) -> u64 {
    [
        caps.max_image_size,
        caps.max_pdf_size,
        caps.max_audio_size,
        caps.max_video_size,
        caps.max_text_size,
    ]
    .iter()
    .max()
    .expect("non-empty")
    .as_u64()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
    pub published_at: Option<String>,
    pub source: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SearchClient {
    http: reqwest::Client,
    base_url: String,
    timeout: Duration,
}

impl SearchClient {
    pub fn new(base_url: &str, timeout: Duration) -> Result<Self> {
        let http = build_ssrf_safe_client()?;
        Ok(Self::new_with_client(base_url.to_string(), timeout, http))
    }

    pub fn new_with_client(base_url: String, timeout: Duration, http: reqwest::Client) -> Self {
        Self {
            http,
            base_url,
            timeout,
        }
    }

    pub async fn web_search(&self, query: &str, max_results: usize) -> Result<Vec<SearchResult>> {
        let effective_max = max_results.min(SEARX_RESPONSE_LIMIT);

        let response: SearxSearchResponse = self
            .http
            .get(&self.base_url)
            .query(&[("q", query), ("format", "json")])
            .timeout(self.timeout)
            .send()
            .await
            .wrap_err("Failed to call SearXNG search endpoint")?
            .error_for_status()
            .wrap_err("SearXNG returned error status")?
            .json()
            .await
            .wrap_err("Failed to parse SearXNG search response")?;

        let results = response
            .results
            .into_iter()
            .take(effective_max)
            .map(|r| SearchResult {
                title: truncate_response(&r.title, 200),
                url: r.url,
                snippet: truncate_response(&r.content.unwrap_or_default(), 500),
                published_at: r.published_date,
                source: r.engine,
            })
            .collect();

        Ok(results)
    }

    pub async fn fetch_for_read(
        &self,
        raw_url: &str,
        caps: &AiMediaConfig,
    ) -> Result<FetchedContent> {
        let url = reqwest::Url::parse(raw_url).wrap_err("Invalid URL")?;

        match url.scheme() {
            "http" | "https" => {}
            other => bail!("Unsupported URL scheme: {other}"),
        }

        if ssrf_active() && is_blocked_host_literal(&url) {
            bail!(BLOCKED_TARGET)
        }

        let response = self
            .http
            .get(url.clone())
            .timeout(self.timeout)
            .send()
            .await
            .wrap_err("Failed to fetch URL")?
            .error_for_status()
            .wrap_err("URL returned error status")?;

        let max_cap = overall_cap(caps);

        if let Some(length) = response.content_length()
            && length > max_cap
        {
            bail!("Response too large")
        }

        let header_ct = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let initial_capacity = response
            .content_length()
            .map(|len| len.min(max_cap) as usize)
            .unwrap_or(0);

        let mut stream = response.bytes_stream();
        let mut buf: Vec<u8> = Vec::with_capacity(initial_capacity);
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.wrap_err("Failed to read URL response body")?;
            if (buf.len() as u64).saturating_add(chunk.len() as u64) > max_cap {
                bail!("Response too large")
            }
            buf.extend_from_slice(&chunk);
        }

        let head = &buf[..buf.len().min(16)];
        let Some(bucket) = detect(&header_ct, head) else {
            bail!("Unsupported content type")
        };

        let bucket_cap = caps.cap_for(bucket).as_u64();
        if buf.len() as u64 > bucket_cap {
            bail!("Response too large")
        }

        let content_type = if header_ct.is_empty() {
            infer::get(head)
                .map(|k| k.mime_type().to_string())
                .unwrap_or_else(|| "application/octet-stream".to_string())
        } else {
            header_ct
        };

        let payload = if bucket == Bucket::Text {
            let media_type = content_type
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            let body = String::from_utf8_lossy(&buf).to_string();
            let text = if matches!(
                media_type.as_str(),
                "text/html" | "application/xhtml+xml" | "application/xml" | "text/xml"
            ) {
                extract_readable_text(&body)
            } else {
                collapse_ws(&body)
            };
            if text.is_empty() {
                bail!("No readable content extracted")
            }
            Payload::Text(text)
        } else {
            Payload::Bytes(buf)
        };

        Ok(FetchedContent {
            bucket,
            content_type,
            payload,
        })
    }
}

#[derive(Debug, Deserialize)]
struct SearxSearchResponse {
    #[serde(default)]
    results: Vec<SearxResult>,
}

#[derive(Debug, Deserialize)]
struct SearxResult {
    #[serde(default)]
    title: String,
    url: String,
    #[serde(default)]
    content: Option<String>,
    #[serde(default, rename = "publishedDate")]
    published_date: Option<String>,
    #[serde(default)]
    engine: Option<String>,
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_unspecified()
                || v4.is_documentation()
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
                || v6.is_unspecified()
                || v6
                    .to_ipv4_mapped()
                    .is_some_and(|v4| is_blocked_ip(IpAddr::V4(v4)))
        }
    }
}

fn is_blocked_host_literal(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else {
        return true;
    };

    if host.eq_ignore_ascii_case("localhost") || host.to_ascii_lowercase().ends_with(".localhost") {
        return true;
    }

    if let Ok(ip) = host.parse::<IpAddr>() {
        return is_blocked_ip(ip);
    }

    false
}

fn extract_readable_text(html: &str) -> String {
    let doc = Html::parse_document(html);
    let article_sel = Selector::parse("article, main").expect("valid selector");
    let para_sel = Selector::parse("p, h1, h2, h3, li, blockquote").expect("valid selector");
    let body_sel = Selector::parse("body").expect("valid selector");

    let mut chunks: Vec<String> = doc
        .select(&article_sel)
        .flat_map(|node| node.select(&para_sel))
        .map(|n| collapse_ws(&n.text().collect::<Vec<_>>().join(" ")))
        .filter(|line| !line.is_empty())
        .collect();

    if chunks.is_empty()
        && let Some(body) = doc.select(&body_sel).next()
    {
        chunks = body
            .select(&para_sel)
            .map(|n| collapse_ws(&n.text().collect::<Vec<_>>().join(" ")))
            .filter(|line| !line.is_empty())
            .collect();
    }

    if chunks.is_empty() {
        return collapse_ws(&doc.root_element().text().collect::<Vec<_>>().join(" "));
    }

    chunks.join("\n")
}

#[cfg(test)]
mod tests {

    use std::time::Duration;

    use wiremock::matchers::{method as wm_method, path as wm_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::ai::content::detect::Bucket;

    #[test]
    fn blocks_localhost_and_private_ips() {
        assert!(is_blocked_host_literal(
            &reqwest::Url::parse("http://localhost/test").expect("url")
        ));
        assert!(is_blocked_host_literal(
            &reqwest::Url::parse("http://127.0.0.1/test").expect("url")
        ));
        assert!(is_blocked_host_literal(
            &reqwest::Url::parse("http://10.0.0.2/test").expect("url")
        ));
        assert!(!is_blocked_host_literal(
            &reqwest::Url::parse("https://example.com/test").expect("url")
        ));
    }

    fn is_html_content_type(content_type: &str) -> bool {
        let media_type = content_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();

        matches!(
            media_type.as_str(),
            "text/html" | "application/xhtml+xml" | "application/xml" | "text/xml"
        )
    }

    #[test]
    fn detects_html_like_content_types() {
        assert!(is_html_content_type("text/html"));
        assert!(is_html_content_type("text/html; charset=utf-8"));
        assert!(is_html_content_type("application/xhtml+xml"));
        assert!(is_html_content_type("application/xml"));
        assert!(!is_html_content_type("application/json"));
    }

    #[test]
    fn blocks_ipv6_mapped_ipv4_loopback() {
        assert!(is_blocked_ip(
            "::ffff:127.0.0.1".parse::<IpAddr>().expect("ip")
        ));
        assert!(is_blocked_ip(
            "::ffff:10.0.0.1".parse::<IpAddr>().expect("ip")
        ));
        assert!(!is_blocked_ip(
            "::ffff:8.8.8.8".parse::<IpAddr>().expect("ip")
        ));
    }

    #[test]
    fn extracts_readable_text_from_html() {
        let html = r#"
            <html><body>
                <nav>menu</nav>
                <article>
                    <h1>Title</h1>
                    <p>First paragraph.</p>
                    <p>Second paragraph.</p>
                </article>
                <script>ignore me</script>
            </body></html>
        "#;

        let out = extract_readable_text(html);
        assert!(out.contains("Title"), "got: {out}");
        assert!(out.contains("First paragraph."), "got: {out}");
        assert!(!out.contains("ignore me"), "got: {out}");
    }

    #[test]
    fn parses_searx_json_shape() {
        let payload = serde_json::json!({
            "results": [
                {
                    "title": "Headline",
                    "url": "https://example.com/news",
                    "content": "Snippet text",
                    "publishedDate": "2026-04-25",
                    "engine": "news"
                }
            ]
        });
        let parsed: SearxSearchResponse = serde_json::from_value(payload).expect("parse");
        assert_eq!(parsed.results.len(), 1);
        assert_eq!(parsed.results[0].title, "Headline");
        assert_eq!(parsed.results[0].url, "https://example.com/news");
    }

    fn caps() -> AiMediaConfig {
        AiMediaConfig::default()
    }

    fn make_test_client(base_url: &str) -> SearchClient {
        ssrf_bypass_for_tests(true);
        SearchClient::new(base_url, Duration::from_secs(2)).expect("client")
    }

    #[tokio::test]
    async fn fetch_for_read_returns_text_bucket_for_html() {
        crate::install_crypto_provider();
        let server = MockServer::start().await;
        Mock::given(wm_method("GET"))
            .and(wm_path("/page"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/html; charset=utf-8")
                    .set_body_string("<html><body><p>Hello</p></body></html>"),
            )
            .mount(&server)
            .await;

        let client = make_test_client(&format!("{}/search", server.uri()));
        let url = format!("{}/page", server.uri());
        let fetched = client
            .fetch_for_read(&url, &caps())
            .await
            .expect("fetch ok");
        assert_eq!(fetched.bucket, Bucket::Text);
        match fetched.payload {
            Payload::Text(t) => assert!(t.contains("Hello"), "got: {t}"),
            Payload::Bytes(_) => panic!("expected Text payload"),
        }
    }

    #[tokio::test]
    async fn fetch_for_read_returns_image_bucket_for_png() {
        crate::install_crypto_provider();
        let server = MockServer::start().await;
        let png = vec![0x89u8, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00];
        Mock::given(wm_method("GET"))
            .and(wm_path("/p.png"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/png")
                    .set_body_bytes(png.clone()),
            )
            .mount(&server)
            .await;

        let client = make_test_client(&format!("{}/search", server.uri()));
        let url = format!("{}/p.png", server.uri());
        let fetched = client.fetch_for_read(&url, &caps()).await.expect("fetch");
        assert_eq!(fetched.bucket, Bucket::Image);
        match fetched.payload {
            Payload::Bytes(b) => assert_eq!(b, png),
            Payload::Text(_) => panic!("expected Bytes payload"),
        }
    }

    #[tokio::test]
    async fn fetch_for_read_rejects_oversize_via_content_length() {
        crate::install_crypto_provider();
        let server = MockServer::start().await;
        let png_magic = vec![0x89u8, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00];
        Mock::given(wm_method("GET"))
            .and(wm_path("/big.png"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/png")
                    .set_body_bytes(png_magic.clone()),
            )
            .mount(&server)
            .await;

        let tiny_caps = AiMediaConfig {
            max_image_size: bytesize::ByteSize::b(1),
            max_pdf_size: bytesize::ByteSize::b(1),
            max_audio_size: bytesize::ByteSize::b(1),
            max_video_size: bytesize::ByteSize::b(1),
            max_text_size: bytesize::ByteSize::b(1),
            ..AiMediaConfig::default()
        };

        let client = make_test_client(&format!("{}/search", server.uri()));
        let err = client
            .fetch_for_read(&format!("{}/big.png", server.uri()), &tiny_caps)
            .await
            .expect_err("should reject");
        assert!(
            err.to_string().to_lowercase().contains("too large"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn fetch_for_read_rejects_unsupported_content_type() {
        crate::install_crypto_provider();
        let server = MockServer::start().await;
        Mock::given(wm_method("GET"))
            .and(wm_path("/x"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/x-mystery")
                    .set_body_bytes(vec![0xDE, 0xAD, 0xBE, 0xEF]),
            )
            .mount(&server)
            .await;

        let client = make_test_client(&format!("{}/search", server.uri()));
        let err = client
            .fetch_for_read(&format!("{}/x", server.uri()), &caps())
            .await
            .expect_err("reject");
        assert!(
            err.to_string().to_lowercase().contains("unsupported"),
            "{err}"
        );
    }
}
