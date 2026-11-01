use tokio::sync::watch;

/// Controls whether supervised processes may restart after a failure.
#[non_exhaustive]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ProcessActivityState {
    /// Automatic failure recovery and graceful Windows stops are enabled.
    #[default]
    Active,
    /// Failure recovery is paused and Windows stops kill the existing tree directly.
    Quiescing,
}

/// Publishes activity changes to readers owned by one process host.
#[derive(Debug, Clone)]
pub struct ProcessActivitySource {
    state: watch::Sender<ProcessActivityState>,
}

/// Reads the current activity and waits for changes without publishing updates.
#[derive(Debug, Clone)]
pub struct ProcessActivity {
    state: watch::Receiver<ProcessActivityState>,
}

impl Default for ProcessActivitySource {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessActivitySource {
    /// Creates an independent source initialized to [`ProcessActivityState::Active`].
    pub fn new() -> Self {
        Self {
            state: watch::channel(ProcessActivityState::Active).0,
        }
    }

    /// Retains the new state, waking readers only when the value changes.
    pub fn set(&self, state: ProcessActivityState) {
        self.state.send_if_modified(|current| {
            if *current == state {
                return false;
            }
            *current = state;
            true
        });
    }

    /// Subscribes to the latest retained state.
    pub fn subscribe(&self) -> ProcessActivity {
        ProcessActivity {
            state: self.state.subscribe(),
        }
    }
}

impl ProcessActivity {
    /// Returns the latest state without consuming a change notification.
    pub fn state(&self) -> ProcessActivityState {
        *self.state.borrow()
    }

    /// Waits for an unseen change and returns the latest state.
    /// If all sources are dropped, retains the last state and waits indefinitely.
    pub async fn changed(&mut self) -> ProcessActivityState {
        if self.state.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
        *self.state.borrow_and_update()
    }
}

pub(super) fn requires_tree_kill(activity: Option<&ProcessActivity>, windows: bool) -> bool {
    windows && activity.is_some_and(|activity| activity.state() == ProcessActivityState::Quiescing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::FutureExt;

    #[test]
    fn windows_quiescing_stop_uses_tree_kill_and_active_keeps_helper() {
        let source = ProcessActivitySource::new();
        let reader = source.subscribe();
        assert!(!requires_tree_kill(None, true));
        assert!(!requires_tree_kill(Some(&reader), true));
        source.set(ProcessActivityState::Quiescing);
        assert!(requires_tree_kill(Some(&reader), true));
        assert!(!requires_tree_kill(Some(&reader), false));
        source.set(ProcessActivityState::Active);
        assert!(!requires_tree_kill(Some(&reader), true));
    }

    #[tokio::test]
    async fn retains_updates_before_subscribing_and_isolates_sources() {
        let source = ProcessActivitySource::new();
        source.set(ProcessActivityState::Quiescing);
        assert_eq!(source.subscribe().state(), ProcessActivityState::Quiescing);
        assert_eq!(
            ProcessActivitySource::new().subscribe().state(),
            ProcessActivityState::Active
        );
    }

    #[tokio::test]
    async fn only_changes_wake_readers_and_revocation_is_retained() {
        let source = ProcessActivitySource::new();
        let mut reader = source.subscribe();
        source.set(ProcessActivityState::Active);
        assert!(reader.changed().now_or_never().is_none());
        source.set(ProcessActivityState::Quiescing);
        assert_eq!(
            reader.changed().now_or_never(),
            Some(ProcessActivityState::Quiescing)
        );
        source.set(ProcessActivityState::Quiescing);
        assert!(reader.changed().now_or_never().is_none());
        source.set(ProcessActivityState::Active);
        assert_eq!(reader.changed().await, ProcessActivityState::Active);
        drop(source);
        assert_eq!(reader.state(), ProcessActivityState::Active);
        assert!(reader.changed().now_or_never().is_none());
    }
}
