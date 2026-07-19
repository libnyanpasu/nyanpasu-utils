use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
    time::Duration,
};

use tokio::io::AsyncWriteExt;

const EPOCH_PID_VERSION: u32 = 1;
const IDENTITY_WAIT_ATTEMPTS: usize = 20;
const IDENTITY_WAIT_DELAY: Duration = Duration::from_millis(25);
const KILL_WAIT_ATTEMPTS: usize = 100;
const KILL_WAIT_DELAY: Duration = Duration::from_millis(50);

/// Describes one manager-owned, per-epoch pid record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpochPidFile {
    path: PathBuf,
    epoch: u64,
    runtime_config: PathBuf,
}

impl EpochPidFile {
    pub fn new(path: impl Into<PathBuf>, epoch: u64, runtime_config: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            epoch,
            runtime_config: runtime_config.into(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn runtime_config(&self) -> &Path {
        &self.runtime_config
    }
}

/// Versioned pid-file contents used for post-manager-kill orphan recovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpochPidRecord {
    pub pid: u32,
    pub epoch: u64,
    pub executable: String,
    pub start_token: u64,
    pub runtime_config: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrphanReapOutcome {
    NotFound,
    AlreadyExited,
    Killed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcessIdentity {
    executable: String,
    start_token: u64,
}

/// Owns one pid file around a spawned child.
///
/// Legacy numeric files remain supported for writing and cleanup, but no longer
/// authorize residual kills because they cannot prove epoch/start identity.
/// Epoch records kill only a fully matching same-epoch leftover.
pub(crate) enum PidFileGuard {
    Legacy {
        path: PathBuf,
        pid: AtomicU32,
    },
    Epoch {
        spec: EpochPidFile,
        expected_exe: String,
        record: parking_lot::Mutex<Option<EpochPidRecord>>,
    },
}

impl PidFileGuard {
    pub(crate) async fn prepare_legacy(path: PathBuf) -> std::io::Result<Self> {
        validate_pid_target(&path).await?;
        Ok(Self::Legacy {
            path,
            pid: AtomicU32::new(0),
        })
    }

    pub(crate) async fn prepare_epoch(
        spec: EpochPidFile,
        expected_exe: String,
    ) -> std::io::Result<Self> {
        let spec = normalize_epoch_spec(spec).await?;
        validate_pid_target(&spec.path).await?;

        if let Some(record) = read_epoch_pid_file(&spec.path).await? {
            validate_record_for_spec(&record, &spec, &expected_exe)?;
            reap_record(&record).await?;
            remove_record_if_matches(&spec.path, &record).await?;
        }

        Ok(Self::Epoch {
            spec,
            expected_exe,
            record: parking_lot::Mutex::new(None),
        })
    }

    pub(crate) async fn write(&self, pid: u32) -> std::io::Result<()> {
        match self {
            Self::Legacy { path, pid: slot } => {
                crate::os::create_pid_file(path, pid).await?;
                slot.store(pid, Ordering::Relaxed);
                Ok(())
            }
            Self::Epoch {
                spec,
                expected_exe,
                record,
            } => {
                let identity = wait_for_process_identity(pid).await?.ok_or_else(|| {
                    identity_error(format!("spawned pid {pid} disappeared before recording"))
                })?;
                if !exe_names_equal(&identity.executable, expected_exe) {
                    return Err(identity_error(format!(
                        "spawned pid {pid} executable `{}` does not match `{expected_exe}`",
                        identity.executable
                    )));
                }
                let value = EpochPidRecord {
                    pid,
                    epoch: spec.epoch,
                    executable: identity.executable,
                    start_token: identity.start_token,
                    runtime_config: spec.runtime_config.clone(),
                };
                write_epoch_record(&spec.path, &value).await?;
                *record.lock() = Some(value);
                Ok(())
            }
        }
    }

    /// Best-effort removal; never fails the process pump.
    pub(crate) async fn cleanup(&self) {
        let result = match self {
            Self::Legacy { path, pid } => {
                let pid = pid.load(Ordering::Relaxed);
                if crate::os::get_pid_from_file(path).await != Some(pid) {
                    return;
                }
                tokio::fs::remove_file(path).await
            }
            Self::Epoch { spec, record, .. } => {
                let value = record.lock().clone();
                match value {
                    Some(value) => remove_record_if_matches(&spec.path, &value).await,
                    None => return,
                }
            }
        };
        if let Err(error) = result
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!("failed to remove pid file: {error}");
        }
    }
}

/// Reads a structured epoch pid record. Numeric legacy files are rejected.
pub async fn read_epoch_pid_file(path: impl AsRef<Path>) -> std::io::Result<Option<EpochPidRecord>> {
    let path = path.as_ref();
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(invalid_input(format!(
                "pid file must not be a symlink: {}",
                path.display()
            )));
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err(invalid_input(format!(
                "pid file must be a regular file: {}",
                path.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    let raw = tokio::fs::read_to_string(path).await?;
    parse_epoch_record(&raw).map(Some)
}

/// Kills the orphan in `path` only after validating the full epoch record and
/// proving that both pid and runtime config are contained by `runtime_dir`.
pub async fn reap_epoch_pid_file(
    path: impl AsRef<Path>,
    runtime_dir: impl AsRef<Path>,
) -> std::io::Result<OrphanReapOutcome> {
    let path = path.as_ref();
    let runtime_dir = tokio::fs::canonicalize(runtime_dir.as_ref()).await?;
    let parent = path
        .parent()
        .ok_or_else(|| invalid_input("pid file has no parent directory"))?;
    if tokio::fs::canonicalize(parent).await? != runtime_dir {
        return Err(invalid_input("pid file escapes the runtime directory"));
    }
    let epoch = epoch_from_file_name(path, "core-", ".pid")?;
    let Some(record) = read_epoch_pid_file(path).await? else {
        return Ok(OrphanReapOutcome::NotFound);
    };
    if record.epoch != epoch {
        return Err(invalid_data("pid filename and embedded epoch differ"));
    }
    let runtime_parent = record
        .runtime_config
        .parent()
        .ok_or_else(|| invalid_input("runtime config has no parent directory"))?;
    if tokio::fs::canonicalize(runtime_parent).await? != runtime_dir {
        return Err(invalid_input("runtime config escapes the runtime directory"));
    }
    let runtime_epoch = epoch_from_file_name(&record.runtime_config, "config-", ".yaml")?;
    if runtime_epoch != epoch {
        return Err(invalid_data(
            "runtime config filename and embedded epoch differ",
        ));
    }
    validate_runtime_target(&record.runtime_config).await?;

    let outcome = reap_record(&record).await?;
    remove_record_if_matches(path, &record).await?;
    Ok(outcome)
}

async fn normalize_epoch_spec(spec: EpochPidFile) -> std::io::Result<EpochPidFile> {
    let pid_epoch = epoch_from_file_name(&spec.path, "core-", ".pid")?;
    let config_epoch = epoch_from_file_name(&spec.runtime_config, "config-", ".yaml")?;
    if pid_epoch != spec.epoch || config_epoch != spec.epoch {
        return Err(invalid_input(
            "epoch pid/config filenames must match the requested epoch",
        ));
    }
    let pid_parent = spec
        .path
        .parent()
        .ok_or_else(|| invalid_input("pid file has no parent directory"))?;
    let config_parent = spec
        .runtime_config
        .parent()
        .ok_or_else(|| invalid_input("runtime config has no parent directory"))?;
    let pid_parent = tokio::fs::canonicalize(pid_parent).await?;
    let config_parent = tokio::fs::canonicalize(config_parent).await?;
    if pid_parent != config_parent {
        return Err(invalid_input(
            "epoch pid and runtime config must share one directory",
        ));
    }
    validate_runtime_target(&spec.runtime_config).await?;
    Ok(EpochPidFile {
        path: pid_parent.join(format!("core-{}.pid", spec.epoch)),
        epoch: spec.epoch,
        runtime_config: config_parent.join(format!("config-{}.yaml", spec.epoch)),
    })
}

async fn validate_pid_target(path: &Path) -> std::io::Result<()> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(invalid_input(format!(
            "pid file must not be a symlink: {}",
            path.display()
        ))),
        Ok(metadata) if !metadata.is_file() => Err(invalid_input(format!(
            "pid file must be a regular file: {}",
            path.display()
        ))),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

async fn validate_runtime_target(path: &Path) -> std::io::Result<()> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(invalid_input(format!(
            "runtime config must not be a symlink: {}",
            path.display()
        ))),
        Ok(metadata) if !metadata.is_file() => Err(invalid_input(format!(
            "runtime config must be a regular file: {}",
            path.display()
        ))),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn validate_record_for_spec(
    record: &EpochPidRecord,
    spec: &EpochPidFile,
    expected_exe: &str,
) -> std::io::Result<()> {
    if record.epoch != spec.epoch {
        return Err(identity_error(format!(
            "pid file belongs to epoch {}, not {}",
            record.epoch, spec.epoch
        )));
    }
    if record.runtime_config != spec.runtime_config {
        return Err(identity_error(
            "pid record runtime config does not match the requested epoch",
        ));
    }
    if !exe_names_equal(&record.executable, expected_exe) {
        return Err(identity_error(format!(
            "pid record executable `{}` does not match `{expected_exe}`",
            record.executable
        )));
    }
    Ok(())
}

async fn reap_record(record: &EpochPidRecord) -> std::io::Result<OrphanReapOutcome> {
    let validator = [record.executable.to_lowercase()];
    reap_record_with_kill(record, || {
        crate::os::kill_pid(record.pid, Some(&validator))
    })
    .await
}

async fn reap_record_with_kill<K, F>(
    record: &EpochPidRecord,
    kill: K,
) -> std::io::Result<OrphanReapOutcome>
where
    K: FnOnce() -> F,
    F: std::future::Future<Output = std::io::Result<()>>,
{
    let Some(identity) = process_identity(record.pid)? else {
        return Ok(OrphanReapOutcome::AlreadyExited);
    };
    if !record_matches_identity(record, &identity) {
        return Err(identity_error(format!(
            "cannot prove ownership of live pid {}",
            record.pid
        )));
    }

    // Windows may report access denied when termination is already in flight.
    // A kill error is therefore provisional until the bounded identity window
    // proves that this exact process survived it.
    let kill_error = kill().await.err();
    let mut identity_error = None;
    for attempt in 0..KILL_WAIT_ATTEMPTS {
        match process_identity(record.pid) {
            Ok(None) => return Ok(OrphanReapOutcome::Killed),
            Ok(Some(identity)) if !record_matches_identity(record, &identity) => {
                return Ok(OrphanReapOutcome::Killed);
            }
            Ok(Some(_)) => identity_error = None,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                identity_error = Some(error);
            }
            Err(error) => return Err(error),
        }
        if attempt + 1 < KILL_WAIT_ATTEMPTS {
            tokio::time::sleep(KILL_WAIT_DELAY).await;
        }
    }
    if let Some(error) = kill_error {
        return Err(error);
    }
    if let Some(error) = identity_error.take() {
        return Err(error);
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        format!("pid {} remained alive after validated kill", record.pid),
    ))
}

async fn wait_for_process_identity(pid: u32) -> std::io::Result<Option<ProcessIdentity>> {
    for attempt in 0..IDENTITY_WAIT_ATTEMPTS {
        if let Some(identity) = process_identity(pid)? {
            return Ok(Some(identity));
        }
        if attempt + 1 < IDENTITY_WAIT_ATTEMPTS {
            tokio::time::sleep(IDENTITY_WAIT_DELAY).await;
        }
    }
    Ok(None)
}

fn process_identity(pid: u32) -> std::io::Result<Option<ProcessIdentity>> {
    use sysinfo::{Pid, ProcessRefreshKind, RefreshKind, System, UpdateKind};

    let kind = RefreshKind::nothing()
        .with_processes(ProcessRefreshKind::nothing().with_exe(UpdateKind::Always));
    let mut system = System::new_with_specifics(kind);
    system.refresh_specifics(kind);
    let Some(process) = system.process(Pid::from_u32(pid)) else {
        return Ok(None);
    };
    let executable = process
        .exe()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .ok_or_else(|| identity_error(format!("cannot resolve executable for live pid {pid}")))?;
    Ok(Some(ProcessIdentity {
        executable: executable.to_owned(),
        start_token: process.start_time(),
    }))
}

fn record_matches_identity(record: &EpochPidRecord, identity: &ProcessIdentity) -> bool {
    record.start_token == identity.start_token
        && exe_names_equal(&record.executable, &identity.executable)
}

#[cfg(windows)]
fn exe_names_equal(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

#[cfg(not(windows))]
fn exe_names_equal(left: &str, right: &str) -> bool {
    left == right
}

async fn write_epoch_record(path: &Path, record: &EpochPidRecord) -> std::io::Result<()> {
    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    validate_pid_target(path).await?;
    if tokio::fs::try_exists(path).await? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("pid file unexpectedly exists: {}", path.display()),
        ));
    }
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp = path.with_extension(format!("pid.tmp-{}-{counter}", std::process::id()));
    let result = async {
        let mut file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)
            .await?;
        file.write_all(serialize_epoch_record(record)?.as_bytes())
            .await?;
        file.flush().await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&temp, path).await
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temp).await;
    }
    result
}

