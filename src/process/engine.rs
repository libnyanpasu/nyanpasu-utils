//! processkit adapter. This is the only file in `src/process` that uses processkit.

use std::{collections::VecDeque, future::pending, sync::Arc, time::Duration};

use processkit::prelude::StreamExt;
use tokio::sync::{mpsc, oneshot, watch};

use super::{
    command::Command,
    error::ProcessError,
    event::{ProcessEvent, TerminatedPayload},
    handle::{Containment, Ctrl},
    pid_file::PidFileGuard,
};

pub(crate) struct SpawnParts {
    pub pid: u32,
    pub containment: Containment,
    pub ctrl_tx: mpsc::Sender<Ctrl>,
    pub terminated_rx: watch::Receiver<Option<TerminatedPayload>>,
    pub events_rx: mpsc::Receiver<ProcessEvent>,
}

struct PumpParts {
    run: processkit::RunningProcess,
    events: processkit::OutputEvents,
    group: Arc<processkit::ProcessGroup>,
    stdin_tx: Option<mpsc::UnboundedSender<StdinWrite>>,
    kill_grace: Duration,
    ev_tx: mpsc::Sender<ProcessEvent>,
    ctrl_rx: mpsc::Receiver<Ctrl>,
    term_tx: watch::Sender<Option<TerminatedPayload>>,
    pid_guard: Option<PidFileGuard>,
}

fn build_pk(cmd: &Command) -> processkit::Command {
    let mut pk = processkit::Command::new(&cmd.program).args(&cmd.args);
    for (key, value) in &cmd.envs {
        pk = pk.env(key, value);
    }
    if let Some(dir) = &cmd.current_dir {
        pk = pk.current_dir(dir);
    }
    if let Some(encoding) = cmd.encoding {
        pk = pk.encoding(encoding);
    }
    if let Some(timeout) = cmd.timeout {
        pk = pk.timeout(timeout);
    }
    #[cfg(windows)]
    if cmd.hide_window {
        pk = pk.create_no_window();
    }
    pk = pk.output_buffer(processkit::OutputBufferPolicy::unbounded().with_max_bytes(256 * 1024));
    if cmd.pipe_stdin {
        pk = pk.keep_stdin_open();
    }
    pk
}

pub(crate) async fn run_capture(cmd: Command) -> Result<super::error::ProcessOutput, ProcessError> {
    let program = cmd.program.to_string_lossy().into_owned();
    let timeout = cmd.timeout;
    let result = build_pk(&cmd)
        .output_string()
        .await
        .map_err(|error| ProcessError::Spawn {
            program,
            message: error.to_string(),
        })?;

    if result.timed_out() {
        return Err(ProcessError::Timeout {
            after: timeout.unwrap_or_default(),
        });
    }

    Ok(super::error::ProcessOutput {
        code: result.code(),
        stdout: result.stdout().clone(),
        stderr: result.stderr().to_owned(),
    })
}

fn map_containment(mechanism: processkit::Mechanism) -> Containment {
    match mechanism {
        processkit::Mechanism::JobObject => Containment::JobObject,
        processkit::Mechanism::CgroupV2 => Containment::CgroupV2,
        processkit::Mechanism::ProcessGroup => Containment::ProcessGroup,
        _ => Containment::ProcessGroup,
    }
}

fn map_outcome(outcome: &processkit::Outcome) -> TerminatedPayload {
    TerminatedPayload {
        code: outcome.code(),
        signal: outcome.signal(),
    }
}

async fn hard_kill_deadline(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at).await,
        None => pending::<()>().await,
    }
}

type StdinWrite = (Vec<u8>, oneshot::Sender<Result<(), ProcessError>>);

async fn write_stdin(
    mut stdin: processkit::ProcessStdin,
    mut requests: mpsc::UnboundedReceiver<StdinWrite>,
) {
    while let Some((data, reply)) = requests.recv().await {
        if stdin.write(&data).await.is_err() || stdin.flush().await.is_err() {
            let _ = reply.send(Err(ProcessError::StdinUnavailable));
            drop(stdin);
            requests.close();
            while let Some((_, reply)) = requests.recv().await {
                let _ = reply.send(Err(ProcessError::StdinUnavailable));
            }
            return;
        }
        let _ = reply.send(Ok(()));
    }
}

