use std::{sync::Arc, time::Duration};

use tokio_util::sync::CancellationToken;

use super::{
    command::Command,
    error::ProcessError,
    event::{ProcessEvent, TerminatedPayload},
    handle::ProcessHandle,
};

type Factory = Arc<dyn Fn() -> Command + Send + Sync>;
type EventHook = Arc<dyn Fn(SupervisorEvent) + Send + Sync>;
type ProcessEventHook = Arc<dyn Fn(ProcessEvent) + Send + Sync>;

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

fn time_entropy() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15);
    // SplitMix64 finalizer decorrelates consecutive calls and spreads their bits.
    let mut z = nanos.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
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

    pub(crate) fn delay_for(&self, attempt: u32) -> Duration {
        let base = self
            .initial
            .saturating_mul(2u32.saturating_pow(attempt.min(16)))
            .min(self.max);
        if !self.jitter {
            return base;
        }
        // Cheap deterministic-free jitter in [-25%, +25%] without a rand dependency.
        let base_ns = base.as_nanos().max(1) as u64;
        let span = base_ns / 2; // total width 50% => ±25%
        let offset = time_entropy() % span.max(1);
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

pub struct SupervisorBuilder {
    factory: Factory,
    policy: RestartPolicy,
    backoff: Backoff,
    readiness: ReadinessProbe,
    on_event: Option<EventHook>,
    on_process_event: Option<ProcessEventHook>,
    cancel_token: Option<CancellationToken>,
}

pub struct Supervisor {
    token: CancellationToken,
    current: Arc<tokio::sync::Mutex<Option<ProcessHandle>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Supervisor {
    pub fn builder<F>(factory: F) -> SupervisorBuilder
    where
        F: Fn() -> Command + Send + Sync + 'static,
    {
        SupervisorBuilder {
            factory: Arc::new(factory),
            policy: RestartPolicy::OnFailure { max_restarts: 5 },
            backoff: Backoff::exponential(Duration::from_secs(1), Duration::from_secs(30))
                .with_jitter(),
            readiness: ReadinessProbe::AliveAfter(Duration::from_millis(1500)),
            on_event: None,
            on_process_event: None,
            cancel_token: None,
        }
    }

    /// Cancels restarts, gracefully kills the current child, and waits for the
    /// supervision loop to end.
    pub async fn stop(self) -> Result<(), ProcessError> {
        self.token.cancel();
        let current = self.current.lock().await.take();
        if let Some(handle) = current {
            let _ = handle.graceful_kill().await;
        }
        let _ = self.task.await;
        Ok(())
    }
}

impl SupervisorBuilder {
    pub fn restart_policy(mut self, policy: RestartPolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn backoff(mut self, backoff: Backoff) -> Self {
        self.backoff = backoff;
        self
    }

    pub fn readiness(mut self, readiness: ReadinessProbe) -> Self {
        self.readiness = readiness;
        self
    }

    pub fn on_event(mut self, hook: impl Fn(SupervisorEvent) + Send + Sync + 'static) -> Self {
        self.on_event = Some(Arc::new(hook));
        self
    }

    pub fn on_process_event(mut self, hook: impl Fn(ProcessEvent) + Send + Sync + 'static) -> Self {
        self.on_process_event = Some(Arc::new(hook));
        self
    }

    pub fn cancel_token(mut self, token: CancellationToken) -> Self {
        self.cancel_token = Some(token);
        self
    }

    /// Starts the supervision loop.
    ///
    /// The first spawn happens before this method returns, so its failure is
    /// returned directly. Each successful spawn emits [`SupervisorEvent::Started`]
    /// and forwards all child [`ProcessEvent`] values to the process hook. A child
    /// surviving [`ReadinessProbe::AliveAfter`] emits [`SupervisorEvent::Ready`]
    /// and resets the restart attempt. Each exit emits [`SupervisorEvent::Exited`];
    /// exit code zero ends the loop without a restart. Other exits and later spawn
    /// failures consume the restart budget and emit [`SupervisorEvent::Restarting`]
    /// after the configured backoff, or [`SupervisorEvent::GaveUp`] when exhausted.
    /// Cancellation interrupts readiness or backoff, prevents further restarts,
    /// gracefully stops the current child, and ends with [`SupervisorEvent::Stopped`].
    pub async fn spawn(self) -> Result<Supervisor, ProcessError> {
        let token = self.cancel_token.unwrap_or_default().child_token();
        let current: Arc<tokio::sync::Mutex<Option<ProcessHandle>>> = Arc::default();
        let emit = {
            let hook = self.on_event.clone();
            move |event: SupervisorEvent| {
                if let Some(hook) = &hook {
                    hook(event);
                }
            }
        };

        let (first_handle, first_rx) = (self.factory)().spawn().await?;
        emit(SupervisorEvent::Started {
            pid: first_handle.pid(),
        });
        *current.lock().await = Some(first_handle);

        let factory = self.factory;
        let policy = self.policy;
        let backoff = self.backoff;
        let readiness = self.readiness;
        let on_process_event = self.on_process_event;
        let token_ = token.clone();
        let current_ = current.clone();

        let task = tokio::spawn(async move {
            let mut attempt = 0;
            let mut next_rx = Some(first_rx);

            loop {
                if let Some(mut rx) = next_rx.take() {
                    let ReadinessProbe::AliveAfter(ready_delay) = readiness;
                    let ready_at = tokio::time::Instant::now() + ready_delay;
                    let mut readiness_pending = true;
                    let mut cancelled = false;
                    let mut kill_task = None;

                    let payload = loop {
                        tokio::select! {
                            biased;
                            _ = token_.cancelled(), if !cancelled => {
                                cancelled = true;
                                readiness_pending = false;
                                if let Some(handle) = current_.lock().await.take() {
                                    kill_task = Some(tokio::spawn(async move {
                                        let _ = handle.graceful_kill().await;
                                    }));
                                }
                            }
                            maybe_event = rx.recv() => match maybe_event {
                                Some(event) => {
                                    if let Some(hook) = &on_process_event {
                                        hook(event.clone());
                                    }
                                    if let ProcessEvent::Terminated(payload) = event {
                                        break payload;
                                    }
                                }
                                None => break TerminatedPayload {
                                    code: None,
                                    signal: None,
                                },
                            },
                            _ = tokio::time::sleep_until(ready_at), if readiness_pending => {
                                readiness_pending = false;
                                let child_is_alive = current_
                                    .lock()
                                    .await
                                    .as_ref()
                                    .is_some_and(|handle| handle.terminated.borrow().is_none());
                                if child_is_alive && !token_.is_cancelled() {
                                    attempt = 0;
                                    emit(SupervisorEvent::Ready);
                                }
                            }
                        }
                    };

                    if let Some(task) = kill_task {
                        let _ = task.await;
                    }
                    current_.lock().await.take();
                    let clean_exit = payload.code == Some(0);
                    emit(SupervisorEvent::Exited(payload));

                    if cancelled || token_.is_cancelled() {
                        emit(SupervisorEvent::Stopped);
                        return;
                    }
                    if clean_exit || matches!(policy, RestartPolicy::Never) {
                        return;
                    }
                } else if token_.is_cancelled() {
                    emit(SupervisorEvent::Stopped);
                    return;
                }

                attempt += 1;
                let RestartPolicy::OnFailure { max_restarts } = policy else {
                    return;
                };
                if attempt > max_restarts {
                    emit(SupervisorEvent::GaveUp);
                    return;
                }

                let delay = backoff.delay_for(attempt - 1);
                emit(SupervisorEvent::Restarting { attempt, delay });
                tokio::select! {
                    biased;
                    _ = token_.cancelled() => {
                        emit(SupervisorEvent::Stopped);
                        return;
                    }
                    _ = tokio::time::sleep(delay) => {}
                }

                if token_.is_cancelled() {
                    emit(SupervisorEvent::Stopped);
                    return;
                }

                match (factory)().spawn().await {
                    Ok((handle, rx)) => {
                        let pid = handle.pid();
                        *current_.lock().await = Some(handle);
                        next_rx = Some(rx);
                        if !token_.is_cancelled() {
                            emit(SupervisorEvent::Started { pid });
                        }
                    }
                    Err(error) => {
                        tracing::error!("supervisor respawn failed: {error}");
                    }
                }
            }
        });

        Ok(Supervisor {
            token,
            current,
            task,
        })
    }
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
        let samples: Vec<_> = (0..1000).map(|_| b.delay_for(0)).collect();

        for d in &samples {
            assert!(
                *d >= Duration::from_secs(3) && *d <= Duration::from_secs(5),
                "{d:?}"
            );
        }
        assert!(samples.iter().min().unwrap() < &Duration::from_secs(4));
        assert!(samples.iter().max().unwrap() > &Duration::from_secs(4));
    }
}
