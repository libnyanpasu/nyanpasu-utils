//! Generic child-process management: spawn, event stream, kill, supervise.
//!
//! Design: docs/superpowers/specs/2026-07-16-nyanpasu-utils-process-module-design.md

mod error;
mod event;

pub use error::{ProcessError, ProcessOutput};
pub use event::{ProcessEvent, TerminatedPayload};