fn handle_ctrl(
    ctrl: Ctrl,
    group: &processkit::ProcessGroup,
    stdin_tx: &Option<mpsc::UnboundedSender<StdinWrite>>,
    kill_grace: Duration,
    hard_kill_at: &mut Option<tokio::time::Instant>,
) {
    match ctrl {
        Ctrl::Kill(reply) => {
            *hard_kill_at = None;
            let result = group
                .kill_all()
                .map_err(|error| ProcessError::Engine(error.to_string()));
            let _ = reply.send(result);
        }
        Ctrl::GracefulKill(reply) => {
            #[cfg(unix)]
            let result = group
                .signal(processkit::Signal::Term)
                .map_err(|error| ProcessError::Engine(error.to_string()));
            #[cfg(unix)]
            if result.is_ok() {
                *hard_kill_at = Some(tokio::time::Instant::now() + kill_grace);
            }

            #[cfg(windows)]
            let result = {
                let _ = kill_grace;
                *hard_kill_at = None;
                group
                    .kill_all()
                    .map_err(|error| ProcessError::Engine(error.to_string()))
            };

            let _ = reply.send(result);
        }
        Ctrl::WriteStdin(data, reply) => {
            if let Some(stdin_tx) = stdin_tx {
                if let Err(error) = stdin_tx.send((data, reply)) {
                    let (_, reply) = error.0;
                    let _ = reply.send(Err(ProcessError::StdinUnavailable));
                }
            } else {
                let _ = reply.send(Err(ProcessError::StdinUnavailable));
            }
        }
    }
}

pub(crate) async fn spawn(cmd: Command) -> Result<SpawnParts, ProcessError> {
    let program = cmd.program.to_string_lossy().into_owned();
    let capacity = cmd.event_channel_capacity;
    let kill_grace = cmd.kill_grace;
    let pipe_stdin = cmd.pipe_stdin;
    let pid_guard = match &cmd.pid_file {
        Some(path) => {
            let expected_exe = std::path::Path::new(&cmd.program)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned());
            Some(PidFileGuard::prepare(path.clone(), expected_exe).await?)
        }
        None => None,
    };
    let pk = build_pk(&cmd);

    let spawn_error = |error: processkit::Error| ProcessError::Spawn {
        program: program.clone(),
        message: error.to_string(),
    };
    let group = Arc::new(processkit::ProcessGroup::new().map_err(&spawn_error)?);
    let containment = map_containment(group.mechanism());
    let mut run = group.start(&pk).await.map_err(spawn_error)?;
    let pid = run
        .pid()
        .ok_or_else(|| ProcessError::Engine("spawned process has no pid".into()))?;
    if let Some(g) = &pid_guard
        && let Err(e) = g.write(pid).await
    {
        tracing::warn!("failed to write pid file: {e}");
    }
    let stdin_tx = if pipe_stdin {
        run.take_stdin().map(|stdin| {
            let (stdin_tx, stdin_rx) = mpsc::unbounded_channel();
            tokio::spawn(write_stdin(stdin, stdin_rx));
            stdin_tx
        })
    } else {
        None
    };
    let events = run
        .output_events()
        .map_err(|error| ProcessError::Engine(error.to_string()))?;

    let (ev_tx, ev_rx) = mpsc::channel(capacity);
    let (ctrl_tx, ctrl_rx) = mpsc::channel(8);
    let (term_tx, term_rx) = watch::channel(None);

    tokio::spawn(pump(PumpParts {
        run,
        events,
        group,
        stdin_tx,
        kill_grace,
        ev_tx,
        ctrl_rx,
        term_tx,
        pid_guard,
    }));

    Ok(SpawnParts {
        pid,
        containment,
        ctrl_tx,
        terminated_rx: term_rx,
        events_rx: ev_rx,
    })
}

