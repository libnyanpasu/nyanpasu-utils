#![cfg(feature = "process")]

use nyanpasu_utils::process::{Command, ProcessError, ProcessEvent};

fn child() -> &'static str {
    env!("CARGO_BIN_EXE_nyanpasu-test-child")
}

#[tokio::test]
async fn write_stdin_roundtrip() {
    let (handle, mut rx) = Command::new(child())
        .args(["echo-stdin"])
        .pipe_stdin(true)
        .spawn()
        .await
        .unwrap();
    handle.write_stdin(b"ping\n").await.unwrap();
    let mut echoed = None;
    while let Some(e) = rx.recv().await {
        if let ProcessEvent::Stdout(l) = e {
            echoed = Some(l);
            break;
        }
    }
    assert_eq!(echoed.unwrap().trim(), "echo:ping");
}

#[tokio::test]
async fn write_stdin_without_pipe_is_error() {
    let (handle, _rx) = Command::new(child())
        .args(["sleep-forever"])
        .spawn()
        .await
        .unwrap();
    let err = handle.write_stdin(b"x").await.err().unwrap();
    assert!(matches!(err, ProcessError::StdinUnavailable));
    handle.kill().await.unwrap();
}

#[tokio::test]
async fn write_stdin_does_not_stall_output_pump() {
    let (handle, mut rx) = Command::new(child())
        .args(["spam-stdout", "10000"])
        .pipe_stdin(true)
        .event_channel_capacity(8)
        .spawn()
        .await
        .unwrap();

    handle.write_stdin(b"x\n").await.unwrap();

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ProcessEvent::Stdout(_)))
            .count(),
        10000
    );
    assert!(matches!(events.last(), Some(ProcessEvent::Terminated(_))));
}
