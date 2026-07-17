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

#[cfg(unix)]
#[tokio::test]
async fn symlink_pid_file_is_rejected() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target.pid");
    let pid_path = dir.path().join("core.pid");
    std::fs::write(&target, std::process::id().to_string()).unwrap();
    symlink(&target, &pid_path).unwrap();

    let error = Command::new(child())
        .args(["exit-with", "0"])
        .pid_file(&pid_path)
        .spawn()
        .await
        .err()
        .expect("symlink pid file must fail spawn");
    assert!(matches!(
        error,
        nyanpasu_utils::process::ProcessError::Io(error)
            if error.kind() == std::io::ErrorKind::InvalidInput
    ));
}

#[tokio::test]
async fn unrelated_pid_is_not_killed_by_residual_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    let pid_path = dir.path().join("core.pid");
    let own_pid = std::process::id();
    std::fs::write(&pid_path, own_pid.to_string()).unwrap();

    let (handle, mut rx) = Command::new(child())
        .args(["exit-with", "0"])
        .pid_file(&pid_path)
        .spawn()
        .await
        .unwrap();
    while rx.recv().await.is_some() {}

    assert_eq!(handle.wait().await.unwrap().code, Some(0));
    assert!(pid_alive(own_pid), "pid validator killed the test process");
}
