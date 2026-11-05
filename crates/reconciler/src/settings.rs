use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconcilerSettings {
    pub initial_backoff: Duration,
    pub maximum_backoff: Duration,
    pub periodic_resynchronization: Duration,
    pub resynchronization_spread: Duration,
}

impl Default for ReconcilerSettings {
    fn default() -> Self {
        Self {
            initial_backoff: Duration::from_millis(100),
            maximum_backoff: Duration::from_secs(30),
            periodic_resynchronization: Duration::from_secs(30),
            resynchronization_spread: Duration::ZERO,
        }
    }
}

const PHASES: u32 = 1_000;

impl ReconcilerSettings {
    #[must_use]
    pub fn resynchronization_period(&self) -> Duration {
        self.periodic_resynchronization + self.resynchronization_spread
    }

    #[must_use]
    pub fn spread_by(self, spread: usize) -> Self {
        let phase = u32::try_from(spread % PHASES as usize).unwrap_or_default();
        Self { resynchronization_spread: self.periodic_resynchronization * phase / PHASES, ..self }
    }
}

impl ReconcilerSettings {
    #[must_use]
    pub fn backoff_after(&self, consecutive_failures: u32) -> Duration {
        let exponent = consecutive_failures.saturating_sub(1).min(20);
        self.initial_backoff.saturating_mul(1 << exponent).min(self.maximum_backoff)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_up_to_the_maximum() {
        let settings = ReconcilerSettings::default();
        assert_eq!(settings.backoff_after(1), Duration::from_millis(100));
        assert_eq!(settings.backoff_after(2), Duration::from_millis(200));
        assert_eq!(settings.backoff_after(4), Duration::from_millis(800));
        assert_eq!(settings.backoff_after(30), Duration::from_secs(30));
        assert_eq!(settings.backoff_after(u32::MAX), Duration::from_secs(30));
    }
}