async fn remove_record_if_matches(
    path: &Path,
    expected: &EpochPidRecord,
) -> std::io::Result<()> {
    if read_epoch_pid_file(path).await?.as_ref() != Some(expected) {
        return Ok(());
    }
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn serialize_epoch_record(record: &EpochPidRecord) -> std::io::Result<String> {
    let runtime_config = record.runtime_config.to_str().ok_or_else(|| {
        invalid_input("runtime config path must be UTF-8 for an epoch pid record")
    })?;
    Ok(format!(
        "version={EPOCH_PID_VERSION}\npid={}\nepoch={}\nexecutable={}\nstart-token={}\nruntime-config={}\n",
        record.pid,
        record.epoch,
        hex_encode(record.executable.as_bytes()),
        record.start_token,
        hex_encode(runtime_config.as_bytes()),
    ))
}

fn parse_epoch_record(raw: &str) -> std::io::Result<EpochPidRecord> {
    let mut fields = BTreeMap::new();
    for line in raw.lines() {
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| invalid_data("malformed epoch pid record line"))?;
        if fields.insert(key, value).is_some() {
            return Err(invalid_data("duplicate epoch pid record field"));
        }
    }
    let expected = [
        "epoch",
        "executable",
        "pid",
        "runtime-config",
        "start-token",
        "version",
    ];
    if fields.len() != expected.len() || !expected.iter().all(|key| fields.contains_key(key)) {
        return Err(invalid_data("epoch pid record fields are incomplete"));
    }
    let version = parse_field::<u32>(&fields, "version")?;
    if version != EPOCH_PID_VERSION {
        return Err(invalid_data(format!(
            "unsupported epoch pid record version {version}"
        )));
    }
    let executable = String::from_utf8(hex_decode(required(&fields, "executable")?)?)
        .map_err(|_| invalid_data("epoch pid executable is not UTF-8"))?;
    let runtime_config = String::from_utf8(hex_decode(required(&fields, "runtime-config")?)?)
        .map_err(|_| invalid_data("epoch pid runtime path is not UTF-8"))?;
    Ok(EpochPidRecord {
        pid: parse_field(&fields, "pid")?,
        epoch: parse_field(&fields, "epoch")?,
        executable,
        start_token: parse_field(&fields, "start-token")?,
        runtime_config: PathBuf::from(runtime_config),
    })
}

