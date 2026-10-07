//! Bounded retry timing. A short successful connection does not reset the budget.
use std::time::{Duration, Instant};

pub const AUTHORITY_TIMEOUT: Duration = Duration::from_secs(20);
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(45);
const STABLE_WINDOW: Duration = Duration::from_secs(60);
const MAX_ATTEMPTS: u32 = 5;

#[derive(Clone, Copy, Debug)]
pub struct Retry {
    pub generation: u64,
    pub at: Instant,
}
impl Retry {
    pub fn is_due(self, now: Instant, generation: u64) -> bool {
        self.generation == generation && now >= self.at
    }
}

#[derive(Default)]
pub struct RetryBudget {
    attempts: u32,
    healthy_since: Option<Instant>,
}
impl RetryBudget {
    pub fn schedule(&mut self, generation: u64, now: Instant) -> Option<Retry> {
        if self
            .healthy_since
            .take()
            .is_some_and(|since| now.saturating_duration_since(since) >= STABLE_WINDOW)
        {
            self.attempts = 0;
        }
        if self.attempts >= MAX_ATTEMPTS {
            return None;
        }
        let delay = Duration::from_secs(1 << self.attempts.min(4));
        self.attempts += 1;
        Some(Retry {
            generation,
            at: now + delay,
        })
    }
    pub fn healthy(&mut self, now: Instant) {
        self.healthy_since.get_or_insert(now);
    }
    pub fn attempts(&self) -> u32 {
        self.attempts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retries_back_off_and_exhaust() {
        let now = Instant::now();
        let mut budget = RetryBudget::default();
        for seconds in [1, 2, 4, 8, 16] {
            assert_eq!(
                budget.schedule(7, now).unwrap().at,
                now + Duration::from_secs(seconds)
            );
        }
        assert!(budget.schedule(7, now).is_none());
    }
    #[test]
    fn deadline_and_generation_both_guard_retry() {
        let now = Instant::now();
        let retry = RetryBudget::default().schedule(7, now).unwrap();
        assert!(!retry.is_due(now, 7));
        assert!(!retry.is_due(retry.at, 8));
        assert!(retry.is_due(retry.at, 7));
    }
    #[test]
    fn flapping_successes_do_not_reset_budget() {
        let mut budget = RetryBudget::default();
        let mut now = Instant::now();
        for _ in 0..MAX_ATTEMPTS {
            assert!(budget.schedule(0, now).is_some());
            budget.healthy(now);
            now += STABLE_WINDOW - Duration::from_secs(1);
        }
        assert!(budget.schedule(0, now).is_none());
    }
    #[test]
    fn only_stable_success_resets_budget() {
        let now = Instant::now();
        let mut budget = RetryBudget::default();
        for _ in 0..MAX_ATTEMPTS {
            budget.schedule(0, now).unwrap();
        }
        // Time spent disconnected does not reset an exhausted budget.
        assert!(budget.schedule(0, now + STABLE_WINDOW * 2).is_none());
        budget.healthy(now + STABLE_WINDOW * 2);
        let retry = budget.schedule(1, now + STABLE_WINDOW * 3).unwrap();
        assert_eq!(retry.at, now + STABLE_WINDOW * 3 + Duration::from_secs(1));
        assert_eq!(budget.attempts(), 1);
    }
}
