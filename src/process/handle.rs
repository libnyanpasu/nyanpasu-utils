use tokio::sync::{mpsc, oneshot, watch};

use super::{error::ProcessError, event::TerminatedPayload};

/// The kernel containment mechanism actually in effect (mirrors the engine's report).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Containment {
    JobObject,
    CgroupV2,
    ProcessGroup,
}

pub(crate) enum Ctrl {
    GracefulKill(oneshot::Sender<Result<(), ProcessError>>),
    Kill(oneshot::Sender<Result<(), ProcessError>>),
    WriteStdin(Vec<u8>, oneshot::Sender<Result<(), ProcessError>>),
}

/// Cloneable handle to a spawned child. Dropping all handles and the event
/// receiver kills the whole process tree.
#[derive(Clone)]
pub struct ProcessHandle {
    pub(crate) pid: u32,
    pub(crate) containment: Containment,
    pub(crate) ctrl: mpsc::Sender<Ctrl>,
    pub(crate) terminated: watch::Receiver<Option<TerminatedPayload>>,
}

impl ProcessHandle {
    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn containment(&self) -> Containment {
        self.containment
    }

    /// Waits until the child terminates; returns immediately if it already has.
    pub async fn wait(&self) -> Result<TerminatedPayload, ProcessError> {
        let mut rx = self.terminated.clone();
        loop {
            if let Some(payload) = rx.borrow().clone() {
                return Ok(payload);
            }
            rx.changed()
                .await
                .map_err(|_| ProcessError::Engine("process pump task dropped".into()))?;
        }
    }

    pub async fn graceful_kill(&self) -> Result<(), ProcessError> {
        self.send_ctrl(Ctrl::GracefulKill).await?;
        self.wait().await?;
        Ok(())
    }

    pub async fn kill(&self) -> Result<(), ProcessError> {
        self.send_ctrl(Ctrl::Kill).await?;
        self.wait().await?;
        Ok(())
    }

    /// Writes and flushes bytes to the child's stdin pipe on a dedicated task,
    /// so output draining never stalls. `Ok(())` means the bytes were written
    /// and flushed successfully.
    pub async fn write_stdin(&self, data: &[u8]) -> Result<(), ProcessError> {
        let data = data.to_vec();
        self.send_ctrl(move |reply| Ctrl::WriteStdin(data, reply))
            .await
    }

    pub(crate) async fn send_ctrl(
        &self,
        make: impl FnOnce(oneshot::Sender<Result<(), ProcessError>>) -> Ctrl,
    ) -> Result<(), ProcessError> {
        let (tx, rx) = oneshot::channel();
        if self.ctrl.send(make(tx)).await.is_err() {
            return if self.terminated.borrow().is_some() {
                Ok(())
            } else {
                Err(ProcessError::AlreadyExited)
            };
        }
        rx.await.map_err(|_| ProcessError::AlreadyExited)?
    }
}
