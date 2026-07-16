#![cfg(feature = "process")]

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use nyanpasu_utils::process::{
    Backoff, Command, ReadinessProbe, RestartPolicy, Supervisor, SupervisorEvent,
};

fn child() -> &'static str {
    env!("CARGO_BIN_EXE_nyanpasu-test-child")
}

#[derive(Clone, Default)]
struct EventLog(Arc<Mutex<Vec<SupervisorEvent>>>);

impl EventLog {
    fn push(&self, e: SupervisorEvent) {
        self.0.lock().unwrap().push(e);
    }
    fn snapshot(&self) -> Vec<SupervisorEvent> {
        self.0.lock().unwrap().clone()
    }
    async fn wait_for(&self, pred: impl Fn(&[SupervisorEvent]) -> bool, timeout: Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if pred(&self.snapshot()) {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timeout; log = {:?}",
                self.snapshot()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

#[tokio::test]
async fn restarts_on_failure_then_gives_up() {
    let log = EventLog::default();
    let log2 = log.clone();
    let _sup = Supervisor::builder(|| Command::new(child()).args(["exit-with", "1"]))
        .restart_policy(RestartPolicy::OnFailure { max_restarts: 2 })
        .backoff(Backoff::exponential(
            Duration::from_millis(10),
            Duration::from_millis(40),
        ))
        .readiness(ReadinessProbe::AliveAfter(Duration::from_millis(5000))) // never ready
        .on_event(move |e| log2.push(e))
        .spawn()
        .await
        .unwrap();

    log.wait_for(
        |evs| evs.iter().any(|e| matches!(e, SupervisorEvent::GaveUp)),
        Duration::from_secs(10),
    )
    .await;
    let evs = log.snapshot();
    let starts = evs
        .iter()
        .filter(|e| matches!(e, SupervisorEvent::Started { .. }))
        .count();
    let restarts = evs
        .iter()
        .filter(|e| matches!(e, SupervisorEvent::Restarting { .. }))
        .count();
    assert_eq!(starts, 3, "initial + 2 restarts, log = {evs:?}");
    assert_eq!(restarts, 2);
    assert!(!evs.iter().any(|e| matches!(e, SupervisorEvent::Ready)));
}

#[tokio::test]
async fn ready_emitted_and_stop_is_clean() {
    let log = EventLog::default();
    let log2 = log.clone();
    let sup = Supervisor::builder(|| Command::new(child()).args(["sleep-forever"]))
        .readiness(ReadinessProbe::AliveAfter(Duration::from_millis(100)))
        .on_event(move |e| log2.push(e))
        .spawn()
        .await
        .unwrap();

    log.wait_for(
        |evs| evs.iter().any(|e| matches!(e, SupervisorEvent::Ready)),
        Duration::from_secs(5),
    )
    .await;
    sup.stop().await.unwrap();
    let evs = log.snapshot();
    assert!(
        matches!(evs.last().unwrap(), SupervisorEvent::Stopped),
        "log = {evs:?}"
    );
    // no restart after stop
    assert!(
        !evs.iter()
            .any(|e| matches!(e, SupervisorEvent::Restarting { .. }))
    );
}

#[tokio::test]
async fn clean_exit_does_not_restart() {
    let log = EventLog::default();
    let log2 = log.clone();
    let _sup = Supervisor::builder(|| Command::new(child()).args(["exit-with", "0"]))
        .restart_policy(RestartPolicy::OnFailure { max_restarts: 3 })
        .on_event(move |e| log2.push(e))
        .spawn()
        .await
        .unwrap();
    log.wait_for(
        |evs| evs.iter().any(|e| matches!(e, SupervisorEvent::Exited(_))),
        Duration::from_secs(5),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let evs = log.snapshot();
    assert_eq!(
        evs.iter()
            .filter(|e| matches!(e, SupervisorEvent::Started { .. }))
            .count(),
        1
    );
    assert!(
        !evs.iter()
            .any(|e| matches!(e, SupervisorEvent::Restarting { .. }))
    );
}

#[tokio::test]
async fn first_spawn_failure_is_error() {
    let r = Supervisor::builder(|| Command::new("definitely-not-a-real-binary-42"))
        .spawn()
        .await;
    assert!(r.is_err());
}
