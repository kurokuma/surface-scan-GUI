use crate::model::{Config, Record};
use base64::Engine;
use futures::StreamExt;
use reqwest::{Client, Url};
use scraper::{Html, Selector};
use sha2::{Digest, Sha256};
use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::OnceLock,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

// MurmurHash3 x86_32, seed 0. Favicon uses MIME Base64 (76 columns, LF, final LF).
pub fn mmh3(bytes: &[u8]) -> i32 {
    let mut h = 0u32;
    let mix = |mut k: u32| {
        k = k.wrapping_mul(0xcc9e2d51);
        k = k.rotate_left(15);
        k.wrapping_mul(0x1b873593)
    };
    let mut chunks = bytes.chunks_exact(4);
    for c in &mut chunks {
        h ^= mix(u32::from_le_bytes(c.try_into().unwrap()));
        h = h.rotate_left(13).wrapping_mul(5).wrapping_add(0xe6546b64);
    }
    let mut tail = 0u32;
    for (i, b) in chunks.remainder().iter().enumerate() {
        tail |= (*b as u32) << (8 * i);
    }
    if !chunks.remainder().is_empty() {
        h ^= mix(tail);
    }
    h ^= bytes.len() as u32;
    h ^= h >> 16;
    h = h.wrapping_mul(0x85ebca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2ae35);
    h ^= h >> 16;
    h as i32
}

fn favicon_mmh3(bytes: &[u8]) -> i32 {
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    let mut mime = Vec::new();
    for line in encoded.as_bytes().chunks(76) {
        mime.extend_from_slice(line);
        mime.push(b'\n');
    }
    mmh3(&mime)
}

pub fn client(config: &Config, ip: Ipv4Addr, port: u16) -> anyhow::Result<Client> {
    let mut builder = Client::builder()
        .no_proxy()
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .redirect(reqwest::redirect::Policy::none())
        .danger_accept_invalid_certs(config.insecure)
        .connect_timeout(Duration::from_secs(config.timeout_secs))
        .timeout(Duration::from_secs(config.timeout_secs))
        .pool_max_idle_per_host(0)
        .user_agent("WebServiceFingerprint/0.1 (service discovery)");
    if !config.host.is_empty() {
        builder = builder.resolve(&config.host, SocketAddr::new(ip.into(), port));
    }
    Ok(builder.build()?)
}

