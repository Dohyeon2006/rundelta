//! I/O-free domain and wire representations shared inside the binary crate.
//!
//! Serializable structs in this module mirror the frozen metadata, event, and
//! report-adjacent storage contracts. They carry observed facts and evidence;
//! they do not infer causation, physical file identity, or capture reliability.

use serde::{Deserialize, Serialize};

/// Final capture state exposed to the record command after storage finalization.
///
/// This is an in-memory result type. Persisted metadata keeps its existing
/// string representation and schema version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaptureState {
    /// The target completion was observed and confirmed by the collector.
    Completed,
    /// Recorder cancellation was observed while supervising the collector.
    Interrupted,
    /// Capture or collector facts could not establish target completion.
    CaptureFailed,
}

impl std::fmt::Display for CaptureState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Completed => "completed",
            Self::Interrupted => "interrupted",
            Self::CaptureFailed => "capture_failed",
        })
    }
}

/// Final completeness classification exposed by a completed record use case.
///
/// The enum is deliberately not serialized; metadata v3 continues to use its
/// established string values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaptureCompleteness {
    /// All supported observations were captured without warnings.
    CompleteForSupportedEvents,
    /// Target completion was established, but warnings limit the evidence.
    Partial,
    /// Target completion or capture supervision was not established.
    Incomplete,
}

impl std::fmt::Display for CaptureCompleteness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::CompleteForSupportedEvents => "complete_for_supported_events",
            Self::Partial => "partial",
            Self::Incomplete => "incomplete",
        })
    }
}

/// Result of a record use case whose structured files were already finalized.
///
/// This presentation-neutral value is not part of metadata, event, marker, or
/// diff JSON schemas. `capture::record` constructs it only after `store::finish`
/// succeeds; it does not strengthen the recorded facts or durability claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecordResult {
    /// Stable internal record identifier.
    pub(crate) id: String,
    /// User-provided record label.
    pub(crate) name: String,
    /// Final capture state.
    pub(crate) state: CaptureState,
    /// Final evidence completeness classification.
    pub(crate) completeness: CaptureCompleteness,
    /// Number of normalized events written to storage.
    pub(crate) event_count: usize,
    /// Capture and parser warnings preserved in metadata.
    pub(crate) warnings: Vec<String>,
    /// Display spelling of the finalized evidence directory.
    pub(crate) evidence_dir: String,
    /// Business exit status computed for the record command.
    pub(crate) cli_exit: i32,
}

/// Original raw-trace coordinates supporting one normalized observation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Evidence {
    /// Evidence file spelling relative to the stored run directory.
    pub(crate) file: String,
    /// One-based first raw line used by the observation.
    pub(crate) line: usize,
    /// One-based inclusive final raw line used by the observation.
    pub(crate) end_line: usize,
}

/// Event-v2 normalized observation derived from raw syscall evidence.
///
/// An event records what was observed, not why it happened or whether two path
/// spellings identify the same physical object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Event {
    /// Persisted event schema version; current writers use version 2.
    pub(crate) schema_version: u32,
    /// Raw numeric task-ID spelling reported by strace.
    pub(crate) pid: String,
    /// Normalized observation family such as `file`, `exec`, or `spawn`.
    pub(crate) kind: String,
    /// Observed syscall operation name.
    pub(crate) operation: String,
    /// Observed path spelling, when the syscall evidence establishes one.
    pub(crate) path: Option<String>,
    /// Normalized syscall result or raw process-completion spelling.
    pub(crate) outcome: String,
    /// Raw evidence coordinates retained without reinterpretation.
    pub(crate) evidence: Evidence,
    /// Observed execution arguments, when completely decoded.
    #[serde(default)]
    pub(crate) argv: Option<Vec<String>>,
    /// Successful spawn result when it establishes a child task ID.
    #[serde(default)]
    pub(crate) child_pid: Option<String>,
    /// Last observed/lexically described CWD context, never current physical identity.
    /// Older records may have used this context to derive absolute path spellings.
    #[serde(default)]
    pub(crate) cwd_before: Option<String>,
    /// Unresolved relative path or shared/changed filesystem context.
    /// False does not certify physical path identity (especially in older records).
    #[serde(default)]
    pub(crate) context_uncertain: bool,
}