fn required<'a>(fields: &'a BTreeMap<&str, &str>, key: &str) -> std::io::Result<&'a str> {
    fields
        .get(key)
        .copied()
        .ok_or_else(|| invalid_data(format!("missing epoch pid field `{key}`")))
}

fn parse_field<T: std::str::FromStr>(
    fields: &BTreeMap<&str, &str>,
    key: &str,
) -> std::io::Result<T> {
    required(fields, key)?
        .parse()
        .map_err(|_| invalid_data(format!("invalid epoch pid field `{key}`")))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn hex_decode(value: &str) -> std::io::Result<Vec<u8>> {
    let (pairs, remainder) = value.as_bytes().as_chunks::<2>();
    if !remainder.is_empty() {
        return Err(invalid_data("hex field has odd length"));
    }
    pairs
        .iter()
        .map(|pair| {
            let high = hex_nibble(pair[0])?;
            let low = hex_nibble(pair[1])?;
            Ok((high << 4) | low)
        })
        .collect()
}

fn hex_nibble(value: u8) -> std::io::Result<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(invalid_data("invalid hex field")),
    }
}

fn epoch_from_file_name(path: &Path, prefix: &str, suffix: &str) -> std::io::Result<u64> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid_input("epoch artifact filename must be UTF-8"))?;
    let epoch = name
        .strip_prefix(prefix)
        .and_then(|value| value.strip_suffix(suffix))
        .ok_or_else(|| invalid_input(format!("invalid epoch artifact name `{name}`")))?;
    epoch
        .parse()
        .map_err(|_| invalid_input(format!("invalid epoch in artifact name `{name}`")))
}

