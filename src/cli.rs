//! Declarative command-line input definitions.
//!
//! This module performs no storage, process, or rendering work. `main` maps the
//! parsed values to one use case at the process boundary.

use clap::{Parser, Subcommand};
use std::{ffi::OsString, path::PathBuf};

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
    /// Permanently remove one saved run and its raw evidence.
    Delete {
        /// Exact unique record label or generated ID.
        name_or_id: String,
    },
    /// Compare runs. Exit: 0 verified equal, 1 verified changed, 2 error/corrupt, 3 incomplete/unverified.
    Diff {
        /// Exact unique label or ID for the left comparison side.
        left: String,
        /// Exact unique label or ID for the right comparison side.
        right: String,
        /// Render the frozen diff-v4 JSON view instead of text.
        #[arg(long)]
        json: bool,
    },
}
