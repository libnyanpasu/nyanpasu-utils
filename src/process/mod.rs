//! Generic child-process management: spawn, event stream, kill, supervise.
//!
//! Design: docs/superpowers/specs/2026-07-16-nyanpasu-utils-process-module-design.md

mod command;
mod engine;
mod error;
mod event;
mod handle;
mod pid_file;

pub use command::Command;
pub use error::{ProcessError, ProcessOutput};
pub use event::{ProcessEvent, TerminatedPayload};
pub use handle::{Containment, ProcessHandle};
