use crate::{input, model::Config};
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
        let total = input::job_count(&config.ranges, config.ports.len(), config.schemes.len());
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
}
