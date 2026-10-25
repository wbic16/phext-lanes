//! In-memory limits for sign-in and writes.
//!
//! A window is ten minutes. Auth attempts are counted per address. Writes
//! are counted per address and per signed-in account. The nginx site adds
//! a coarser request cap in front of this.

use std::sync::Mutex;
use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Copy)]
pub enum Bucket {
    AuthIp,
    WriteIp,
    WriteAccount,
}

struct Hit {
    key: String,
    at: Instant,
}

pub struct Limiter {
    auth_per_ip: usize,
    writes_per_ip: usize,
    writes_per_account: usize,
    hits: Mutex<Vec<Hit>>,
}

impl Limiter {
    pub fn standard() -> Self {
        Self::from_limits(&crate::config::Limits::default())
    }

    /// The rate caps from the server's configured limits.
    pub fn from_limits(limits: &crate::config::Limits) -> Self {
        Self::with_caps(
            limits.auth_per_ip,
            limits.writes_per_ip,
            limits.writes_per_account,
        )
    }

    pub fn with_caps(auth_per_ip: usize, writes_per_ip: usize, writes_per_account: usize) -> Self {
        Self {
            auth_per_ip,
            writes_per_ip,
            writes_per_account,
            hits: Mutex::new(Vec::new()),
        }
    }

    #[allow(clippy::result_unit_err)]
    pub fn hit(&self, bucket: Bucket, id: &str) -> Result<(), ()> {
        let (prefix, limit) = match bucket {
            Bucket::AuthIp => ("a", self.auth_per_ip),
            Bucket::WriteIp => ("w", self.writes_per_ip),
            Bucket::WriteAccount => ("u", self.writes_per_account),
        };
        let key = format!("{prefix}:{id}");
        let mut hits = self.hits.lock().map_err(|_| ())?;
        let now = Instant::now();
        hits.retain(|hit| now.saturating_duration_since(hit.at) < WINDOW);
        let count = hits.iter().filter(|hit| hit.key == key).count();
        if count >= limit {
            return Err(());
        }
        hits.push(Hit { key, at: now });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_third_attempt_in_a_tight_cap_is_refused() {
        let limiter = Limiter::with_caps(2, 2, 2);
        assert!(limiter.hit(Bucket::AuthIp, "203.0.113.8").is_ok());
        assert!(limiter.hit(Bucket::AuthIp, "203.0.113.8").is_ok());
        assert!(limiter.hit(Bucket::AuthIp, "203.0.113.8").is_err());
        assert!(limiter.hit(Bucket::AuthIp, "203.0.113.9").is_ok());
        assert!(limiter.hit(Bucket::WriteAccount, "github:1").is_ok());
        assert!(limiter.hit(Bucket::WriteAccount, "github:2").is_ok());
    }
}
