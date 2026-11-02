use anyhow::{bail, Context, Result};
use regex::Regex;
use std::{net::Ipv4Addr, sync::OnceLock};

pub fn targets(text: &str, extract: bool) -> Result<Vec<(u32, u32)>> {
    if text.len() > 2 * 1024 * 1024 {
        bail!("入力は2 MiB以内にしてください");
    }
    static RE: OnceLock<Regex> = OnceLock::new();
    let re =
        RE.get_or_init(|| Regex::new(r"(?:[0-9]{1,3}\.){3}[0-9]{1,3}(?:/[0-9]{1,3})?").unwrap());
    let tokens: Vec<&str> = if extract {
        re.find_iter(text)
            .filter_map(|m| {
                let before = &text[..m.start()];
                if before
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_ascii_alphanumeric() || c == '.')
                {
                    return None;
                }
                // A slash after a URL host or a second slash starts a path, not a CIDR prefix.
                let token = m.as_str();
                let token = match token.split_once('/') {
                    Some((ip, _))
                        if before.ends_with("://") || text[m.end()..].starts_with('/') =>
                    {
                        ip
                    }
                    _ => token,
                };
                token_ends(&text[m.start() + token.len()..]).then_some(token)
            })
            .collect()
    } else {
        text.lines()
            .flat_map(|l| {
                l.split('#')
                    .next()
                    .unwrap_or("")
                    .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
            })
            .filter(|s| !s.is_empty())
            .collect()
    };
    let mut ranges = Vec::new();
    for token in tokens {
        let (ip, prefix) = token.split_once('/').unwrap_or((token, "32"));
        let parsed = (|| -> Result<_> {
            let ip: Ipv4Addr = ip
                .parse()
                .with_context(|| format!("IPv4が不正です: {token}"))?;
            let prefix: u32 = prefix.parse()?;
            if prefix > 32 {
                bail!("CIDRが不正です: {token}");
            }
            let mask = u32::MAX.checked_shl(32 - prefix).unwrap_or(0);
            let start = u32::from(ip) & mask;
            Ok((start, start | !mask))
        })();
        match parsed {
            Ok(r) => ranges.push(r),
            Err(_) if extract => (),
            Err(e) => return Err(e),
        }
    }
    if ranges.is_empty() {
        bail!("有効なIPv4/CIDRがありません");
    }
    ranges.sort_unstable();
    let mut merged: Vec<(u32, u32)> = Vec::new();
    for (start, end) in ranges {
        if let Some(last) = merged.last_mut() {
            if start as u64 <= last.1 as u64 + 1 {
                last.1 = last.1.max(end);
                continue;
            }
        }
        merged.push((start, end));
    }
    Ok(merged)
}

/// A trailing dot ends a sentence unless it continues a longer dotted name or number.
fn token_ends(rest: &str) -> bool {
    let mut chars = rest.chars();
    match chars.next() {
        None => true,
        Some('.') => !chars.next().is_some_and(|c| c.is_ascii_alphanumeric()),
        Some(c) => !c.is_ascii_alphanumeric(),
    }
}

/// Host/SNI must stay a domain name: URL parsers read forms such as `127.1` as IPv4 literals,
/// which would connect there directly instead of using the pinned target address.
pub fn host(text: &str) -> Result<String> {
    let host = text.trim();
    let is_domain = host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        && reqwest::Url::parse(&format!("http://{host}/"))
            .ok()
            .and_then(|url| url.host_str().map(|h| h.parse::<Ipv4Addr>().is_err()))
            .unwrap_or(false);
    if !is_domain {
        bail!("Host/SNIはドメイン名のみを指定してください");
    }
    Ok(host.to_owned())
}

pub fn ports(text: &str) -> Result<Vec<u16>> {
    let mut values = std::collections::BTreeSet::new();
    for token in text
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
    {
        let (start, end) = token.split_once('-').unwrap_or((token, token));
        let start: u16 = start
            .parse()
            .context("ポートは1〜65535で指定してください")?;
        let end: u16 = end.parse().context("ポート範囲が不正です")?;
        if start == 0 || start > end {
            bail!("ポート範囲が不正です: {token}");
        }
        values.extend(start..=end);
    }
    if values.is_empty() {
        bail!("ポートを指定してください");
    }
    Ok(values.into_iter().collect())
}

pub fn job_count(ranges: &[(u32, u32)], ports: usize, schemes: usize) -> u64 {
    ranges
        .iter()
        .map(|(s, e)| *e as u64 - *s as u64 + 1)
        .sum::<u64>()
        * ports as u64
        * schemes as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn merges_and_handles_boundaries() {
        let r = targets("192.168.1.4/30\n192.168.1.5\n255.255.255.255", false).unwrap();
        assert_eq!(job_count(&r, 1, 1), 5);
        assert_eq!(
            job_count(&targets("0.0.0.0/0", false).unwrap(), 1, 1),
            4294967296
        );
        assert!(targets("1.2.3.4/33", false).is_err());
    }
    #[test]
    fn extracts_without_partial_matches() {
        let r = targets(
            "src=192.168.1.7/24; http://10.0.0.1:80 999.1.1.1 11.2.3.4444 1.2.3.4/333",
            true,
        )
        .unwrap();
        assert_eq!(job_count(&r, 1, 1), 257);
    }
    #[test]
    fn extracts_from_sentences_and_urls() {
        for (text, count) in [
            ("src 10.0.0.1.", 1),
            ("(10.0.0.1)", 1),
            ("http://10.0.0.1/index.html", 1),
            ("http://10.0.0.1/24", 1),
            ("10.0.0.1/24/path", 1),
            ("10.0.0.0/24.", 256),
        ] {
            assert_eq!(
                job_count(&targets(text, true).unwrap(), 1, 1),
                count,
                "{text}"
            );
        }
        for text in ["10.0.0.1.example.com", "1.2.3.4.5", "1.2.3.4/24.5"] {
            assert!(targets(text, true).is_err(), "{text}");
        }
    }
    #[test]
    fn host_must_be_a_domain_name() {
        assert_eq!(host(" fixture.test ").unwrap(), "fixture.test");
        assert!(host("Example.COM").is_ok());
        for s in [
            "127.1", "123", "0x7f.1", "1.2.3.4", "a.1", "a..b", "-a.b", "a_b.c",
        ] {
            assert!(host(s).is_err(), "{s}");
        }
    }
    #[test]
    fn validates_ports() {
        assert_eq!(
            ports("80,443,80,8000-8002").unwrap(),
            vec![80, 443, 8000, 8001, 8002]
        );
        assert_eq!(ports("1-65535").unwrap().len(), 65535);
        for s in ["0", "65536", "4-1", ""] {
            assert!(ports(s).is_err());
        }
    }
}
