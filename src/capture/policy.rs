//! Resource limits and representability checks for one capture.
//!
//! Policy reads do not inspect a collector or storage. Validation preserves the
//! existing monotonic arithmetic and error wording; supervision separately owns
//! the startup anchor, observed excesses and termination deadlines.

use anyhow::{Context, Result};
use std::time::{Duration, Instant};

/// Resource policy for one recorded command.
///
/// Limits are recorder policy only and are deliberately not persisted as new
/// metadata fields. A zero duration or byte limit is a valid immediate
/// boundary; trace bytes trigger only after the anchored file grows beyond the
/// configured value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CaptureLimits {
    /// Elapsed limit anchored immediately before formal collector startup;
    /// parent-side handshake supervision uses this same clock. Excludes the
    /// version probe, parsing, and storage finalization. Individual Linux
    /// syscalls and post-termination cleanup have separate timing boundaries.
    max_duration: Option<Duration>,
    /// Soft upper bound for the anchored raw trace inode.
    max_trace_bytes: Option<u64>,
    /// Grace between TERM and KILL during supervised termination.
    terminate_grace: Duration,
}

impl CaptureLimits {
    /// Read the elapsed boundary without observing a clock or collector.
    pub(super) fn max_duration(self) -> Option<Duration> {
        self.max_duration
    }

    /// Read the soft anchored-byte boundary without inspecting storage.
    pub(super) fn max_trace_bytes(self) -> Option<u64> {
        self.max_trace_bytes
    }

    /// Read the first-termination grace without starting its deadline.
    pub(super) fn terminate_grace(self) -> Duration {
        self.terminate_grace
    }

    /// Preserve exact durations across the private owner frame and test fixtures.
    /// Validation still rejects unrepresentable policy before capture side effects.
    pub(super) fn from_durations(
        max_duration: Option<Duration>,
        max_trace_bytes: Option<u64>,
        terminate_grace: Duration,
    ) -> Self {
        Self {
            max_duration,
            max_trace_bytes,
            terminate_grace,
        }
    }

    /// Map declarative CLI millisecond values into capture policy.
    pub(crate) fn from_millis(
        max_duration_ms: Option<u64>,
        max_trace_bytes: Option<u64>,
        terminate_grace_ms: u64,
    ) -> Self {
        Self {
            max_duration: max_duration_ms.map(Duration::from_millis),
            max_trace_bytes,
            terminate_grace: Duration::from_millis(terminate_grace_ms),
        }
    }

    /// Reject unrepresentable policy before storage or process side effects.
    pub(super) fn validate(self) -> Result<()> {
        self.validate_at(Instant::now())
    }

    /// Check clock arithmetic again at the actual supervision anchor.
    pub(super) fn validate_at(self, anchor: Instant) -> Result<()> {
        if let Some(duration) = self.max_duration {
            anchor
                .checked_add(duration)
                .context("capture duration limit cannot be represented by the monotonic clock")?;
        }
        anchor
            .checked_add(self.terminate_grace)
            .context("capture termination grace cannot be represented by the monotonic clock")?;
        Ok(())
    }
}

impl Default for CaptureLimits {
    fn default() -> Self {
        Self::from_millis(None, None, 2_000)
    }
}
