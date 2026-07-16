/// Exit information of a terminated child. Field semantics match the legacy
/// `core::TerminatedPayload` so downstream migration is a rename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminatedPayload {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

/// Events delivered on the channel returned by [`crate::process::Command::spawn`].
///
/// Contract: `Terminated` is always the final event on the channel.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum ProcessEvent {
    Stdout(String),
    Stderr(String),
    /// Non-fatal IO/decode error while pumping output. The process may still be alive.
    Error(String),
    Terminated(TerminatedPayload),
}
