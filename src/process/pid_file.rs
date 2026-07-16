use std::{
    path::PathBuf,
    sync::atomic::{AtomicU32, Ordering},
};

/// Owns a pid file's lifecycle around one spawned child (design §5.4).
pub(crate) struct PidFileGuard {
    path: PathBuf,
    pid: AtomicU32,
}

impl PidFileGuard {
    pub(crate) async fn prepare(
        path: PathBuf,
        expected_exe: Option<String>,
    ) -> std::io::Result<Self> {
        let validator: Option<Vec<String>> = expected_exe.map(|e| vec![e.to_lowercase()]);
        match crate::os::kill_by_pid_file(&path, validator.as_deref()).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!("failed to kill residual process from pid file: {e}"),
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
