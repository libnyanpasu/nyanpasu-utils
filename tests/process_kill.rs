#![cfg(feature = "process")]

use std::time::Duration;

use nyanpasu_utils::process::{Command, ProcessEvent};

fn child() -> &'static str {
    env!("CARGO_BIN_EXE_nyanpasu-test-child")
}

fn pid_alive(pid: u32) -> bool {
    use sysinfo::{Pid, ProcessRefreshKind, RefreshKind, System};
    let kind = RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing());
    let mut s = System::new_with_specifics(kind);
    s.refresh_specifics(kind);
    s.process(Pid::from_u32(pid)).is_some()
}

#[tokio::test]
async fn kill_terminates_and_wait_returns() {
    let (handle, mut rx) = Command::new(child())
        .args(["sleep-forever"])
        .spawn()
        .await
        .unwrap();
    // wait for "ready" so we know it's running
    loop {
        match rx.recv().await.unwrap() {
            ProcessEvent::Stdout(l) if l.contains("ready") => break,
            ProcessEvent::Terminated(_) => panic!("exited early"),
            _ => {}
        }
    }
    handle.kill().await.unwrap();
    let payload = handle.wait().await.unwrap();
    assert_ne!(payload.code, Some(0)); // hard kill is never a clean exit
    // killing again is idempotent-Ok
    handle.kill().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn graceful_kill_delivers_sigterm_first() {
    let (handle, mut rx) = Command::new(child())
        .args(["trap-term"])
        .spawn()
        .await
        .unwrap();
    loop {
        match rx.recv().await.unwrap() {
            ProcessEvent::Stdout(l) if l.contains("ready") => break,
            ProcessEvent::Terminated(_) => panic!("exited early"),
            _ => {}
        }
    }
    handle.graceful_kill().await.unwrap();
    // trap-term exits 0 on SIGTERM -> proves the graceful tier was delivered
    assert_eq!(handle.wait().await.unwrap().code, Some(0));
}

#[cfg(windows)]
#[tokio::test]
async fn graceful_kill_without_helper_falls_back_on_windows() {
    let (handle, mut rx) = Command::new(child())
        .args(["sleep-forever"])
        .spawn()
        .await
        .unwrap();
    ready(&mut rx).await;
    handle.graceful_kill().await.unwrap();
    assert_ne!(handle.wait().await.unwrap().code, Some(0));
}

#[cfg(windows)]
async fn ready(rx: &mut tokio::sync::mpsc::Receiver<ProcessEvent>) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match rx.recv().await.expect("ready event") {
                ProcessEvent::Stdout(line) if line == "ready" => break,
                ProcessEvent::Terminated(exit) => panic!("exited before ready: {exit:?}"),
                _ => {}
            }
        }
    })
    .await
    .expect("ready timeout");
}

#[cfg(windows)]
#[tokio::test]
async fn windows_helper_delivers_ctrlc_and_allows_drop() {
    let dir = tempfile::tempdir().unwrap();
    let cleanup = dir.path().join("cleanup");
    let helper = dir.path().join("nyanpasu-recording-shutdown-helper.exe");
    std::fs::copy(child(), &helper).unwrap();
    let (handle, mut rx) = Command::new(child())
        .arg("trap-ctrlc")
        .arg(&cleanup)
        .windows_shutdown_helper(&helper)
        .spawn()
        .await
        .unwrap();
    ready(&mut rx).await;
    tokio::time::timeout(Duration::from_secs(10), handle.graceful_kill())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(handle.wait().await.unwrap().code, Some(0));
    assert_eq!(std::fs::read_to_string(cleanup).unwrap(), "dropped");
    assert_eq!(
        std::fs::read_to_string(helper.with_extension("calls")).unwrap(),
        "called\n"
    );
    handle.graceful_kill().await.unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn windows_repeated_stop_is_bounded_when_ctrlc_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let cleanup = dir.path().join("cleanup");
    let grace = Duration::from_secs(1);
    let (handle, mut rx) = Command::new(child())
        .arg("ignore-ctrlc")
        .arg(&cleanup)
        .windows_shutdown_helper(child())
        .kill_grace(grace)
        .spawn()
        .await
        .unwrap();
    ready(&mut rx).await;
    let started = tokio::time::Instant::now();
    let mut signals = 0;
    let (first, second) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(handle.graceful_kill(), async {
            loop {
                match rx.recv().await.expect("Ctrl+C observation") {
                    ProcessEvent::Stdout(line) if line == "got-ctrlc" => {
                        signals += 1;
                        break;
                    }
                    ProcessEvent::Terminated(exit) => panic!("no Ctrl+C before exit: {exit:?}"),
                    _ => {}
                }
            }
            handle.graceful_kill().await
        })
    })
    .await
    .unwrap();
    first.unwrap();
    second.unwrap();
    assert!(
        started.elapsed() >= grace,
        "Ctrl+C was not given its grace period"
    );
    assert_ne!(handle.wait().await.unwrap().code, Some(0));
    assert!(!cleanup.exists(), "hard kill should not run Drop");
    while let Some(event) = rx.recv().await {
        if matches!(event, ProcessEvent::Stdout(ref line) if line == "got-ctrlc") {
            signals += 1;
        }
    }
    assert_eq!(signals, 1);
}

