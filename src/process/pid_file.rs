use std::{
    path::PathBuf,
    sync::atomic::{AtomicU32, Ordering},
};

/// Owns a pid file's lifecycle around one spawned child (design §5.4).
///
/// The pid-file directory must be writable only by the service user. File
/// contents are advisory and are always validated against the expected
/// executable name before any residual-process kill.
///
/// Cleanup verifies that the file still belongs to this child and runs after both
/// normal completion and pump abandonment.
pub(crate) struct PidFileGuard {
    path: PathBuf,
    pid: AtomicU32,
}

impl PidFileGuard {
    /// Kill any residual process recorded in the pid file after validating it.
    pub(crate) async fn prepare(
        path: PathBuf,
        expected_exe: Option<String>,
    ) -> std::io::Result<Self> {
        match tokio::fs::symlink_metadata(&path).await {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("pid file must not be a symlink: {}", path.display()),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }

        if let Some(expected_exe) = expected_exe {
            let validator = [expected_exe.to_lowercase()];
            match crate::os::kill_by_pid_file(&path, Some(&validator)).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!("failed to kill residual process from pid file: {e}"),
            }
        } else {
            tracing::warn!("no validator derivable; skipping residual kill");
        }
        Ok(Self {
            path,
            pid: AtomicU32::new(0),
        })
    }

    pub(crate) async fn write(&self, pid: u32) -> std::io::Result<()> {
        crate::os::create_pid_file(&self.path, pid).await?;
        self.pid.store(pid, Ordering::Relaxed);
        Ok(())
    }

    /// Best-effort removal; never fails the pump.
    pub(crate) async fn cleanup(&self) {
        let pid = self.pid.load(Ordering::Relaxed);
        if crate::os::get_pid_from_file(&self.path).await != Some(pid) {
            return;
        }
        if let Err(e) = tokio::fs::remove_file(&self.path).await
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!("failed to remove pid file {:?}: {e}", self.path);
        }
    }
}
