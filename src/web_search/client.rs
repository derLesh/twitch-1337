use std::net::IpAddr;
use std::time::Duration;

use eyre::{Result, WrapErr as _, bail};
use reqwest::header;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};

use crate::APP_USER_AGENT;

const SEARX_RESPONSE_LIMIT: usize = 10;
const FETCH_RESPONSE_MAX_BYTES: usize = 512 * 1024;
const FETCH_TEXT_MAX_CHARS: usize = 8_000;

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
        let http = reqwest::Client::builder()
            .user_agent(APP_USER_AGENT)
            .build()
            .wrap_err("Failed to build web-search HTTP client")?;

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
                title: truncate_chars(&collapse_ws(&r.title), 200),
                url: r.url,
                snippet: truncate_chars(&collapse_ws(&r.content.unwrap_or_default()), 500),
                published_at: r.published_date,
                source: r.engine,
            })
            .collect();

        Ok(results)
    }

    pub async fn fetch_url(&self, raw_url: &str) -> Result<String> {
        let url = reqwest::Url::parse(raw_url).wrap_err("Invalid URL")?;

        match url.scheme() {
            "http" | "https" => {}
            other => bail!("Unsupported URL scheme: {other}"),
        }

        if is_blocked_host_literal(&url) {
            bail!("Blocked target host")
        }

        if resolves_to_blocked_ip(&url).await? {
            bail!("Blocked target host")
        }

        let response = self
            .http
            .get(url)
            .timeout(self.timeout)
            .send()
            .await
            .wrap_err("Failed to fetch URL")?
            .error_for_status()
            .wrap_err("URL returned error status")?;

        if let Some(length) = response.content_length()
            && length > FETCH_RESPONSE_MAX_BYTES as u64
        {
            bail!("Response too large")
        }

        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();

        let bytes = response
            .bytes()
            .await
            .wrap_err("Failed to read URL response body")?;

        if bytes.len() > FETCH_RESPONSE_MAX_BYTES {
            bail!("Response too large")
        }

        let body = String::from_utf8_lossy(&bytes).to_string();
        let text = if is_html_content_type(&content_type) {
            extract_readable_text(&body)
        } else {
            collapse_ws(&body)
        };

        if text.is_empty() {
            bail!("No readable content extracted")
        }

        Ok(truncate_chars(&text, FETCH_TEXT_MAX_CHARS))
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

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let len = value.chars().count();
    if len <= max_chars {
        return value.to_string();
    }
    let cutoff = value
        .char_indices()
        .nth(max_chars)
        .map(|(idx, _)| idx)
        .unwrap_or(value.len());
    format!("{}...", &value[..cutoff])
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
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

async fn resolves_to_blocked_ip(url: &reqwest::Url) -> Result<bool> {
    let Some(host) = url.host_str() else {
        return Ok(true);
    };

    if host.parse::<IpAddr>().is_ok() {
        return Ok(false);
    }

    let port = url.port_or_known_default().unwrap_or(80);
    let mut saw_any_address = false;

    let addrs = tokio::net::lookup_host((host, port))
        .await
        .wrap_err("Failed to resolve target host")?;

    for addr in addrs {
        saw_any_address = true;
        if is_blocked_ip(addr.ip()) {
            return Ok(true);
        }
    }

    if !saw_any_address {
        return Ok(true);
    }

    Ok(false)
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
    use super::*;

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

    #[test]
    fn detects_html_like_content_types() {
        assert!(is_html_content_type("text/html"));
        assert!(is_html_content_type("text/html; charset=utf-8"));
        assert!(is_html_content_type("application/xhtml+xml"));
        assert!(is_html_content_type("application/xml"));
        assert!(!is_html_content_type("application/json"));
    }

    #[tokio::test]
    async fn blocks_dns_resolution_to_loopback() {
        let url = reqwest::Url::parse("http://localhost/test").expect("url");
        assert!(resolves_to_blocked_ip(&url).await.expect("dns resolve"));
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
}
