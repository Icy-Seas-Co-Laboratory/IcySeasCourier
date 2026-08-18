use rand::Rng;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub base: Duration,
    pub maximum: Duration,
    pub max_attempts: u32,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            base: Duration::from_millis(500),
            maximum: Duration::from_secs(30),
            max_attempts: 8,
        }
    }
}

impl RetryPolicy {
    pub fn delay(&self, attempt: u32) -> Duration {
        let exponential = self
            .base
            .saturating_mul(2_u32.saturating_pow(attempt.min(20)))
            .min(self.maximum);
        Duration::from_millis(rand::rng().random_range(0..=exponential.as_millis() as u64))
    }

    pub fn retries_http_status(status: u16) -> bool {
        matches!(status, 408 | 429 | 500 | 502 | 503 | 504)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn classifies_transient_statuses() {
        assert!(RetryPolicy::retries_http_status(503));
        assert!(!RetryPolicy::retries_http_status(401));
    }
}