async fn read_body(response: reqwest::Response, limit: usize) -> anyhow::Result<(Vec<u8>, bool)> {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let remaining = limit.saturating_sub(bytes.len());
        if chunk.len() > remaining {
            bytes.extend_from_slice(&chunk[..remaining]);
            return Ok((bytes, true));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok((bytes, false))
}

fn html_info(body: &str) -> (String, Option<String>) {
    static TITLE: OnceLock<Selector> = OnceLock::new();
    static LINK: OnceLock<Selector> = OnceLock::new();
    let document = Html::parse_document(body);
    let title = document
        .select(TITLE.get_or_init(|| Selector::parse("title").unwrap()))
        .next()
        .map(|n| {
            n.text()
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    let icon = document
        .select(LINK.get_or_init(|| Selector::parse("link[rel][href]").unwrap()))
        .find(|n| {
            n.value()
                .attr("rel")
                .unwrap_or("")
                .split_whitespace()
                .any(|r| r.eq_ignore_ascii_case("icon"))
        })
        .and_then(|n| n.value().attr("href"))
        .map(str::to_owned);
    (title, icon)
}

pub async fn probe(config: &Config, ip: Ipv4Addr, port: u16, scheme: &str) -> Record {
    let start = Instant::now();
    let hostname = if config.host.is_empty() {
        ip.to_string()
    } else {
        config.host.clone()
    };
    let url = format!("{scheme}://{hostname}:{port}/");
    let mut record = Record {
        ip: ip.to_string(),
        port,
        scheme: scheme.into(),
        url: url.clone(),
        host: hostname,
        timestamp_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
        ..Default::default()
    };
    let client = match client(config, ip, port) {
        Ok(c) => c,
        Err(e) => {
            record.error = e.to_string();
            return record;
        }
    };
    let result = async {
        let response = client
            .get(&url)
            .header("Accept-Encoding", "identity")
            .send()
            .await?;
        record.status = Some(response.status().as_u16());
        record.http_version = format!("{:?}", response.version());
        for name in response.headers().keys() {
            record.response_headers.insert(
                name.to_string(),
                response
                    .headers()
                    .get_all(name)
                    .iter()
                    .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
                    .collect(),
            );
        }
        let header = |name: &str| {
            record
                .response_headers
                .get(name)
                .and_then(|v| v.first())
                .cloned()
                .unwrap_or_default()
        };
        record.server = header("server");
        record.content_type = header("content-type");
        record.content_length = header("content-length").parse().ok();
        record.location = header("location");
        record.cookies = record
            .response_headers
            .get("set-cookie")
            .cloned()
            .unwrap_or_default();
        let (bytes, truncated) = read_body(response, config.body_limit).await?;
        record.body_sha256 = sha256(&bytes);
        record.body_bytes = bytes.len();
        record.body_truncated = truncated;
        record.body_base64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        (
            record.body,
            record.body_encoding,
            record.body_decoding_lossy,
        ) = crate::text::decode(&bytes, &record.content_type);
        let (title, icon) = html_info(&record.body);
        record.title = title;
        record.title_sha256 = sha256(record.title.as_bytes());
        let base = Url::parse(&url)?;
        // Keep every follow-up on the requested origin/IP/port; never follow redirects or CDN links.
        match base.join(icon.as_deref().unwrap_or("/favicon.ico")) {
            Err(e) => record.favicon_error = format!("favicon URLが不正です: {e}"),
            Ok(icon_url)
                if icon_url.origin() != base.origin()
                    || !icon_url.username().is_empty()
                    || icon_url.password().is_some() =>
            {
                record.favicon_error = "外部originのfaviconは取得対象外".into();
            }
            Ok(icon_url) => {
                record.favicon_url = icon_url.to_string();
                match async {
                    let response = client
                        .get(icon_url)
                        .header("Accept-Encoding", "identity")
                        .send()
                        .await?;
                    record.favicon_status = Some(response.status().as_u16());
                    if !response.status().is_success() {
                        anyhow::bail!("favicon HTTP {}", response.status());
                    }
                    let (bytes, truncated) = read_body(response, config.body_limit).await?;
                    if truncated {
                        anyhow::bail!("faviconが取得上限を超えました");
                    }
                    record.favicon_sha256 = sha256(&bytes);
                    record.favicon_mmh3 = Some(favicon_mmh3(&bytes));
                    Ok::<_, anyhow::Error>(())
                }
                .await
                {
                    Ok(_) => (),
                    Err(e) => record.favicon_error = format!("{e:#}"),
                }
            }
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if let Err(e) = result {
        record.error = format!("{e:#}");
    }
    if scheme == "https" {
        match tokio::time::timeout(
            Duration::from_secs(config.timeout_secs),
            crate::tls::inspect(&ip.to_string(), port, &config.host, config.insecure),
        )
        .await
        {
            Ok(Ok(info)) => record.tls = Some(info),
            Ok(Err(e)) => record.tls_error = format!("{e:#}"),
            Err(_) => record.tls_error = "TLS metadata timeout".into(),
        }
    }
    record.elapsed_ms = start.elapsed().as_millis();
    record
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hash_vectors() {
        assert_eq!(mmh3(b""), 0);
        assert_eq!(mmh3(b"foo"), -156908512);
        assert_eq!(mmh3(b"hello"), 613153351);
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
    #[tokio::test]
    async fn local_http_fingerprint() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buf = [0; 4096];
                let n = stream.read(&mut buf).await.unwrap();
                let favicon = String::from_utf8_lossy(&buf[..n]).contains("GET /icon.ico");
                let body = if favicon {
                    "ICON"
                } else {
                    "<title>Hello &amp; Rust</title><link rel='icon' href='/icon.ico'>"
                };
                stream.write_all(format!("HTTP/1.1 200 OK\r\nServer: fixture\r\nSet-Cookie: a=1\r\nSet-Cookie: b=2\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).as_bytes()).await.unwrap();
            }
        });
        let cfg = Config {
            ranges: vec![],
            ports: vec![port],
            schemes: vec!["http".into()],
            concurrency: 2,
            timeout_secs: 2,
            body_limit: 4096,
            insecure: true,
            host: String::new(),
            worker_index: 0,
            workers: 1,
        };
        let r = probe(&cfg, Ipv4Addr::LOCALHOST, port, "http").await;
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
        assert!(r.error.is_empty(), "{}", r.error);
        assert_eq!(r.status, Some(200));
        assert_eq!(r.title, "Hello & Rust");
        assert_eq!(r.cookies.len(), 2);
        assert_eq!(r.favicon_sha256, sha256(b"ICON"));
        assert!(!r.body_truncated);
    }

    #[tokio::test]
    async fn truncation_and_redirect_scope() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for redirect in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = tokio::spawn(async move {
                // One root and one local favicon request, with no request to redirect/remote icon.
                for _ in 0..2 {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let mut buf = [0; 2048];
                    let _ = stream.read(&mut buf).await.unwrap();
                    let response = if redirect {
                        "HTTP/1.1 302 Found\r\nLocation: http://192.0.2.1/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned()
                    } else {
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: 20\r\nConnection: close\r\n\r\n{}",
                            "x".repeat(20)
                        )
                    };
                    stream.write_all(response.as_bytes()).await.unwrap();
                }
            });
            let cfg = Config {
                ranges: vec![],
                ports: vec![port],
                schemes: vec![],
                concurrency: 1,
                timeout_secs: 2,
                body_limit: 8,
                insecure: true,
                host: "fixture.test".into(),
                worker_index: 0,
                workers: 1,
            };
            let r = probe(&cfg, Ipv4Addr::LOCALHOST, port, "http").await;
            tokio::time::timeout(Duration::from_secs(5), server)
                .await
                .unwrap()
                .unwrap();
            assert!(r.error.is_empty(), "{}", r.error);
            if redirect {
                assert_eq!(r.status, Some(302));
                assert_eq!(r.location, "http://192.0.2.1/");
            } else {
                assert!(r.body_truncated);
                assert_eq!(r.body_bytes, 8);
                assert_eq!(r.body_sha256, sha256(b"xxxxxxxx"));
                assert!(r.favicon_mmh3.is_none());
            }
        }
    }

    #[tokio::test]
    async fn invalid_favicon_url_is_not_a_probe_error() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0; 2048];
            let _ = stream.read(&mut buf).await.unwrap();
            let body = "<title>ok</title><link rel=icon href='http://'>";
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        let cfg = Config {
            ranges: vec![],
            ports: vec![port],
            schemes: vec![],
            concurrency: 1,
            timeout_secs: 2,
            body_limit: 4096,
            insecure: true,
            host: String::new(),
            worker_index: 0,
            workers: 1,
        };
        let r = probe(&cfg, Ipv4Addr::LOCALHOST, port, "http").await;
        server.await.unwrap();
        assert!(r.error.is_empty(), "{}", r.error);
        assert_eq!(r.title, "ok");
        assert!(r.favicon_error.contains("favicon URL"));
    }

    #[test]
    fn external_icon_stays_external() {
        let (_, icon) = html_info("<link rel='shortcut icon' href='https://example.org/a.ico'>");
        let base = Url::parse("http://127.0.0.1:80/").unwrap();
        assert_ne!(
            base.join(icon.as_deref().unwrap()).unwrap().origin(),
            base.origin()
        );
    }
}
