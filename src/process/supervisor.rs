use std::time::Duration;

use super::event::TerminatedPayload;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartPolicy {
    Never,
    OnFailure { max_restarts: u32 },
}

#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    initial: Duration,
    max: Duration,
    jitter: bool,
}

impl Backoff {
    pub fn exponential(initial: Duration, max: Duration) -> Self {
        Self {
            initial,
            max,
            jitter: false,
        }
    }

    pub fn with_jitter(mut self) -> Self {
        self.jitter = true;
        self
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "used by the Supervisor runtime in Task 12")
    )]
    pub(crate) fn delay_for(&self, attempt: u32) -> Duration {
        let base = self
            .initial
            .saturating_mul(2u32.saturating_pow(attempt.min(16)))
            .min(self.max);
        if !self.jitter {
            return base;
        }
        // Cheap deterministic-free jitter in [-25%, +25%] without a rand dependency.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0) as u64;
        let base_ns = base.as_nanos().max(1) as u64;
        let span = base_ns / 2; // total width 50% => ±25%
        let offset = nanos % span.max(1);
        Duration::from_nanos(base_ns - span / 2 + offset)
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Copy)]
pub enum ReadinessProbe {
    /// Emit `Ready` if the child is still alive after this delay
    /// (successor of the legacy 1.5s `DelayCheckpointPass`, design §5.3).
    AliveAfter(Duration),
}

#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum SupervisorEvent {
    Started { pid: u32 },
    Ready,
    Exited(TerminatedPayload),
    Restarting { attempt: u32, delay: Duration },
    GaveUp,
    Stopped,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn backoff_doubles_and_caps() {
        let b = Backoff::exponential(Duration::from_secs(1), Duration::from_secs(30));
        assert_eq!(b.delay_for(0), Duration::from_secs(1));
        assert_eq!(b.delay_for(1), Duration::from_secs(2));
        assert_eq!(b.delay_for(4), Duration::from_secs(16));
        assert_eq!(b.delay_for(10), Duration::from_secs(30)); // capped
    }

    #[test]
    fn jitter_stays_within_25_percent() {
        let b = Backoff::exponential(Duration::from_secs(4), Duration::from_secs(60)).with_jitter();
        for _ in 0..100 {
            let d = b.delay_for(0);
            assert!(
                d >= Duration::from_secs(3) && d <= Duration::from_secs(5),
                "{d:?}"
            );
        }
    }
}
