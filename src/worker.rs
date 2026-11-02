use crate::model::{Config, Record};
use anyhow::{Context, Result};
use futures::{stream, StreamExt};
use std::{
    io::{BufRead, BufReader, Write},
    net::Ipv4Addr,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::SyncSender,
        Arc,
    },
    time::Duration,
};

pub enum Event {
    Prepared(Vec<crate::model::DomainTarget>),
    Row(u64, Box<Record>),
    Problem(String),
    Done(bool),
}

#[derive(serde::Serialize, serde::Deserialize)]
struct WorkerRow {
    job_id: u64,
    record: Record,
}

pub fn run_worker() -> Result<()> {
    let mut config: Config = serde_json::from_reader(std::io::stdin().lock())?;
    for target in &mut config.domains {
        target.domain = crate::input::domain(&target.domain)?;
    }
    anyhow::ensure!(
        (1..=8).contains(&config.workers)
            && config.worker_index < config.workers
            && (1..=1024).contains(&config.concurrency)
            && (config.host.is_empty() || crate::input::host(&config.host).is_ok()),
        "invalid worker configuration"
    );
    anyhow::ensure!(
        config
            .domains
            .iter()
            .all(|d| crate::input::domain(&d.domain).is_ok()),
        "invalid domain target"
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let config = Arc::new(config);
        let cfg = &*config;
        let completed: std::collections::HashSet<u64> =
            cfg.completed_jobs.iter().copied().collect();
        let jobs = cfg
            .ranges
            .iter()
            .flat_map(|&(start, end)| {
                (start as u64..=end as u64).map(|n| (Some(Ipv4Addr::from(n as u32)), ""))
            })
            .chain(cfg.domains.iter().map(|d| (d.ip, d.domain.as_str())))
            .flat_map(|(ip, domain)| {
                cfg.ports.iter().flat_map(move |&port| {
                    cfg.schemes
                        .iter()
                        .map(move |scheme| (ip, domain, port, scheme))
                })
            })
            .enumerate()
            .filter(|(i, _)| {
                i % cfg.workers == cfg.worker_index && !completed.contains(&(*i as u64))
            });
        let mut results = stream::iter(jobs)
            .map(|(job_id, (ip, domain, port, scheme))| {
                // Probes run on runtime threads so a full stdout pipe here cannot stall their I/O
                // and turn slow GUI consumption into false timeouts.
                let config = config.clone();
                let scheme = scheme.clone();
                let domain = domain.to_owned();
                tokio::spawn(async move {
                    WorkerRow {
                        job_id: job_id as u64,
                        record: crate::scan::probe(&config, ip, port, &scheme, &domain).await,
                    }
                })
            })
            .buffer_unordered(cfg.concurrency);
        let mut output = std::io::BufWriter::new(std::io::stdout().lock());
        while let Some(record) = results.next().await {
            serde_json::to_writer(&mut output, &record?)?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
        Ok::<_, anyhow::Error>(())
    })
}

/// Workers end every record with a newline, so a final unterminated line means the process was
/// killed mid-write. Unexpected exits are reported from the exit status, not from this fragment.
fn read_record(reader: &mut impl BufRead, line: &mut Vec<u8>) -> Result<Option<WorkerRow>> {
    line.clear();
    reader.read_until(b'\n', line)?;
    if line.last() != Some(&b'\n') {
        return Ok(None);
    }
    Ok(Some(serde_json::from_slice(line)?))
}

pub fn launch(
    config: Config,
    cancel: Arc<AtomicBool>,
    tx: SyncSender<Event>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut config = config;
        let prepared = crate::dns::prepare(&mut config, &cancel);
        if !config.domains.is_empty() {
            let _ = tx.send(Event::Prepared(config.domains.clone()));
        }
        if let Err(e) = prepared {
            if !cancel.load(Ordering::Relaxed) {
                let _ = tx.send(Event::Problem(format!("{e:#}")));
            }
            let _ = tx.send(Event::Done(true));
            return;
        }
        let mut children = Vec::new();
        let mut readers = Vec::new();
        let mut failed = false;
        for index in 0..config.workers {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let spawn = (|| -> Result<_> {
                let mut command = Command::new(std::env::current_exe()?);
                command
                    .arg("--worker")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    command.creation_flags(0x08000000);
                }
                let mut child = command.spawn().context("worker起動失敗")?;
                let mut cfg = config.clone();
                cfg.worker_index = index;
                let written = (|| -> Result<()> {
                    let mut stdin = child.stdin.take().context("worker stdinなし")?;
                    serde_json::to_writer(&mut stdin, &cfg)?;
                    Ok(())
                })();
                if let Err(e) = written {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(e);
                }
                Ok(child)
            })();
            match spawn {
                Ok(mut child) => {
                    let stdout = child.stdout.take().unwrap();
                    let stderr = child.stderr.take().unwrap();
                    let sender = tx.clone();
                    readers.push(std::thread::spawn(move || {
                        let mut stdout = BufReader::new(stdout);
                        let mut line = Vec::new();
                        loop {
                            match read_record(&mut stdout, &mut line) {
                                Ok(Some(row)) => {
                                    if sender
                                        .send(Event::Row(row.job_id, Box::new(row.record)))
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                                Ok(None) => break,
                                Err(e) => {
                                    let _ = sender.send(Event::Problem(format!("worker出力: {e}")));
                                    break;
                                }
                            }
                        }
                    }));
                    let sender = tx.clone();
                    readers.push(std::thread::spawn(move || {
                        // Read stderr concurrently so a full pipe cannot stall a worker.
                        for line in BufReader::new(stderr).lines().take(32).flatten() {
                            let _ = sender.send(Event::Problem(line));
                        }
                    }));
                    children.push(child);
                }
                Err(e) => {
                    failed = true;
                    let _ = tx.send(Event::Problem(format!("{e:#}")));
                    break;
                }
            }
        }
        let mut stopped = failed;
        loop {
            let cancelling = cancel.load(Ordering::Relaxed) || failed;
            let mut alive = false;
            for child in &mut children {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        if !status.success() && !cancelling {
                            failed = true;
                            let _ = tx.send(Event::Problem(format!("worker終了: {status}")));
                        }
                    }
                    Ok(None) => {
                        alive = true;
                        if cancelling {
                            let _ = child.kill();
                        }
                    }
                    Err(e) => {
                        failed = true;
                        let _ = child.kill();
                        let _ = child.wait();
                        let _ = tx.send(Event::Problem(e.to_string()));
                    }
                }
            }
            if cancelling {
                stopped = true;
            }
            if !alive {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        for reader in readers {
            let _ = reader.join();
        }
        let _ = tx.send(Event::Done(stopped || failed));
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn killed_worker_fragment_is_not_a_parse_error() {
        let mut input =
            std::io::Cursor::new(b"{\"job_id\":7,\"record\":{\"port\":80,\"timestamp_unix_ms\":1234567890123}}\n{\"job_id\":8,\"record\":{\"port\":8".to_vec());
        let mut line = Vec::new();
        let row = read_record(&mut input, &mut line).unwrap().unwrap();
        assert_eq!(row.record.port, 80);
        assert_eq!(row.job_id, 7);
        assert_eq!(row.record.timestamp_unix_ms, 1234567890123);
        assert!(read_record(&mut input, &mut line).unwrap().is_none());
        let mut input = std::io::Cursor::new(b"not json\n".to_vec());
        assert!(read_record(&mut input, &mut line).is_err());
    }
}
