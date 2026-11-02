use encoding_rs::Encoding;
use regex::Regex;
use std::sync::OnceLock;

/// HTTP charset, HTML declarations and statistical detection, with BOM taking precedence.
pub fn decode(bytes: &[u8], content_type: &str) -> (String, String, bool) {
    let header = content_type
        .split(';')
        .skip(1)
        .find_map(|part| {
            let (key, value) = part.trim().split_once('=')?;
            key.trim()
                .eq_ignore_ascii_case("charset")
                .then(|| value.trim().trim_matches(['\'', '"']))
        })
        .and_then(|label| Encoding::for_label(label.as_bytes()));
    let bom = Encoding::for_bom(bytes).map(|(e, _)| e);
    let meta = || {
        static CHARSET: OnceLock<Regex> = OnceLock::new();
        let re = CHARSET
            .get_or_init(|| Regex::new(r#"(?i)charset\s*=\s*["']?\s*([a-z0-9._-]+)"#).unwrap());
        let prefix = String::from_utf8_lossy(&bytes[..bytes.len().min(8192)]);
        let html = scraper::Html::parse_document(&prefix);
        let selector = scraper::Selector::parse("meta").unwrap();
        for element in html.select(&selector) {
            if let Some(encoding) = element
                .value()
                .attr("charset")
                .and_then(|s| Encoding::for_label(s.trim().as_bytes()))
            {
                return Some(encoding);
            }
            if element
                .value()
                .attr("http-equiv")
                .is_some_and(|s| s.eq_ignore_ascii_case("content-type"))
            {
                if let Some(content) = element.value().attr("content") {
                    if let Some(cap) = re.captures(content) {
                        if let Some(encoding) = Encoding::for_label(cap[1].as_bytes()) {
                            return Some(encoding);
                        }
                    }
                }
            }
        }
        None
    };
    let encoding = bom.or(header).or_else(meta).unwrap_or_else(|| {
        if std::str::from_utf8(bytes).map_or_else(|e| e.error_len().is_none(), |_| true) {
            return encoding_rs::UTF_8;
        }
        let mut detector = chardetng::EncodingDetector::new();
        detector.feed(bytes, true);
        detector.guess(None, true)
    });
    let (text, actual, lossy) = encoding.decode(bytes);
    (text.into_owned(), actual.name().into(), lossy)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chinese_encodings_and_meta() {
        for (encoding, label, text) in [
            (encoding_rs::GBK, "gb2312", "中文网页标题，欢迎使用网络服务"),
            (encoding_rs::GB18030, "gb18030", "简体中文服务𠀀"),
            (
                encoding_rs::BIG5,
                "big5",
                "繁體中文網頁標題，歡迎使用網路服務",
            ),
        ] {
            for meta in [
                format!("<meta charset='{label}'>"),
                format!("<meta http-equiv='Content-Type' content='text/html; charset={label}'>"),
            ] {
                let html = format!("{meta}<title>{text}</title>");
                let (bytes, _, errors) = encoding.encode(&html);
                assert!(!errors);
                let (decoded, _, errors) = decode(&bytes, "text/html");
                assert!(!errors);
                assert_eq!(decoded, html);
                assert_eq!(
                    decode(&bytes, &format!("text/html; charset={label}")).0,
                    html
                );
            }
        }
        let text = "<title>中文・日本語</title>";
        assert_eq!(decode(text.as_bytes(), "text/html").0, text);
        assert_eq!(
            decode(b"\xef\xbb\xbfhello", "text/html; charset=gbk").0,
            "hello"
        );
    }
}
