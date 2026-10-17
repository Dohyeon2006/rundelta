//! Declarative command-line input definitions.
//!
//! This module performs no storage, process, or rendering work. `main` maps the
//! parsed values to one use case at the process boundary.
//!
//! Policy checks belong to the receiving use case: parsing a duration, label,
//! or environment selector does not establish that recording is safe or that
//! a stored label has a unique match. Keeping this distinction prevents CLI
//! validation from becoming a second source of storage/capture policy.

use clap::{Parser, Subcommand, ValueEnum};
use std::{ffi::OsString, path::PathBuf};

// Clap derives visible help from field/variant Rustdoc. Additional implementation
// rationale uses ordinary comments so documentation maintenance does not silently
// change the command's help contract. OsString keeps Unix argv bytes until capture
// converts them to the reversible stored spelling; UTF-8 parsing would lose data.

/// Top-level RunDelta command-line input.
#[derive(Parser)]
#[command(version, about = "Compare observed Linux command executions")]
pub(crate) struct Cli {
    /// Optional storage-root override.
    #[arg(long, global = true)]
    pub(crate) storage: Option<PathBuf>,
    /// Exactly one requested use case.
    #[command(subcommand)]
    pub(crate) command: Action,
}

/// Explicit report contracts accepted at the command-line input boundary.
///
/// Automatic selection is represented by an omitted option. This finite input
/// type keeps a newly accepted CLI value from silently falling back to another
/// contract when the process boundary dispatches it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum DiffReportVersion {
    /// Trace integrity and target provenance, without exhaustive event mapping.
    #[value(name = "5")]
    TraceV5,
    /// Complete changed-outcome event references and bounded-sample counts.
    #[value(name = "6")]
    OutcomeV6,
}

/// One declarative RunDelta use-case selection.
#[derive(Subcommand)]
pub(crate) enum Action {
    /// Record a command. Target status is preserved; capture failure/limit = 125.
    Record {
        /// User label for the new record.
        name: String,
        /// Record this startup environment variable (repeatable, values stored verbatim).
        #[arg(long = "env", value_name = "NAME")]
        environment: Vec<String>,
        /// Opt into generation-bearing events and a marker-bound raw input receipt.
        #[arg(long, value_name = "CONTRACT", value_parser = ["trace-v1"])]
        evidence_contract: Option<String>,
        /// Stop capture after this many milliseconds; omitted means no duration limit.
        #[arg(long, value_name = "MILLISECONDS")]
        max_duration_ms: Option<u64>,
        /// Stop capture after the raw trace grows beyond this many bytes.
        #[arg(long, value_name = "BYTES")]
        max_trace_bytes: Option<u64>,
        /// Wait this many milliseconds after TERM before escalating to KILL.
        #[arg(long, value_name = "MILLISECONDS", default_value_t = 2_000)]
        terminate_grace_ms: u64,
        /// Executable and argv, preserved as separate operating-system strings.
        #[arg(last = true, required = true)]
        command: Vec<OsString>,
    },
    /// List saved runs.
    List {
        /// Render the typed compatibility JSON view instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Show metadata and normalized events; includes stored arguments/environment values.
    Show {
        /// Exact unique record label or generated ID.
        name_or_id: String,
        /// Render stored JSON instead of the text presentation.
        #[arg(long)]
        json: bool,
    },
    /// Isolate one saved run, then permanently remove its raw and structured evidence.
    Delete {
        /// Exact unique record label or generated ID.
        name_or_id: String,
    },
    /// Inspect or explicitly recover isolated deletions; never acts on restart.
    Deleted {
        /// One explicit recovery operation.
        #[command(subcommand)]
        command: DeletedAction,
    },
    /// Compare runs. Exit: 0 verified equal, 1 verified changed, 2 error/corrupt, 3 incomplete/unverified.
    Diff {
        /// Exact unique label or ID for the left comparison side.
        left: String,
        /// Exact unique label or ID for the right comparison side.
        right: String,
        /// Request an explicit report contract, including legacy unverified facts.
        #[arg(long, value_name = "VERSION", value_enum)]
        report_version: Option<DiffReportVersion>,
        /// Render the selected diff JSON contract instead of text.
        #[arg(long)]
        json: bool,
    },
}

/// Explicit operations on private deletion-recovery tickets.
#[derive(Subcommand)]
pub(crate) enum DeletedAction {
    /// List complete, partially cleaned and damaged deletion slots.
    List {
        /// Render typed deletion-state JSON.
        #[arg(long)]
        json: bool,
    },
    /// Restore only a ticket whose physical cleanup has never started.
    Restore {
        /// Exact deletion ticket ID, never a record label or path.
        ticket: String,
    },
    /// Continue irreversible physical cleanup of one exact deletion ticket.
    Purge {
        /// Exact deletion ticket ID, never a record label or path.
        ticket: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_report_contracts_are_finite_and_omission_stays_automatic() {
        let parse = |version: Option<&str>| {
            let mut arguments = vec!["rundelta", "diff", "left", "right"];
            if let Some(version) = version {
                arguments.extend(["--report-version", version]);
            }
            Cli::try_parse_from(arguments)
        };
        for (version, expected) in [
            (None, None),
            (Some("5"), Some(DiffReportVersion::TraceV5)),
            (Some("6"), Some(DiffReportVersion::OutcomeV6)),
        ] {
            let Action::Diff { report_version, .. } = parse(version).unwrap().command else {
                panic!("diff input must select the diff use case");
            };
            assert_eq!(report_version, expected);
        }
        // Reject unsupported contracts at the input boundary before storage is
        // touched. In particular, "4" must not become an alternate automatic flag.
        for unsupported in ["4", "7", "latest", ""] {
            assert!(parse(Some(unsupported)).is_err(), "{unsupported:?}");
        }
    }
}
