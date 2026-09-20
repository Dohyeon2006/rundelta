use clap::{Parser, Subcommand};
use std::{ffi::OsString, path::PathBuf};
#[derive(Parser)]
#[command(version, about = "Compare observed Linux command executions")]
pub struct Cli {
    #[arg(long, global = true)]
    pub storage: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Action,
}
#[derive(Subcommand)]
pub enum Action {
    /// Record a command. Target exit status is preserved; collector error = 125.
    Record {
        name: String,
        /// Record this startup environment variable (repeatable, values stored verbatim).
        #[arg(long = "env", value_name = "NAME")]
        environment: Vec<String>,
        #[arg(last = true, required = true)]
        command: Vec<OsString>,
    },
    /// List saved runs.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Show metadata and normalized events; includes stored arguments/environment values.
    Show {
        name_or_id: String,
        #[arg(long)]
        json: bool,
    },
    /// Permanently remove one saved run and its raw evidence.
    Delete { name_or_id: String },
    /// Compare runs. Exit: 0 verified equal, 1 verified changed, 2 error/corrupt, 3 incomplete/unverified.
    Diff {
        left: String,
        right: String,
        #[arg(long)]
        json: bool,
    },
}
