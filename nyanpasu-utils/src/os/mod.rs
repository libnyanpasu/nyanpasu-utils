#![allow(unused_imports)]
#![allow(dead_code)]

mod os_impl;

pub use os_impl::*;
use sysinfo::{Pid, ProcessRefreshKind, RefreshKind, System};
use tracing_attributes::instrument;

use std::{fmt::Display, io::Error as IoError, path::Path};
use tokio::{fs::OpenOptions, io::AsyncWriteExt};

#[instrument]
pub async fn create_pid_file<T>(path: T, pid: u32) -> Result<(), std::io::Error>
where
    T: AsRef<Path> + std::fmt::Debug,
{
    let path = path.as_ref();
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .await?;
    file.write_all(pid.to_string().as_bytes()).await?;
    Ok(())
}

#[instrument]
pub async fn get_pid_from_file<T>(path: T) -> Option<u32>
where
    T: AsRef<Path> + std::fmt::Debug,
{
    let path = path.as_ref();
    let content = tokio::fs::read_to_string(path).await.unwrap_or_default();
    let pid = content.trim().parse().ok()?;
    Some(pid)
}

#[instrument]
pub fn pid_exists(pid: u32) -> bool {
    let kind = RefreshKind::new().with_processes(ProcessRefreshKind::new());
    let mut system = System::new_with_specifics(kind);
    system.refresh_specifics(kind);
    system.process(Pid::from_u32(pid)).is_some()
}

#[instrument]
pub async fn kill_pid(pid: u32) -> Result<(), std::io::Error> {
    tracing::debug!("kill pid: {}", pid);
    if pid_exists(pid) {
        tracing::debug!("pid exists, kill it");
        let list = kill_tree::tokio::kill_tree(pid as u32)
            .await
            .map_err(|e| IoError::new(std::io::ErrorKind::Other, format!("kill error: {:?}", e)))?;
        for p in list {
            if matches!(p, kill_tree::Output::Killed { .. }) {
                tracing::info!("process is killed: {:?}", p);
            }
        }
    }
    Ok(())
}

#[instrument]
pub async fn kill_by_pid_file<T>(path: T) -> Result<(), std::io::Error>
where
    T: AsRef<Path> + std::fmt::Debug,
{
    let pid = match get_pid_from_file(&path).await {
        Some(pid) => pid,
        None => {
            tracing::debug!("pid file not found or parsing error, skip");
            return Ok(());
        }
    };
    kill_pid(pid).await?;
    tokio::fs::remove_file(path).await
}
