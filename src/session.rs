use crate::model::{Config, DomainTarget};
use std::collections::HashSet;

/// A scan plan is immutable until completion or explicit reset. Completion is acknowledged
/// only after a full result is retained, so killed/in-flight probes remain resumable.
pub struct ScanSession {
    config: Config,
    completed: HashSet<u64>,
    pub total: u64,
}

impl ScanSession {
    pub fn new(mut config: Config) -> Self {
        config.completed_jobs.clear();
        let total = config.job_count();
        Self {
            config,
            completed: HashSet::new(),
            total,
        }
    }

    pub fn accept(&mut self, job_id: u64) -> bool {
        job_id < self.total && self.completed.insert(job_id)
    }

    pub fn completed(&self) -> u64 {
        self.completed.len() as u64
    }
    pub fn pending(&self) -> bool {
        self.completed() < self.total
    }

    pub fn pin_domains(&mut self, domains: Vec<DomainTarget>) {
        if self
            .config
            .domains
            .iter()
            .map(|d| &d.domain)
            .eq(domains.iter().map(|d| &d.domain))
        {
            self.config.domains = domains;
        }
    }

    pub fn resume_config(&self) -> Config {
        let mut config = self.config.clone();
        config.completed_jobs = self.completed.iter().copied().collect();
        config.completed_jobs.sort_unstable();
        config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resumes_only_unacknowledged_jobs_after_multiple_stops() {
        let config = Config {
            ranges: vec![(0x7f000001, 0x7f000002)],
            ports: vec![80, 443],
            schemes: vec!["http".into(), "https".into()],
            concurrency: 2,
            timeout_secs: 1,
            body_limit: 1024,
            insecure: true,
            host: String::new(),
            workers: 2,
            worker_index: 0,
            completed_jobs: vec![],
            domains: vec![],
            resolve_domains: false,
        };
        let mut session = ScanSession::new(config);
        assert_eq!(session.total, 8);
        assert!(session.pending());
        for id in [5, 1, 7] {
            assert!(session.accept(id));
        }
        assert!(!session.accept(5));
        assert!(!session.accept(8));
        assert_eq!(session.completed(), 3);
        assert_eq!(session.resume_config().completed_jobs, vec![1, 5, 7]);
        assert!(session.accept(0));
        assert_eq!(session.resume_config().completed_jobs, vec![0, 1, 5, 7]);
        // Jobs not acknowledged (including in-flight/partially written responses) are retried.
        for id in [2, 3, 4, 6] {
            assert!(session.accept(id));
        }
        assert!(!session.pending());
        assert_eq!(session.completed(), 8);
    }

    #[test]
    fn domain_pins_do_not_change_resume_job_ids() {
        let cfg = Config {
            ranges: vec![(0x7f000001, 0x7f000001)],
            ports: vec![80],
            schemes: vec!["http".into(), "https".into()],
            concurrency: 1,
            timeout_secs: 2,
            body_limit: 1024,
            insecure: true,
            host: String::new(),
            worker_index: 0,
            workers: 1,
            completed_jobs: vec![],
            domains: vec![DomainTarget {
                domain: "localhost".into(),
                ip: None,
            }],
            resolve_domains: true,
        };
        let mut session = ScanSession::new(cfg);
        assert_eq!(session.total, 4);
        assert!(session.accept(2));
        session.pin_domains(vec![DomainTarget {
            domain: "localhost".into(),
            ip: Some(std::net::Ipv4Addr::LOCALHOST),
        }]);
        let cfg = session.resume_config();
        assert_eq!(cfg.domains[0].ip, Some(std::net::Ipv4Addr::LOCALHOST));
        assert_eq!(cfg.completed_jobs, vec![2]);
        assert_eq!(cfg.job_count(), 4);
    }
}