#[cfg(windows)]
#[tokio::test]
async fn windows_missing_helper_falls_back_without_waiting_for_grace() {
    let dir = tempfile::tempdir().unwrap();
    let (handle, mut rx) = Command::new(child())
        .arg("sleep-forever")
        .windows_shutdown_helper(dir.path().join("missing.exe"))
        .kill_grace(Duration::from_secs(30))
        .spawn()
        .await
        .unwrap();
    ready(&mut rx).await;
    tokio::time::timeout(Duration::from_secs(5), handle.graceful_kill())
        .await
        .unwrap()
        .unwrap();
    assert_ne!(handle.wait().await.unwrap().code, Some(0));
}

#[cfg(windows)]
#[tokio::test]
async fn windows_hung_helper_consumes_the_same_grace_and_is_reaped() {
    let dir = tempfile::tempdir().unwrap();
    let helper = dir.path().join("nyanpasu-hung-shutdown-helper.exe");
    std::fs::copy(child(), &helper).unwrap();
    let (handle, mut rx) = Command::new(child())
        .arg("sleep-forever")
        .windows_shutdown_helper(&helper)
        .kill_grace(Duration::from_secs(1))
        .spawn()
        .await
        .unwrap();
    ready(&mut rx).await;
    tokio::time::timeout(Duration::from_secs(5), handle.graceful_kill())
        .await
        .unwrap()
        .unwrap();
    assert_ne!(handle.wait().await.unwrap().code, Some(0));
    let helper_pid: u32 = std::fs::read_to_string(helper.with_extension("pid"))
        .unwrap()
        .parse()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while pid_alive(helper_pid) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("hung helper was not killed");
}

#[tokio::test]
async fn whole_tree_is_reaped() {
    let (handle, mut rx) = Command::new(child())
        .args(["spawn-grandchild"])
        .spawn()
        .await
        .unwrap();
    let grandchild_pid: u32 = loop {
        match rx.recv().await.unwrap() {
            ProcessEvent::Stdout(l) if l.contains("grandchild-pid:") => {
                break l
                    .trim()
                    .trim_start_matches("grandchild-pid:")
                    .parse()
                    .unwrap();
            }
            ProcessEvent::Terminated(_) => panic!("exited early"),
            _ => {}
        }
    };
    assert!(pid_alive(grandchild_pid));
    handle.kill().await.unwrap();
    handle.wait().await.unwrap();
    // the kernel object must have reaped the grandchild too
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!pid_alive(grandchild_pid), "grandchild survived tree kill");
}

#[cfg(windows)]
async fn quiescing_tree_stop(cancel_in_background: bool) {
    use nyanpasu_utils::process::{
        ProcessActivitySource, ProcessActivityState, Supervisor, SupervisorEvent,
    };
    use tokio_util::sync::CancellationToken;

    let dir = tempfile::tempdir().unwrap();
    let helper = dir.path().join("nyanpasu-recording-shutdown-helper.exe");
    std::fs::copy(child(), &helper).unwrap();
    let factory_helper = helper.clone();
    let source = ProcessActivitySource::new();
    let token = CancellationToken::new();
    let (output_tx, mut output_rx) = tokio::sync::mpsc::unbounded_channel();
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    let supervisor = Supervisor::builder(move || {
        Command::new(child())
            .arg("spawn-grandchild")
            .windows_shutdown_helper(&factory_helper)
    })
    .activity(source.subscribe())
    .cancel_token(token.clone())
    .on_process_event(move |event| {
        let _ = output_tx.send(event);
    })
    .on_event(move |event| {
        let _ = event_tx.send(event);
    })
    .spawn()
    .await
    .unwrap();
    let root_pid = match event_rx.recv().await.unwrap() {
        SupervisorEvent::Started { pid } => pid,
        event => panic!("expected Started, got {event:?}"),
    };
    let descendant = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match output_rx.recv().await.unwrap() {
                ProcessEvent::Stdout(line) if line.starts_with("grandchild-pid:") => {
                    break line
                        .trim_start_matches("grandchild-pid:")
                        .parse::<u32>()
                        .unwrap();
                }
                ProcessEvent::Terminated(exit) => {
                    panic!("tree exited before confirmation: {exit:?}")
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    source.set(ProcessActivityState::Quiescing);
    assert!(pid_alive(root_pid), "quiescing stopped a healthy child");
    assert!(pid_alive(descendant));
    if cancel_in_background {
        token.cancel();
        tokio::time::timeout(Duration::from_secs(10), async {
            while !matches!(event_rx.recv().await.unwrap(), SupervisorEvent::Stopped) {}
        })
        .await
        .unwrap();
    }
    tokio::time::timeout(Duration::from_secs(10), supervisor.stop())
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while pid_alive(root_pid) || pid_alive(descendant) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("process tree survived quiescing stop");
    assert!(
        !helper.with_extension("calls").exists(),
        "quiescing stop launched the helper"
    );
}

#[cfg(windows)]
#[tokio::test]
async fn quiescing_stop_skips_windows_helper_and_kills_descendants() {
    quiescing_tree_stop(false).await;
}

#[cfg(windows)]
#[tokio::test]
async fn quiescing_cancellation_skips_windows_helper_and_kills_descendants() {
    quiescing_tree_stop(true).await;
}
