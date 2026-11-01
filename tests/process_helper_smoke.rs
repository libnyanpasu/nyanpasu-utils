//! Sanity checks for the test helper binary itself (std::process, no process feature needed).

fn child() -> &'static str {
    env!("CARGO_BIN_EXE_nyanpasu-test-child")
}

#[test]
fn exit_with_propagates_code() {
    let st = std::process::Command::new(child())
        .args(["exit-with", "3"])
        .status()
        .unwrap();
    assert_eq!(st.code(), Some(3));
}

#[test]
fn echo_lines_writes_stdout_and_stderr() {
    let out = std::process::Command::new(child())
        .args(["echo-lines", "a", "b"])
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(stdout.lines().collect::<Vec<_>>(), vec!["a", "b"]);
    assert!(
        String::from_utf8(out.stderr)
            .unwrap()
            .contains("stderr-marker")
    );
}

#[test]
fn gbk_stdout_emits_expected_bytes() {
    let out = std::process::Command::new(child())
        .args(["gbk-stdout"])
        .output()
        .unwrap();
    assert_eq!(&out.stdout[..4], &[0xD6, 0xD0, 0xCE, 0xC4]);
}

#[cfg(all(windows, feature = "process"))]
#[test]
fn recording_shutdown_helper_confirms_each_invocation() {
    let dir = tempfile::tempdir().unwrap();
    let helper = dir.path().join("nyanpasu-recording-shutdown-helper.exe");
    std::fs::copy(child(), &helper).unwrap();
    for _ in 0..2 {
        let status = std::process::Command::new(&helper)
            .args(["--nyanpasu-process-shutdown", "invalid-pid"])
            .status()
            .unwrap();
        assert!(!status.success());
    }
    assert_eq!(
        std::fs::read_to_string(helper.with_extension("calls")).unwrap(),
        "called\ncalled\n"
    );
}
