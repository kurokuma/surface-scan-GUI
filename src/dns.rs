use crate::model::Config;
use anyhow::Result;
use futures::{stream, StreamExt};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

/// Resolve outside the GUI, pin one sorted IPv4 per domain, and preserve successful lookups
/// across pause/resume. Unresolved names are retried, completed mappings remain unchanged.
pub fn prepare(config: &mut Config, cancel: &AtomicBool) -> Result<()> {
    if !config.resolve_domains {
        return Ok(());
    }
    let todo: Vec<_> = config
        .domains
        .iter()
        .enumerate()
        .filter(|(_, d)| d.ip.is_none())
        .map(|(i, d)| (i, d.domain.clone()))
        .collect();
    let timeout = Duration::from_secs(config.timeout_secs);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let result = runtime.block_on(async {
        let mut lookups = stream::iter(todo)
            .map(|(i, name)| async move {
                let result =
                    tokio::time::timeout(timeout, tokio::net::lookup_host((name.as_str(), 0)))
                        .await;
                let ip = match result {
                    Ok(Ok(addrs)) => addrs
                        .filter_map(|a| match a.ip() {
                            std::net::IpAddr::V4(ip) => Some(ip),
                            _ => None,
                        })
                        .min()
                        .ok_or_else(|| format!("{name}: IPv4アドレスがありません")),
                    Ok(Err(e)) => Err(format!("{name}: {e}")),
                    Err(_) => Err(format!("{name}: DNSタイムアウト")),
                };
                (i, ip)
            })
            .buffer_unordered(16);
        let mut errors = Vec::new();
        loop {
            if cancel.load(Ordering::Relaxed) {
                anyhow::bail!("名前解決を停止しました");
            }
            tokio::select! {
                item = lookups.next() => match item {
                    Some((i,Ok(ip))) => config.domains[i].ip = Some(ip),
                    Some((_,Err(e))) => errors.push(e),
                    None => break,
                },
                _ = tokio::time::sleep(Duration::from_millis(25)) => (),
            }
        }
        anyhow::ensure!(
            errors.is_empty(),
            "DNS解決に失敗しました: {}",
            errors.into_iter().take(5).collect::<Vec<_>>().join(" / ")
        );
        Ok(())
    });
    // System DNS may finish later; never block stop/window-close while waiting for getaddrinfo.
    runtime.shutdown_background();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_dns_preserves_pins_and_direct_mode_never_pre_resolves() {
        let mut cfg = Config {
            ranges: vec![],
            ports: vec![80],
            schemes: vec!["http".into()],
            concurrency: 1,
            timeout_secs: 2,
            body_limit: 1024,
            insecure: true,
            host: String::new(),
            worker_index: 0,
            workers: 1,
            completed_jobs: vec![],
            domains: vec![crate::model::DomainTarget {
                domain: "localhost".into(),
                ip: None,
            }],
            resolve_domains: false,
        };
        let cancel = AtomicBool::new(false);
        prepare(&mut cfg, &cancel).unwrap();
        assert!(cfg.domains[0].ip.is_none());
        cfg.resolve_domains = true;
        prepare(&mut cfg, &cancel).unwrap();
        assert_eq!(cfg.domains[0].ip, Some(std::net::Ipv4Addr::LOCALHOST));
        cfg.domains[0].domain = "no-dns-request.invalid".into();
        prepare(&mut cfg, &cancel).unwrap();
        assert_eq!(cfg.domains[0].ip, Some(std::net::Ipv4Addr::LOCALHOST));
        cfg.domains[0].ip = None;
        cancel.store(true, Ordering::Relaxed);
        assert!(prepare(&mut cfg, &cancel).is_err());
    }
}
