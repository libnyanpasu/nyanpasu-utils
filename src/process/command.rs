use std::{
    ffi::{OsStr, OsString},
    path::PathBuf,
    time::Duration,
};

/// Builder for spawning a managed child process. See module docs for the contract.
pub struct Command {
    #[expect(
        dead_code,
        reason = "fields are read by process::engine starting with Task 5"
    )]
    pub(crate) program: OsString,
    pub(crate) args: Vec<OsString>,
    pub(crate) envs: Vec<(OsString, OsString)>,
    pub(crate) current_dir: Option<PathBuf>,
    pub(crate) encoding: Option<&'static encoding_rs::Encoding>,
    pub(crate) hide_window: bool,
    pub(crate) kill_grace: Duration,
    pub(crate) event_channel_capacity: usize,
    pub(crate) timeout: Option<Duration>,
    pub(crate) pipe_stdin: bool,
    pub(crate) pid_file: Option<PathBuf>,
}

impl Command {
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            program: program.as_ref().to_os_string(),
            args: Vec::new(),
            envs: Vec::new(),
            current_dir: None,
            encoding: None,
            hide_window: true,
            kill_grace: Duration::from_secs(5),
            event_channel_capacity: 64,
            timeout: None,
            pipe_stdin: false,
            pid_file: None,
        }
    }

    pub fn arg(mut self, a: impl AsRef<OsStr>) -> Self {
        self.args.push(a.as_ref().to_os_string());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|a| a.as_ref().to_os_string()));
        self
    }

    pub fn env(mut self, k: impl AsRef<OsStr>, v: impl AsRef<OsStr>) -> Self {
        self.envs
            .push((k.as_ref().to_os_string(), v.as_ref().to_os_string()));
        self
    }

    pub fn current_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(dir.into());
        self
    }

    pub fn encoding(mut self, enc: Option<&'static encoding_rs::Encoding>) -> Self {
        self.encoding = enc;
        self
    }

    pub fn hide_window(mut self, hide: bool) -> Self {
        self.hide_window = hide;
        self
    }

    pub fn kill_grace(mut self, d: Duration) -> Self {
        self.kill_grace = d;
        self
    }

    pub fn event_channel_capacity(mut self, cap: usize) -> Self {
        self.event_channel_capacity = cap.max(1);
        self
    }

    pub fn timeout(mut self, d: Duration) -> Self {
        self.timeout = Some(d);
        self
    }

    pub fn pipe_stdin(mut self, pipe: bool) -> Self {
        self.pipe_stdin = pipe;
        self
    }

    pub fn pid_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.pid_file = Some(path.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn defaults_match_design() {
        let c = Command::new("prog");
        assert_eq!(c.event_channel_capacity, 64);
        assert_eq!(c.kill_grace, Duration::from_secs(5));
        assert!(c.hide_window);
        assert!(!c.pipe_stdin);
        assert!(c.encoding.is_none());
        assert!(c.pid_file.is_none());
        assert!(c.timeout.is_none());
    }

    #[test]
    fn builder_chain_sets_fields() {
        let c = Command::new("prog")
            .arg("-v")
            .args(["a", "b"])
            .env("K", "V")
            .current_dir("C:/tmp")
            .kill_grace(Duration::from_secs(1))
            .event_channel_capacity(8)
            .pipe_stdin(true)
            .hide_window(false);
        assert_eq!(c.args.len(), 3);
        assert_eq!(c.envs.len(), 1);
        assert_eq!(c.event_channel_capacity, 8);
        assert!(c.pipe_stdin);
        assert!(!c.hide_window);
    }
}
