use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub ranges: Vec<(u32, u32)>,
    pub ports: Vec<u16>,
    pub schemes: Vec<String>,
    pub concurrency: usize,
    pub timeout_secs: u64,
    pub body_limit: usize,
    pub insecure: bool,
    pub host: String,
    pub worker_index: usize,
    pub workers: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TlsInfo {
    pub version: String,
    pub cipher: String,
    pub subject: String,
    pub issuer: String,
    pub serial: String,
    pub san: Vec<String>,
    pub self_signed: bool,
    pub not_before: String,
    pub not_after: String,
    pub certificate_sha256: String,
    pub chain_sha256: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Record {
    pub ip: String,
    pub port: u16,
    pub scheme: String,
    pub url: String,
    pub host: String,
    pub timestamp_unix_ms: u128,
    pub elapsed_ms: u128,
    pub status: Option<u16>,
    pub http_version: String,
    pub title: String,
    pub title_sha256: String,
    pub body: String,
    pub body_base64: String,
    pub body_encoding: String,
    pub body_decoding_lossy: bool,
    pub body_sha256: String,
    pub body_bytes: usize,
    pub body_truncated: bool,
    pub server: String,
    pub content_length: Option<u64>,
    pub content_type: String,
    pub response_headers: BTreeMap<String, Vec<String>>,
    pub cookies: Vec<String>,
    pub location: String,
    pub favicon_url: String,
    pub favicon_status: Option<u16>,
    pub favicon_sha256: String,
    pub favicon_mmh3: Option<i32>,
    pub favicon_error: String,
    pub tls: Option<TlsInfo>,
    pub tls_error: String,
    pub error: String,
}

impl Record {
    pub fn browser_url(&self) -> anyhow::Result<String> {
        let ip: std::net::Ipv4Addr = self.ip.parse()?;
        anyhow::ensure!(
            self.port != 0 && matches!(self.scheme.as_str(), "http" | "https"),
            "IP・方式・ポートが不正です"
        );
        Ok(format!("{}://{}:{}/", self.scheme, ip, self.port))
    }

    pub fn search_text(&self) -> String {
        serde_json::to_string(self)
            .unwrap_or_default()
            .to_lowercase()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn browser_links_use_only_validated_endpoint() {
        let mut row = Record {
            ip: "192.0.2.1".into(),
            port: 8443,
            scheme: "https".into(),
            url: "file:///untrusted".into(),
            ..Default::default()
        };
        assert_eq!(row.browser_url().unwrap(), "https://192.0.2.1:8443/");
        row.scheme = "javascript".into();
        assert!(row.browser_url().is_err());
    }
}
