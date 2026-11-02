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
    /// Stable job ordinals whose complete results are already retained by the GUI.
    #[serde(default)]
    pub completed_jobs: Vec<u64>,
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

    /// Lowercased field values, one per line. Field names and escapes never match, and Base64
    /// is left out because it only repeats the body as text that matches queries by chance.
    pub fn search_text(&self) -> String {
        fn collect(value: &serde_json::Value, text: &mut String) {
            match value {
                serde_json::Value::String(s) => {
                    text.push_str(s);
                    text.push('\n');
                }
                serde_json::Value::Number(n) => {
                    text.push_str(&n.to_string());
                    text.push('\n');
                }
                serde_json::Value::Array(items) => items.iter().for_each(|v| collect(v, text)),
                serde_json::Value::Object(map) => map.values().for_each(|v| collect(v, text)),
                serde_json::Value::Bool(_) | serde_json::Value::Null => (),
            }
        }
        let mut text = self
            .response_headers
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        text.push('\n');
        if let Ok(serde_json::Value::Object(mut fields)) = serde_json::to_value(self) {
            fields.remove("body_base64");
            fields.values().for_each(|v| collect(v, &mut text));
        }
        text.to_lowercase()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn search_matches_values_not_field_names() {
        let empty = Record::default().search_text();
        for query in ["title", "favicon_mmh3", "null", "error"] {
            assert!(!empty.contains(query), "{query}");
        }
        let mut row = Record {
            body: "a\"b C:\\x".into(),
            body_base64: "QUJDREVG".into(),
            port: 8443,
            ..Default::default()
        };
        row.response_headers
            .insert("x-powered-by".into(), vec!["PHP".into()]);
        row.tls = Some(TlsInfo {
            subject: "CN=Fixture".into(),
            ..Default::default()
        });
        let text = row.search_text();
        for query in ["a\"b", "c:\\x", "8443", "x-powered-by", "php", "cn=fixture"] {
            assert!(text.contains(query), "{query}");
        }
        assert!(!text.contains("qujdrevg"));
    }
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