fn invalid_input(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message.into())
}

fn invalid_data(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())
}

fn identity_error(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::PermissionDenied, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_record_round_trips() {
        let record = EpochPidRecord {
            pid: 42,
            epoch: 7,
            executable: "core=name.exe".into(),
            start_token: 99,
            runtime_config: PathBuf::from(r"C:\run dir\config-7.yaml"),
        };
        assert_eq!(
            parse_epoch_record(&serialize_epoch_record(&record).unwrap()).unwrap(),
            record
        );
    }

    #[tokio::test]
    async fn kill_failure_is_ignored_only_after_recorded_identity_disappears() {
        #[cfg(windows)]
        let mut child = std::process::Command::new("cmd")
            .args(["/D", "/S", "/C", "ping -n 30 127.0.0.1 >NUL"])
            .spawn()
            .unwrap();
        #[cfg(unix)]
        let mut child = std::process::Command::new("sh")
            .args(["-c", "sleep 30"])
            .spawn()
            .unwrap();

        let pid = child.id();
        let identity = wait_for_process_identity(pid).await.unwrap().unwrap();
        let record = EpochPidRecord {
            pid,
            epoch: 1,
            executable: identity.executable,
            start_token: identity.start_token,
            runtime_config: PathBuf::from("config-1.yaml"),
        };

        let outcome = reap_record_with_kill(&record, || async move {
            crate::os::kill_pid::<String>(pid, None).await?;
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "simulated already-terminating process",
            ))
        })
        .await
        .unwrap();

        assert_eq!(outcome, OrphanReapOutcome::Killed);
        let _ = child.wait();
    }
}