/// Metadata-v3 capture facts stored alongside normalized events.
///
/// Target, collector, capture state, completeness, and warnings remain separate
/// facts. Optional fields also preserve the compatibility shape of older data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Metadata {
    /// Persisted metadata schema version; current writers use version 3.
    pub(crate) schema_version: u32,
    /// RunDelta package version that created the record.
    pub(crate) tool_version: String,
    /// Generated storage identifier.
    pub(crate) id: String,
    /// User-provided record label, never a filesystem path.
    pub(crate) name: String,
    /// Reversible byte spellings of the requested command and arguments.
    pub(crate) command: Vec<String>,
    /// Initial observed working-directory spelling.
    pub(crate) cwd: String,
    /// Capture start timestamp in Unix-epoch milliseconds.
    pub(crate) started: u128,
    /// Capture end timestamp when final facts were produced.
    pub(crate) ended: Option<u128>,
    /// First version line reported by the tracing backend.
    pub(crate) backend: String,
    /// Persisted capture-state spelling.
    pub(crate) state: String,
    /// Explicit target exit code, mutually exclusive with `signal`.
    pub(crate) exit_code: Option<i32>,
    /// Explicit target termination signal, mutually exclusive with `exit_code`.
    pub(crate) signal: Option<i32>,
    /// Capture and parsing limitations in observation order.
    pub(crate) warnings: Vec<String>,
    /// Number of normalized events stored for the run.
    pub(crate) parsed_events: usize,
    /// Number of logical lines observed in the raw trace.
    pub(crate) raw_lines: usize,
    /// None means legacy format did not record an environment selection.
    #[serde(default)]
    pub(crate) environment: Option<std::collections::BTreeMap<String, Option<String>>>,
    /// Persisted evidence-completeness spelling; absent in legacy records.
    #[serde(default)]
    pub(crate) completeness: Option<String>,
    /// Collector exit code, kept distinct from the target outcome.
    #[serde(default)]
    pub(crate) collector_exit_code: Option<i32>,
    /// Collector termination signal, kept distinct from the target outcome.
    #[serde(default)]
    pub(crate) collector_signal: Option<i32>,
    /// Exact root-task completion text when present in raw evidence.
    #[serde(default)]
    pub(crate) target_exit_raw: Option<String>,
}

/// Loaded run facts plus independently classified storage integrity.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Run {
    /// Persisted run metadata.
    pub(crate) metadata: Metadata,
    /// Persisted normalized observations.
    pub(crate) events: Vec<Event>,
    /// Verification result computed while loading the record.
    pub(crate) storage_integrity: StorageIntegrity,
}

/// Encode arbitrary Unix bytes as a reversible ASCII spelling.
pub(crate) fn bytes(v: &[u8]) -> String {
    let mut s = String::new();
    for &b in v {
        match b {
            b' '..=b'~' if b != b'\\' => s.push(b as char),
            _ => s.push_str(&format!("\\x{b:02x}")),
        }
    }
    s
}

/// Storage verification fact kept separate from capture completeness.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StorageIntegrity {
    /// Stable integrity classification such as `verified` or `corrupt`.
    pub(crate) status: String,
    /// Human-readable supporting detail without comparison conclusions.
    pub(crate) detail: String,
}

/// Stable, presentation-neutral classification for a failed storage load.
///
/// This enum is in-memory only. It is not part of metadata v3, event v2,
/// finalization marker v1, or diff report v4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StorageLoadFailureKind {
    /// Supported storage bytes or facts contradict their contract.
    Corrupt,
    /// A valid header names a schema this binary cannot interpret.
    Unsupported,
    /// Storage or another required resource could not be safely accessed.
    Unavailable,
    /// A storage operation could not proceed because its lock was held.
    Busy,
}

/// Neutral load failure passed from persistence to a consuming use case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StorageLoadFailure {
    /// Machine-stable failure class, independent of concrete filesystem errors.
    pub(crate) kind: StorageLoadFailureKind,
    /// Existing diagnostic text, including retained persistence context.
    pub(crate) message: String,
}

/// Result of loading a value without exposing a concrete storage error type.
#[derive(Debug)]
pub(crate) enum LoadOutcome<T> {
    /// The value was loaded and validated.
    Loaded(T),
    /// Loading failed with a neutral classification and diagnostic.
    Failed(StorageLoadFailure),
}

/// Presentation-neutral facts for one row in the record index.
///
/// This in-memory type is not part of the persisted metadata, event, marker,
/// or diff schemas. A readable row retains its complete metadata, while a
/// damaged row exposes only safe index hints and the isolated storage error.
#[derive(Debug, Clone)]
pub(crate) enum RecordSummary {
    /// A record whose metadata and finalized structured evidence were readable.
    Readable {
        /// Persisted metadata, unchanged from the record schema.
        metadata: Box<Metadata>,
        /// Stable display spelling relative to the storage root.
        directory: String,
        /// Verification result for the structured record.
        storage_integrity: StorageIntegrity,
    },
    /// A damaged or unsupported record that could not be loaded.
    Unreadable {
        /// Safe directory-derived ID hint, when the leaf is recognizable.
        id: Option<String>,
        /// Valid label hint decoded from metadata without trusting the record.
        name: Option<String>,
        /// Escaped display spelling relative to the storage root.
        directory: String,
        /// Isolated corruption, compatibility, or availability classification.
        storage_integrity: StorageIntegrity,
    },
}
