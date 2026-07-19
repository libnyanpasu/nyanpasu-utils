# Nyanpasu Utils

Shared utilities for Nyanpasu UI and Nyanpasu Service.

The repository contains a single `nyanpasu-utils` crate. Utilities are organized as feature-gated
modules so the crate can be embedded in a larger monorepo without nested Cargo workspaces:

- `core` — proxy-core process lifecycle management (`core_manager` feature)
- `dirs` — platform-aware application directories (`dirs` feature)
- `io` and `runtime` — shared IO and Tokio runtime helpers
- `network` — platform-specific network configuration (`network` feature)
- `os` — operating-system and process helpers (`os` feature)
- `process` — supervised children and versioned per-epoch PID records. Orphan
  termination uses the same validated process handle on Windows and a pidfd on
  supported Linux kernels. Other Unix targets immediately revalidate before a
  PID signal, with a documented residual PID-reuse window.

Epoch record staging files are swept on the next manager startup. A narrow gap
remains between process creation and identity-record publication; an orphan
from that interval is deliberately not killed without authoritative identity.

Default features preserve the existing public API. Consumers that need a smaller dependency surface
can disable default features and enable only the modules they use.
