#![cfg(feature = "process")]

use std::time::Duration;

use nyanpasu_utils::process::{Command, ProcessEvent};

fn child() -> &'static str {
    env!("CARGO_BIN_EXE_nyanpasu-test-child")
}

async fn collect_all(mut rx: tokio::sync::mpsc::Receiver<ProcessEvent>) -> Vec<ProcessEvent> {
    let mut evs = Vec::new();
    while let Some(e) = rx.recv().await {
        evs.push(e);
    }
    evs
}

fn pid_alive(pid: u32) -> bool {
    use sysinfo::{Pid, ProcessRefreshKind, RefreshKind, System};
    let kind = RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing());
    let mut system = System::new_with_specifics(kind);
    system.refresh_specifics(kind);
    system.process(Pid::from_u32(pid)).is_some()
}

#[tokio::test]
async fn stdout_stderr_then_terminated_last() {
    let (handle, rx) = Command::new(child())
        .args(["echo-lines", "hello", "world"])
        .spawn()
        .await
        .unwrap();
    assert!(handle.pid() > 0);
    let evs = collect_all(rx).await;

    let stdout: Vec<_> = evs
        .iter()
        .filter_map(|e| match e {
            ProcessEvent::Stdout(l) => Some(l.trim_end().to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(stdout, vec!["hello", "world"]);
    assert!(
        evs.iter()
            .any(|e| matches!(e, ProcessEvent::Stderr(l) if l.contains("stderr-marker")))
    );
    // contract: Terminated is the FINAL event
    assert!(matches!(evs.last().unwrap(), ProcessEvent::Terminated(p) if p.code == Some(0)));
    let payload = handle.wait().await.unwrap();
    assert_eq!(payload.code, Some(0));
}

#[tokio::test]
async fn nonzero_exit_code_is_reported() {
    let (handle, rx) = Command::new(child())
        .args(["exit-with", "3"])
        .spawn()
        .await
        .unwrap();
    let evs = collect_all(rx).await;
    assert!(matches!(evs.last().unwrap(), ProcessEvent::Terminated(p) if p.code == Some(3)));
    assert_eq!(handle.wait().await.unwrap().code, Some(3));
}

#[tokio::test]
async fn spam_10k_lines_no_loss_with_default_capacity() {
    let (_handle, rx) = Command::new(child())
        .args(["spam-stdout", "10000"])
        .spawn()
        .await
        .unwrap();
    let evs = collect_all(rx).await;
    let n = evs
        .iter()
        .filter(|e| matches!(e, ProcessEvent::Stdout(_)))
        .count();
    assert_eq!(n, 10000);
    assert!(matches!(evs.last().unwrap(), ProcessEvent::Terminated(_)));
}

#[tokio::test]
async fn spawn_missing_program_is_error() {
    let err = Command::new("definitely-not-a-real-binary-42")
        .spawn()
        .await
        .err()
        .unwrap();
    let msg = err.to_string();
    assert!(!msg.is_empty());
}

#[tokio::test]
async fn containment_matches_platform() {
    use nyanpasu_utils::process::Containment;
    let (handle, rx) = Command::new(child())
        .args(["exit-with", "0"])
        .spawn()
        .await
        .unwrap();
    let c = handle.containment();
    #[cfg(windows)]
    assert_eq!(c, Containment::JobObject);
    #[cfg(target_os = "linux")]
    assert!(matches!(
        c,
        Containment::CgroupV2 | Containment::ProcessGroup
    ));
    #[cfg(all(unix, not(target_os = "linux")))]
    assert_eq!(c, Containment::ProcessGroup);
    collect_all(rx).await;
}

#[tokio::test]
async fn spawn_timeout_kills_the_whole_tree() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let (handle, mut rx) = Command::new(child())
            .args(["spawn-grandchild"])
            .timeout(Duration::from_millis(500))
            .spawn()
            .await
            .unwrap();
        let mut grandchild_pid = None;
        let mut events = Vec::new();

        while let Some(event) = rx.recv().await {
            if let ProcessEvent::Stdout(line) = &event
                && let Some(pid) = line.trim().strip_prefix("grandchild-pid:")
            {
                grandchild_pid = Some(pid.parse::<u32>().unwrap());
            }
            events.push(event);
        }

        assert!(matches!(events.last(), Some(ProcessEvent::Terminated(_))));
        handle.wait().await.unwrap();

        let grandchild_pid = grandchild_pid.expect("grandchild pid event");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while pid_alive(grandchild_pid) && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            !pid_alive(grandchild_pid),
            "grandchild survived spawn timeout"
        );
    })
    .await
    .expect("spawn timeout teardown hung");
}

#[tokio::test]
async fn kill_completes_when_event_receiver_stops_draining() {
    let (handle, _rx) = Command::new(child())
        .args(["spam-stdout", "1000"])
        .event_channel_capacity(1)
        .spawn()
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;
    tokio::time::timeout(Duration::from_secs(30), handle.kill())
        .await
        .expect("kill hung behind event backpressure")
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), handle.wait())
        .await
        .expect("wait hung after kill")
        .unwrap();
}
