//! When to restart a persona, how long to wait, and when to stop calling it
//! a restart.
//!
//! The decision is pure and lives here; the process wiring is in
//! `main.rs`. Two operator stories meet in it and they pull in opposite
//! directions:
//!
//! - *"a crashed persona is restarted with backoff, so one bug does not
//!   silence my assistant"* — so a persona that ran, worked and then died
//!   comes back, quickly, and forever;
//! - *"a runtime that cannot start something must not be indistinguishable
//!   from one that is still starting it"* — so a persona whose image does
//!   not run must be **announced** rather than retried quietly in a loop
//!   that looks, from the outside, like a slow boot.
//!
//! [`Restarts`] separates them with one fact: how long the process stayed
//! up. A run shorter than `healthy_after` never really started, and
//! `start_failures` of those in a row is a failure the operator is told
//! about — once, at error level, with the word `failed` in it. The runtime
//! keeps retrying afterwards (an operator who fixes the image should not
//! also have to restart Hermes), but it has stopped pretending.

use std::time::Duration;

use crate::config::RestartPolicy;

/// What to do after one run of one persona ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Restart {
    /// How long to wait before starting it again.
    pub delay: Duration,
    /// How many runs in a row have now failed to stay up.
    pub consecutive_failures: u32,
    /// True exactly once per failure episode: on the run that reaches
    /// `start_failures`. It is the moment the runtime stops reporting
    /// "restarting" and reports "failed", so it must not be reported on
    /// every subsequent attempt — an error repeated every backoff interval
    /// is noise, and noise is how a real failure gets missed.
    pub announce_failed: bool,
}

/// The restart state of one persona.
#[derive(Debug, Clone)]
pub struct Restarts {
    policy: RestartPolicy,
    consecutive_failures: u32,
    announced: bool,
}

impl Restarts {
    pub fn new(policy: RestartPolicy) -> Self {
        Self {
            policy,
            consecutive_failures: 0,
            announced: false,
        }
    }

    /// Records that a run ended after `ran_for`, and says what happens next.
    ///
    /// A run that could not be spawned at all — no such image, no such
    /// binary — is recorded as a run of zero duration: from the operator's
    /// side it is the same event, a persona that is not there.
    pub fn record_run(&mut self, ran_for: Duration) -> Restart {
        if ran_for >= self.policy.healthy_after {
            // It ran. Whatever killed it is a crash, not a bad image: the
            // count resets, the next restart is immediate-ish, and a later
            // failure episode gets announced again.
            self.consecutive_failures = 0;
            self.announced = false;
            return Restart {
                delay: self.policy.base_backoff,
                consecutive_failures: 0,
                announce_failed: false,
            };
        }
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        let announce_failed =
            !self.announced && self.consecutive_failures >= self.policy.start_failures;
        if announce_failed {
            self.announced = true;
        }
        Restart {
            delay: self.delay_for(self.consecutive_failures),
            consecutive_failures: self.consecutive_failures,
            announce_failed,
        }
    }

    /// Whether this persona has already been announced as failed: what the
    /// runtime reports about it until it stays up again.
    pub fn has_failed(&self) -> bool {
        self.announced
    }

    /// Exponential, doubling per consecutive failure, capped. Saturating
    /// rather than shifting, so a persona that has been down for a week
    /// does not overflow its way back to an instant retry.
    fn delay_for(&self, consecutive_failures: u32) -> Duration {
        let base = self.policy.base_backoff.as_millis() as u64;
        let doubled = base.saturating_mul(1u64 << consecutive_failures.saturating_sub(1).min(32));
        Duration::from_millis(doubled).min(self.policy.max_backoff)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> RestartPolicy {
        RestartPolicy {
            base_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_millis(800),
            healthy_after: Duration::from_millis(5_000),
            start_failures: 3,
        }
    }

    #[test]
    fn a_persona_that_ran_and_crashed_comes_straight_back() {
        let mut restarts = Restarts::new(policy());
        let restart = restarts.record_run(Duration::from_secs(600));
        assert_eq!(restart.delay, Duration::from_millis(100));
        assert_eq!(restart.consecutive_failures, 0);
        assert!(!restart.announce_failed);
        assert!(!restarts.has_failed());
    }

    #[test]
    fn a_persona_that_never_stays_up_backs_off_and_is_announced_once() {
        let mut restarts = Restarts::new(policy());
        let first = restarts.record_run(Duration::ZERO);
        assert_eq!(first.delay, Duration::from_millis(100));
        assert!(!first.announce_failed, "one failure is not yet a verdict");

        let second = restarts.record_run(Duration::from_millis(10));
        assert_eq!(second.delay, Duration::from_millis(200));
        assert!(!second.announce_failed);

        let third = restarts.record_run(Duration::from_millis(10));
        assert_eq!(third.delay, Duration::from_millis(400));
        assert_eq!(third.consecutive_failures, 3);
        assert!(
            third.announce_failed,
            "at start_failures the runtime says failed, not restarting"
        );
        assert!(restarts.has_failed());

        let fourth = restarts.record_run(Duration::ZERO);
        assert!(
            !fourth.announce_failed,
            "the verdict is announced once per episode, not once per attempt"
        );
        assert_eq!(fourth.delay, Duration::from_millis(800));
    }

    #[test]
    fn the_backoff_is_capped() {
        let mut restarts = Restarts::new(policy());
        for _ in 0..40 {
            restarts.record_run(Duration::ZERO);
        }
        assert_eq!(
            restarts.record_run(Duration::ZERO).delay,
            Duration::from_millis(800),
            "a persona down for a long time retries at the cap, not instantly"
        );
    }

    #[test]
    fn a_persona_that_comes_back_clears_its_verdict() {
        let mut restarts = Restarts::new(policy());
        for _ in 0..3 {
            restarts.record_run(Duration::ZERO);
        }
        assert!(restarts.has_failed());

        restarts.record_run(Duration::from_secs(600));
        assert!(
            !restarts.has_failed(),
            "an operator who fixed the image gets a runtime that says so"
        );
        assert!(
            restarts.record_run(Duration::ZERO).delay == Duration::from_millis(100),
            "and a fresh backoff"
        );
    }
}
