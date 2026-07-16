#![cfg(feature = "process")]

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
async fn pid_file_written_and_cleaned_up() {
    let dir = tempfile::tempdir().unwrap();
    let pid_path = dir.path().join("core.pid");
    let (handle, rx) = Command::new(child())
        .args(["exit-with", "0"])
        .pid_file(&pid_path)
        .spawn()
        .await
        .unwrap();
    // pid file exists right after spawn and contains the pid
    let content: u32 = std::fs::read_to_string(&pid_path)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(content, handle.pid());
    // drain to termination -> file removed
    let mut rx = rx;
    while rx.recv().await.is_some() {}
    assert!(!pid_path.exists());
}

#[tokio::test]
async fn residual_process_is_killed_before_spawn() {
    let dir = tempfile::tempdir().unwrap();
    let pid_path = dir.path().join("core.pid");

    let (h1, mut rx1) = Command::new(child())
        .args(["sleep-forever"])
        .pid_file(&pid_path)
        .spawn()
        .await
        .unwrap();
    loop {
        match rx1.recv().await.unwrap() {
            ProcessEvent::Stdout(l) if l.contains("ready") => break,
            ProcessEvent::Terminated(_) => panic!("exited early"),
            _ => {}
        }
    }
    let old_pid = h1.pid();

    // second spawn with the same pid file must kill the residual first
    let (h2, _rx2) = Command::new(child())
        .args(["sleep-forever"])
        .pid_file(&pid_path)
        .spawn()
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(!pid_alive(old_pid), "residual process not killed");
    let content: u32 = std::fs::read_to_string(&pid_path)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(content, h2.pid());
    h2.kill().await.unwrap();
}

#[tokio::test]
async fn abandonment_cleans_up_pid_file() {
    let dir = tempfile::tempdir().unwrap();
    let pid_path = dir.path().join("core.pid");
    let (handle, rx) = Command::new(child())
        .args(["sleep-forever"])
        .pid_file(&pid_path)
        .spawn()
        .await
        .unwrap();
    let pid: u32 = std::fs::read_to_string(&pid_path)
        .unwrap()
        .trim()
        .parse()
        .unwrap();

    drop(handle);
    drop(rx);

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while (pid_alive(pid) || pid_path.exists()) && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    assert!(!pid_alive(pid), "abandoned process still alive");
    assert!(
        !pid_path.exists(),
        "pid file still exists after abandonment"
    );
}