async fn pump(parts: PumpParts) {
    let PumpParts {
        run,
        mut events,
        group,
        stdin_tx,
        kill_grace,
        ev_tx,
        mut ctrl_rx,
        term_tx,
        pid_guard,
    } = parts;
    let mut run = Some(run);
    let mut pending_event = None;
    let mut hard_kill_at = None;
    let mut events_open = true;
    let mut ctrl_open = true;

    loop {
        if !events_open && !ctrl_open {
            let _ = group.kill_all();
            cleanup_pid_file(&pid_guard).await;
            return;
        }

        tokio::select! {
            biased;
            ctrl = ctrl_rx.recv(), if ctrl_open => match ctrl {
                Some(ctrl) => {
                    handle_ctrl(
                        ctrl,
                        &group,
                        &stdin_tx,
                        kill_grace,
                        &mut hard_kill_at,
                    );
                }
                None => ctrl_open = false,
            },
            _ = hard_kill_deadline(hard_kill_at) => {
                hard_kill_at = None;
                let _ = group.kill_all();
            }
            permit = ev_tx.reserve(), if events_open && pending_event.is_some() => match permit {
                Ok(permit) => permit.send(pending_event.take().expect("pending event")),
                Err(_) => {
                    events_open = false;
                    pending_event = None;
                }
            },
            _ = ev_tx.closed(), if events_open => {
                events_open = false;
                pending_event = None;
            }
            event = events.next(), if pending_event.is_none() => match event {
                Some(processkit::OutputEvent::Stdout(line)) if events_open => {
                    pending_event = Some(ProcessEvent::Stdout(line.into_text()));
                }
                Some(processkit::OutputEvent::Stderr(line)) if events_open => {
                    pending_event = Some(ProcessEvent::Stderr(line.into_text()));
                }
                Some(_) => {}
                None => break,
            },
        }
    }

    drop(events);
    let mut finish = Box::pin(run.take().expect("running process").finish());
    let finished = loop {
        if !events_open && !ctrl_open {
            let _ = group.kill_all();
            cleanup_pid_file(&pid_guard).await;
            return;
        }

        tokio::select! {
            biased;
            ctrl = ctrl_rx.recv(), if ctrl_open => match ctrl {
                Some(ctrl) => {
                    handle_ctrl(
                        ctrl,
                        &group,
                        &stdin_tx,
                        kill_grace,
                        &mut hard_kill_at,
                    );
                }
                None => ctrl_open = false,
            },
            _ = hard_kill_deadline(hard_kill_at) => {
                hard_kill_at = None;
                let _ = group.kill_all();
            }
            _ = ev_tx.closed(), if events_open => events_open = false,
            result = &mut finish => break result,
        }
    };

    let (payload, finish_error) = match finished {
        Ok(finished) => (map_outcome(&finished.outcome), None),
        Err(error) => (
            TerminatedPayload {
                code: None,
                signal: None,
            },
            Some(error.to_string()),
        ),
    };
    let _ = term_tx.send(Some(payload.clone()));

    let mut terminal_events = VecDeque::new();
    if let Some(error) = finish_error {
        terminal_events.push_back(ProcessEvent::Error(error));
    }
    terminal_events.push_back(ProcessEvent::Terminated(payload));

    while events_open && !terminal_events.is_empty() {
        tokio::select! {
            biased;
            ctrl = ctrl_rx.recv(), if ctrl_open => match ctrl {
                Some(ctrl) => {
                    handle_ctrl(
                        ctrl,
                        &group,
                        &stdin_tx,
                        kill_grace,
                        &mut hard_kill_at,
                    );
                }
                None => ctrl_open = false,
            },
            _ = hard_kill_deadline(hard_kill_at) => {
                hard_kill_at = None;
                let _ = group.kill_all();
            }
            _ = ev_tx.closed() => events_open = false,
            permit = ev_tx.reserve() => match permit {
                Ok(permit) => permit.send(terminal_events.pop_front().expect("terminal event")),
                Err(_) => events_open = false,
            },
        }
    }

    cleanup_pid_file(&pid_guard).await;
}

async fn cleanup_pid_file(pid_guard: &Option<PidFileGuard>) {
    if let Some(guard) = pid_guard {
        guard.cleanup().await;
    }
}
