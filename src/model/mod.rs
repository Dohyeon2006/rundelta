//! I/O-free domain and wire representations shared inside the binary crate.
//!
//! Serializable structs in this module mirror the frozen metadata, event, and
//! report-adjacent storage contracts. They carry observed facts and evidence;
//! they do not infer causation, physical file identity, or capture reliability.
//!
//! These structs are wire-shaped data, not validated constructors. Deserializing
//! them establishes field shape only. Pure validators below check the versioned
//! fact contracts; storage adds directory, checksum and publication checks, while
//! `capture` derives completion from current trace evidence.
//! String-valued states and optional legacy fields are retained for compatibility,
//! so callers must not infer valid combinations from Rust field types alone.

use serde::{Deserialize, Serialize};

/// Recording protocol chosen explicitly at the process boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordingEvidenceContract {
    /// Existing structured-only metadata3/events2/marker1 protocol.
    StructuredV3,
    /// Opt-in metadata4/events3/raw-manifest1/marker2 protocol.
    TraceV1,
}

/// Frozen target selection over saved normalized event coordinates.
pub(crate) const TARGET_SELECTION_RULE_V1: &str = "first_successful_exec_evidence_order_v1";
/// Trace-local PID lifetime annotations, never cross-run comparison identity.
pub(crate) const TASK_IDENTITY_CONTRACT_V1: &str = "task_generations_evidence_order_v1";

/// One saved trace-local lifetime of a numeric PID.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct TaskIdentity {
    /// Observed numeric PID, without a post-run OS lookup.
    pub(crate) pid: String,
    /// One-based ordinal for this PID inside this trace.
    pub(crate) generation: usize,
}

/// Exact normalized vector entry and its unchanged original evidence coordinates.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct EventReference {
    /// Zero-based index in the persisted event vector, not evidence order.
    pub(crate) index: usize,
    /// Original one-based raw coordinates copied during capture.
    pub(crate) evidence: Evidence,
}

/// Capture-time target selection retained without replaying a newer parser.
///
/// Saved generation labels and raw checksums establish reference consistency,
/// not correctness of every parser inference or authentication of a real run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct TargetProvenance {
    /// Provenance contract version, currently one.
    pub(crate) schema_version: u32,
    /// Frozen selection rule; unknown rules are unsupported, not guessed.
    pub(crate) selection_rule: String,
    /// Frozen meaning of event generation annotations.
    pub(crate) identity_contract: String,
    /// Selected root task lifetime.
    pub(crate) root: TaskIdentity,
    /// First successful exec in raw evidence order.
    pub(crate) root_exec: EventReference,
    /// Observed exit of that same saved lifetime.
    pub(crate) target_exit: EventReference,
}

/// Raw input receipt produced by the same traversal that feeds normalization.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum RawReceipt {
    /// UTF-8 input was consumed to EOF; digest includes original framing bytes.
    Parsed {
        /// Total consumed byte count.
        bytes: u64,
        /// Logical line count including an unterminated final line.
        logical_lines: usize,
        /// Lower-case SHA-256 of the actual normalization input.
        sha256: String,
    },
    /// Skipped or failed input must never acquire a fabricated complete receipt.
    Unverified {
        /// Capture-time limitation, not a retrospective interpretation of raw.
        reason: String,
    },
}

/// Marker-bound raw receipt; hashing alone never authenticates the collector.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RawManifest {
    /// Raw manifest version, currently one.
    pub(crate) schema_version: u32,
    /// Exact record identifier preventing accidental manifest reuse.
    pub(crate) run_id: String,
    /// Fixed evidence leaf, currently raw/trace.log.
    pub(crate) file: String,
    /// Same-stream input receipt or explicit unavailable observation.
    pub(crate) receipt: RawReceipt,
}

/// Loader-computed raw integrity, independent of metadata/events verification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RawIntegrity {
    /// verified, unverified, corrupt or unavailable; never capture success.
    pub(crate) status: String,
    /// Supporting load observation without comparison policy.
    pub(crate) detail: String,
    /// Marker-bound saved receipt when it could be decoded.
    pub(crate) manifest: Option<RawManifest>,
}

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
/// diff JSON schemas. `capture::record` constructs it only after its run writer
/// finalizes successfully; it does not strengthen recorded facts or durability claims.
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
///
/// Coordinates refer to the trace used to create the event, including both lines
/// of an unfinished/resumed syscall. Loading a record does not run a newer parser
/// to relocate them. A structured-file checksum does not cover that raw file, so
/// these references identify supporting material without authenticating it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Evidence {
    /// Evidence file spelling relative to the stored run directory.
    pub(crate) file: String,
    /// One-based first raw line used by the observation.
    pub(crate) line: usize,
    /// One-based inclusive final raw line used by the observation.
    pub(crate) end_line: usize,
}

/// Versioned normalized observation derived from raw syscall evidence.
///
/// An event records what was observed, not why it happened or whether two path
/// spellings identify the same physical object.
///
/// `file` and `exec` paths can be absolute syscall/annotation spellings or explicit
/// unresolved-relative descriptions. `cwd_before` is context, never an authority
/// to join a relative operand into an absolute physical identity. A bare PID is
/// also not an enduring task identity: default v2 keeps generations transient;
/// explicit v3 saves trace-local ordinals without upgrading physical identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Event {
    /// Persisted event schema: default v2, explicit trace-v1 v3.
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
    /// Event-v3 trace-local actor lifetime; absent in historical event formats.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) task_generation: Option<usize>,
    /// Successful spawn's child lifetime, independent of filesystem sharing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) child_generation: Option<usize>,
}

/// Versioned capture facts stored alongside normalized events.
///
/// Target, collector, capture state, completeness, and warnings remain separate
/// facts. Optional fields also preserve the compatibility shape of older data.
///
/// Completion and integrity answer different questions: a finalized capture may
/// still be interrupted or incomplete, and a readable legacy record may remain
/// unverified. Wall-clock timestamps can move backwards and are not used to order
/// syscalls. The normalized evidence coordinates supply observation order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Metadata {
    /// Persisted metadata schema: default v3, explicit trace-v1 v4.
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
    /// Within a present map, a missing key means unselected, `None` means selected
    /// but unset, and `Some(value)` means selected and set. This is startup data,
    /// not an observation of later environment mutation inside the target.
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
    /// Kept even when a future signal spelling cannot be decoded. This field
    /// alone does not save coordinates or lifetime; metadata4's accompanying
    /// target_provenance supplies those capture-time references.
    #[serde(default)]
    pub(crate) target_exit_raw: Option<String>,
    /// Explicit trace-v1 recording contract; absent in metadata1–3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) evidence_contract: Option<String>,
    /// Selected exec/exit coordinates and lifetime copied during this capture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) target_provenance: Option<TargetProvenance>,
}

/// Loaded run facts plus independently classified storage integrity.
///
/// This struct carries the loader's findings; constructing or deserializing a
/// `Run` directly does not itself perform those checks. Consumers must preserve
/// that distinction when using synthetic fixtures or defining new entry points.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Run {
    /// Persisted run metadata.
    pub(crate) metadata: Metadata,
    /// Persisted normalized observations.
    pub(crate) events: Vec<Event>,
    /// Verification result computed while loading the record.
    pub(crate) storage_integrity: StorageIntegrity,
    /// Independent raw load facts; absent for historical structured-only formats.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) raw_integrity: Option<RawIntegrity>,
}

/// Encode arbitrary Unix bytes as a reversible ASCII spelling.
///
/// Backslashes are escaped too, so a literal escape-looking argument cannot be
/// confused with an encoded non-printable byte. This is not JSON escaping or
/// secret redaction; argv/path values remain sensitive after encoding.
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
///
/// `verified` refers to the current structured metadata/events protocol, not raw
/// trace integrity, capture success, origin authentication, or power-loss safety.
/// Keeping this fact beside, rather than inside, capture state prevents one claim
/// from silently standing in for the other.
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
        /// Raw integrity for a new contract, without promoting old record facts.
        raw_integrity: Option<RawIntegrity>,
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

/// Recognize the generated record ID vocabulary without accessing storage.
pub(crate) fn valid_record_id(id: &str) -> bool {
    id.split_once('-').is_some_and(|(time, pid)| {
        !time.is_empty()
            && time.bytes().all(|b| b.is_ascii_hexdigit())
            && u128::from_str_radix(time, 16).is_ok()
            && !pid.is_empty()
            && pid.bytes().all(|b| b.is_ascii_digit())
            && pid.parse::<u32>().is_ok_and(|p| p > 0)
    })
}

/// Validate the shape of one current event without reading or reinterpreting raw evidence.
///
/// This is deliberately narrower than a syscall parser: it checks that an event
/// can participate in the current domain contract. In particular, pathless file
/// and exec events must never disappear from a trusted comparison. Legacy events
/// are validated by their own storage compatibility rules, not by this function.
/// Valid shape proves neither capture completeness nor the origin of the bytes.
pub(crate) fn validate_current_event(event: &Event) -> Result<(), &'static str> {
    if !matches!(event.schema_version, 2 | 3) {
        return Err("current event must use schema 2 or 3");
    }
    if event.schema_version == 2 {
        if event.task_generation.is_some() || event.child_generation.is_some() {
            return Err("event2 cannot claim persistent generation annotations");
        }
    } else if event.task_generation.is_none_or(|value| value == 0)
        || event.child_generation.is_some_and(|value| value == 0)
        || event.child_pid.is_some() != event.child_generation.is_some()
    {
        return Err("event3 requires positive actor and observed-child generations");
    }
    let task_id = |value: &str| {
        !value.is_empty()
            && value.bytes().all(|byte| byte.is_ascii_digit())
            && value.parse::<u32>().is_ok_and(|pid| pid > 0)
    };
    if !task_id(&event.pid) {
        return Err("event PID must be a positive numeric task ID");
    }
    if event.operation.is_empty() || event.outcome.is_empty() {
        return Err("event operation and outcome must be nonempty");
    }
    if event.schema_version == 3 && event.kind == "exit" {
        // Every saved lifetime needs a real completion spelling, including
        // children that are not selected as the target. Unknown legal signal
        // names remain observed completions without invented numeric facts.
        trace_v1_target_fields(&event.outcome)
            .map_err(|_| "event3 exit requires trace-v1 completion framing")?;
    }
    // These are the event-v2 operation/family spellings, not syscall execution
    // or ABI decoding. The parser owns argument positions and collector selection;
    // model validation prevents an impossible family pairing crossing an adapter.
    let family_matches = match event.kind.as_str() {
        "file" => matches!(
            event.operation.as_str(),
            "open" | "openat" | "openat2" | "access" | "faccessat" | "faccessat2"
        ),
        "exec" => matches!(event.operation.as_str(), "execve" | "execveat"),
        "cwd" => matches!(event.operation.as_str(), "chdir" | "fchdir"),
        "spawn" => matches!(
            event.operation.as_str(),
            "clone" | "clone3" | "fork" | "vfork"
        ),
        "context" => matches!(
            event.operation.as_str(),
            "unshare"
                | "setns"
                | "chroot"
                | "pivot_root"
                | "mount"
                | "umount2"
                | "move_mount"
                | "mount_setattr"
        ),
        "exit" => event.operation == "exit",
        _ => false,
    };
    if !family_matches {
        return Err("event operation contradicts its current family");
    }
    if matches!(event.kind.as_str(), "file" | "exec" | "cwd") {
        match event.path.as_deref() {
            Some(path) if path.starts_with('/') => {}
            Some(path) if path.starts_with("relative:") && event.context_uncertain => {}
            None if event.kind == "cwd" && event.context_uncertain => {}
            None | Some("") if event.kind != "cwd" => {
                return Err("file and exec events require a nonempty observed path");
            }
            _ => {
                return Err(
                    "unresolved current path requires explicit relative context and uncertainty",
                );
            }
        }
    }
    if event.evidence.file != "raw/trace.log"
        || event.evidence.line == 0
        || event.evidence.line > event.evidence.end_line
    {
        return Err("current event must retain ordered raw/trace.log evidence coordinates");
    }
    if event.kind != "exec" && event.argv.is_some() {
        return Err("only exec events may carry argv");
    }
    if event.kind != "spawn" && event.child_pid.is_some() {
        return Err("only spawn events may carry a child PID");
    }
    match event.kind.as_str() {
        "file" | "exec" if event.path.as_deref().is_none_or(str::is_empty) => {
            Err("file and exec events require a nonempty observed path")
        }
        "file" | "exec" | "cwd" => Ok(()),
        "spawn" if event.outcome == "success" => {
            match (event.child_pid.as_deref(), event.path.as_deref()) {
                (Some(child), Some(path)) if task_id(child) && path == child => Ok(()),
                _ => {
                    Err("successful spawn must retain the same positive child PID and result path")
                }
            }
        }
        "spawn" if event.child_pid.is_some() => Err("failed spawn must not claim a child PID"),
        "spawn" => Ok(()),
        "context" if event.path.is_none() => Ok(()),
        "exit" if event.path.is_none() && event.operation == "exit" => Ok(()),
        "context" | "exit" => Err("context and exit event shapes contradict their family"),
        _ => Err("unknown current event family"),
    }
}

/// Recheck in-memory comparison inputs against the same pure fact contracts as storage.
///
/// Rust callers can construct wire DTOs directly, so a `verified` string cannot
/// replace validation. This performs no checksum or raw-file I/O and never
/// upgrades historical evidence. The storage adapter remains responsible for
/// proving the supplied integrity classification when loading actual records.
pub(crate) fn validate_comparison_run(run: &Run) -> ContractResult {
    let metadata = &run.metadata;
    if !matches!(
        run.storage_integrity.status.as_str(),
        "verified" | "unverified" | "unfinalized"
    ) {
        return Err(ContractViolation {
            kind: if run.storage_integrity.status == "unsupported" {
                StorageLoadFailureKind::Unsupported
            } else if run.storage_integrity.status == "unavailable" {
                StorageLoadFailureKind::Unavailable
            } else {
                StorageLoadFailureKind::Corrupt
            },
            message: "failed or unknown storage integrity cannot be a loaded comparison input"
                .into(),
        });
    }
    if !matches!(metadata.schema_version, 1..=4)
        || run
            .events
            .iter()
            .any(|event| !matches!(event.schema_version, 1..=3))
    {
        return Err(ContractViolation {
            kind: StorageLoadFailureKind::Unsupported,
            message: "unsupported metadata or event schema in comparison input".into(),
        });
    }
    if metadata.schema_version >= 2
        && (metadata.environment.is_none() || metadata.completeness.is_none())
    {
        return Err(contract_violation(
            "metadata: missing environment or completeness",
        ));
    }
    validate_metadata_contract(metadata)?;
    if metadata.schema_version == 4 {
        let raw = run
            .raw_integrity
            .as_ref()
            .ok_or_else(|| contract_violation("trace-v1 run lacks independent raw integrity"))?;
        if !matches!(
            raw.status.as_str(),
            "verified" | "unverified" | "corrupt" | "unavailable"
        ) {
            return Err(contract_violation("unknown raw integrity classification"));
        }
        if let Some(manifest) = &raw.manifest {
            validate_raw_manifest(manifest, metadata)?;
        }
        // Component status comes from a verified marker-bound receipt or from
        // deliberate withholding. Reject impossible DTO combinations at this
        // pure seam as well as on disk; a status string alone grants no trust.
        let consistent = if run.storage_integrity.status == "unfinalized" {
            raw.status == "unverified" && raw.manifest.is_none() && run.events.is_empty()
        } else if run.storage_integrity.status == "verified" {
            match raw.manifest.as_ref().map(|m| &m.receipt) {
                Some(RawReceipt::Parsed { .. }) => {
                    matches!(raw.status.as_str(), "verified" | "corrupt" | "unavailable")
                }
                Some(RawReceipt::Unverified { .. }) => raw.status == "unverified",
                None => false,
            }
        } else {
            false
        };
        if !consistent {
            return Err(contract_violation(
                "raw component facts contradict structured finalization or receipt",
            ));
        }
    } else if run.raw_integrity.is_some() {
        return Err(contract_violation(
            "historical record cannot claim new raw integrity",
        ));
    }
    if run.storage_integrity.status == "unfinalized"
        && run.events.is_empty()
        && (metadata.schema_version >= 3 || metadata.state == "running")
    {
        // The loader intentionally withholds events without a current marker.
        // Metadata's saved count is not a claim that this Run contains those
        // observations; preserve incomplete status 3 instead of inventing damage.
        return Ok(());
    }
    validate_run_contract(
        metadata,
        &run.events,
        run.storage_integrity.status == "verified",
    )?;
    if run.storage_integrity.status == "verified"
        && (!matches!(metadata.schema_version, 3 | 4) || metadata.state == "running")
    {
        return Err(contract_violation(
            "verified comparison requires current finalized metadata",
        ));
    }
    Ok(())
}

#[derive(Debug)]
/// A pure stored-contract violation, classified as damage only at the read boundary.
pub(crate) struct ContractViolation {
    /// Unsupported vocabulary is a version boundary, distinct from contradictory facts.
    pub(crate) kind: StorageLoadFailureKind,
    message: String,
}

impl std::fmt::Display for ContractViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Result of checking versioned facts without storage or presentation I/O.
pub(crate) type ContractResult = std::result::Result<(), ContractViolation>;

/// Create a contradictory-facts error; I/O adapters choose their boundary mapping.
pub(crate) fn contract_violation(message: impl Into<String>) -> ContractViolation {
    ContractViolation {
        kind: StorageLoadFailureKind::Corrupt,
        message: message.into(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Current metadata state vocabulary; legacy text is not passed through this parser.
pub(crate) enum StoredState {
    /// Only initial facts are present; no finalized outcome may be claimed.
    Running,
    /// A target completion exists, independently of whether evidence is complete.
    Completed,
    /// Capture could not establish a supported complete target run.
    CaptureFailed,
    /// Recorder cancellation is retained as its own capture state.
    Interrupted,
}

impl StoredState {
    /// Interpret the current schema vocabulary without migrating legacy strings.
    pub(crate) fn parse(value: &str) -> std::result::Result<Self, ContractViolation> {
        match value {
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "capture_failed" => Ok(Self::CaptureFailed),
            "interrupted" => Ok(Self::Interrupted),
            _ => Err(contract_violation("unknown capture state")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Current metadata completeness vocabulary, separate from storage integrity.
pub(crate) enum StoredCompleteness {
    /// Initial metadata has not reached a final evidence decision.
    Pending,
    /// Supported evidence is complete and has no recorded limitations.
    CompleteForSupportedEvents,
    /// A target result is known while warnings limit the observations.
    Partial,
    /// Capture supervision or target completion could not be established.
    Incomplete,
}

impl StoredCompleteness {
    /// Interpret the current completeness vocabulary, rejecting absent fields.
    pub(crate) fn parse(value: Option<&str>) -> std::result::Result<Self, ContractViolation> {
        match value {
            Some("pending") => Ok(Self::Pending),
            Some("complete_for_supported_events") => Ok(Self::CompleteForSupportedEvents),
            Some("partial") => Ok(Self::Partial),
            Some("incomplete") => Ok(Self::Incomplete),
            Some(_) => Err(contract_violation("unknown capture completeness")),
            None => Err(contract_violation("completeness is missing")),
        }
    }
}

fn stored_capture_facts(
    metadata: &Metadata,
) -> std::result::Result<(StoredState, StoredCompleteness), ContractViolation> {
    Ok((
        StoredState::parse(&metadata.state)?,
        StoredCompleteness::parse(metadata.completeness.as_deref())?,
    ))
}

/// Validate facts written by the current metadata schema without applying
/// current semantics retroactively to legacy free-form records.
///
/// All supported metadata versions retain explicit-outcome mutual exclusion.
/// V3 adds supported state/completeness combinations and required field facts;
/// it does not parse `target_exit_raw` to prove agreement with numeric fields.
/// The check cannot establish whether the observed capture actually happened.
pub(crate) fn validate_metadata_contract(metadata: &Metadata) -> ContractResult {
    if metadata.schema_version == 4 {
        if metadata.evidence_contract.as_deref() != Some("trace-v1") {
            return Err(contract_violation(
                "metadata4 requires evidence contract trace-v1",
            ));
        }
        if metadata.target_exit_raw.is_some() != metadata.target_provenance.is_some() {
            return Err(contract_violation(
                "trace-v1 target outcome and provenance must occur together",
            ));
        }
        if let Some(outcome) = &metadata.target_exit_raw {
            let expected = trace_v1_target_fields(outcome)?;
            if (metadata.exit_code, metadata.signal) != expected {
                return Err(contract_violation(
                    "trace-v1 numeric target outcome contradicts its saved exit spelling",
                ));
            }
        }
        validate_target_provenance_envelope(metadata)?;
    } else if metadata.evidence_contract.is_some() || metadata.target_provenance.is_some() {
        return Err(contract_violation(
            "historical metadata cannot claim trace-v1 facts",
        ));
    }
    if metadata.exit_code.is_some() && metadata.signal.is_some() {
        return Err(contract_violation(
            "target exit code and signal are mutually exclusive",
        ));
    }
    if metadata.collector_exit_code.is_some() && metadata.collector_signal.is_some() {
        return Err(contract_violation(
            "collector exit code and signal are mutually exclusive",
        ));
    }
    if metadata.schema_version < 3 {
        return Ok(());
    }
    // Current capture uses Linux wait/strace facts: exit codes are unsigned
    // eight-bit values and Linux signal numbers are 1..=64. Numeric 128+signal
    // remains a CLI convention and must never be stored as a signal substitute.
    // Keep legacy numeric spellings untouched; their evidence is not upgraded.
    for (code, signal) in [
        (metadata.exit_code, metadata.signal),
        (metadata.collector_exit_code, metadata.collector_signal),
    ] {
        if code.is_some_and(|value| !(0..=255).contains(&value)) {
            return Err(contract_violation("current exit code must be in 0..=255"));
        }
        if signal.is_some_and(|value| !(1..=64).contains(&value)) {
            return Err(contract_violation("current Linux signal must be in 1..=64"));
        }
    }
    if !valid_record_id(&metadata.id) {
        return Err(contract_violation("ID is not one safe generated component"));
    }
    if metadata.name.is_empty() || metadata.name.chars().any(char::is_control) {
        return Err(contract_violation(
            "name must be nonempty without control characters",
        ));
    }
    if metadata.command.is_empty() {
        return Err(contract_violation("command must contain an executable"));
    }
    if metadata.tool_version.is_empty() || metadata.cwd.is_empty() || metadata.backend.is_empty() {
        return Err(contract_violation(
            "tool version, cwd and backend must be nonempty",
        ));
    }
    let environment = metadata
        .environment
        .as_ref()
        .ok_or_else(|| contract_violation("environment is missing"))?;
    if environment
        .keys()
        .any(|key| key.is_empty() || key.contains('=') || key.chars().any(char::is_control))
    {
        return Err(contract_violation(
            "environment keys must be nonempty without '=' or control characters",
        ));
    }

    let (state, completeness) = stored_capture_facts(metadata)?;
    match (state, completeness) {
        (StoredState::Running, StoredCompleteness::Pending) => {
            if metadata.ended.is_some()
                || metadata.exit_code.is_some()
                || metadata.signal.is_some()
                || metadata.collector_exit_code.is_some()
                || metadata.collector_signal.is_some()
                || metadata.target_exit_raw.is_some()
                || !metadata.warnings.is_empty()
                || metadata.parsed_events != 0
                || metadata.raw_lines != 0
            {
                return Err(contract_violation(
                    "running metadata contains finalized capture facts",
                ));
            }
        }
        (
            StoredState::Completed,
            StoredCompleteness::CompleteForSupportedEvents | StoredCompleteness::Partial,
        ) => {
            if metadata.ended.is_none() {
                return Err(contract_violation("completed metadata has no end time"));
            }
            if completeness == StoredCompleteness::Partial && metadata.warnings.is_empty() {
                return Err(contract_violation(
                    "partial metadata records no evidence limitation",
                ));
            }
            if metadata.target_exit_raw.is_none() {
                return Err(contract_violation(
                    "completed metadata has no raw target outcome",
                ));
            }
            if metadata.collector_exit_code.is_none() && metadata.collector_signal.is_none() {
                return Err(contract_violation(
                    "completed metadata has no collector outcome",
                ));
            }
        }
        (StoredState::CaptureFailed, StoredCompleteness::Incomplete) => {
            if metadata.ended.is_none() {
                return Err(contract_violation("final metadata has no end time"));
            }
            if metadata.warnings.is_empty() {
                return Err(contract_violation(
                    "failed capture records no failure warning",
                ));
            }
        }
        (StoredState::Interrupted, StoredCompleteness::Incomplete) => {
            if metadata.ended.is_none() {
                return Err(contract_violation("final metadata has no end time"));
            }
        }
        (StoredState::Completed, _) => {
            return Err(contract_violation(
                "completed metadata must be complete or partial",
            ));
        }
        (StoredState::CaptureFailed | StoredState::Interrupted, _) => {
            return Err(contract_violation(
                "failed or interrupted capture must be incomplete",
            ));
        }
        (StoredState::Running, _) => {
            return Err(contract_violation("running metadata must be pending"));
        }
    }

    if completeness == StoredCompleteness::CompleteForSupportedEvents
        && !metadata.warnings.is_empty()
    {
        return Err(contract_violation(
            "complete metadata cannot contain capture limitations",
        ));
    }
    if (metadata.exit_code.is_some() || metadata.signal.is_some())
        && metadata.target_exit_raw.is_none()
    {
        return Err(contract_violation(
            "structured target outcome has no raw target outcome",
        ));
    }
    if completeness == StoredCompleteness::CompleteForSupportedEvents
        || (state == StoredState::Completed
            && (metadata.exit_code.is_some() || metadata.signal.is_some()))
    {
        // Partial evidence can still establish a target completion, but a known
        // collector disagreement cannot be called completed. Unknown raw target
        // spellings remain partial observations without inventing a numeric fact.
        let target_matches_collector = match (metadata.exit_code, metadata.signal) {
            (Some(code), None) => {
                metadata.collector_exit_code == Some(code) && metadata.collector_signal.is_none()
            }
            (None, Some(signal)) => {
                metadata.collector_signal == Some(signal) && metadata.collector_exit_code.is_none()
            }
            _ => false,
        };
        if !target_matches_collector {
            return Err(contract_violation(
                "completed metadata lacks a confirmed target outcome",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod contract_tests {
    //! Pure DTO contract tests and shared fixtures; no storage or OS dependency.
    use super::valid_record_id as valid_id;
    use super::*;
    use std::collections::BTreeMap;

    /// Current running metadata shared by pure contract and storage-adapter tests.
    pub(crate) fn metadata_fixture(id: &str) -> Metadata {
        Metadata {
            schema_version: 3,
            tool_version: "test".into(),
            id: id.into(),
            name: "test".into(),
            command: vec!["true".into()],
            cwd: "/".into(),
            started: 1,
            ended: None,
            backend: "test".into(),
            state: "running".into(),
            exit_code: None,
            signal: None,
            warnings: vec![],
            parsed_events: 0,
            raw_lines: 0,
            environment: Some(BTreeMap::new()),
            completeness: Some("pending".into()),
            collector_exit_code: None,
            collector_signal: None,
            target_exit_raw: None,
            evidence_contract: None,
            target_provenance: None,
        }
    }

    /// One exit observation retaining the same coordinates across schema variants.
    pub(crate) fn event_fixture(schema_version: u32) -> Event {
        Event {
            schema_version,
            pid: "1".into(),
            kind: "exit".into(),
            operation: "exit".into(),
            path: None,
            outcome: "+++ exited with 0 +++".into(),
            evidence: Evidence {
                file: "raw/trace.log".into(),
                line: 1,
                end_line: 1,
            },
            argv: None,
            child_pid: None,
            cwd_before: None,
            context_uncertain: false,
            task_generation: None,
            child_generation: None,
        }
    }

    /// Completed one-event metadata preserving an opaque raw outcome as evidence.
    pub(crate) fn completed_metadata_fixture(id: &str) -> Metadata {
        let mut metadata = metadata_fixture(id);
        metadata.state = "completed".into();
        // Wall-clock time may move backwards; ordering is not a storage invariant.
        metadata.ended = Some(0);
        metadata.exit_code = Some(0);
        metadata.collector_exit_code = Some(0);
        metadata.target_exit_raw = Some("opaque target exit spelling".into());
        metadata.completeness = Some("complete_for_supported_events".into());
        metadata.parsed_events = 1;
        metadata.raw_lines = 1;
        metadata
    }

    #[test]
    fn current_contract_accepts_each_real_writer_state_and_opaque_raw_outcomes() {
        let event = event_fixture(2);
        let running = metadata_fixture("abc-1");
        validate_metadata_contract(&running).unwrap();

        let completed = completed_metadata_fixture("abc-1");
        validate_run_contract(&completed, std::slice::from_ref(&event), true).unwrap();

        let mut partial = completed.clone();
        partial.exit_code = None;
        partial.completeness = Some("partial".into());
        partial.target_exit_raw = Some("+++ killed by SIG_FUTURE +++".into());
        partial.warnings = vec!["unrecognized target termination signal".into()];
        validate_run_contract(&partial, std::slice::from_ref(&event), true).unwrap();

        let mut failed = completed.clone();
        failed.state = "capture_failed".into();
        failed.completeness = Some("incomplete".into());
        failed.warnings = vec!["collector status does not confirm target completion".into()];
        validate_run_contract(&failed, std::slice::from_ref(&event), true).unwrap();

        let mut interrupted = completed;
        interrupted.state = "interrupted".into();
        interrupted.completeness = Some("incomplete".into());
        validate_run_contract(&interrupted, &[event], true).unwrap();
    }

    #[test]
    fn metadata_contract_rejects_outcome_state_and_writer_fact_contradictions() {
        let completed = completed_metadata_fixture("abc-1");
        let mut cases = Vec::new();

        let mut target_both = completed.clone();
        target_both.signal = Some(9);
        cases.push(("target outcome", target_both));

        let mut collector_both = completed.clone();
        collector_both.collector_signal = Some(9);
        cases.push(("collector outcome", collector_both));

        let mut running_ended = metadata_fixture("abc-1");
        running_ended.ended = Some(2);
        cases.push(("running ended", running_ended));

        let mut running_pending = metadata_fixture("abc-1");
        running_pending.completeness = Some("partial".into());
        cases.push(("running completeness", running_pending));

        let mut running_outcome = metadata_fixture("abc-1");
        running_outcome.exit_code = Some(0);
        running_outcome.target_exit_raw = Some("opaque".into());
        cases.push(("running outcome", running_outcome));

        let mut completed_pending = completed.clone();
        completed_pending.completeness = Some("pending".into());
        cases.push(("completed completeness", completed_pending));

        let mut completed_without_end = completed.clone();
        completed_without_end.ended = None;
        cases.push(("completed end", completed_without_end));

        let mut structured_without_raw = completed.clone();
        structured_without_raw.target_exit_raw = None;
        cases.push(("structured target without raw", structured_without_raw));

        let mut complete_with_warning = completed.clone();
        complete_with_warning.warnings.push("limitation".into());
        cases.push(("complete warning", complete_with_warning));

        let mut complete_without_target = completed.clone();
        complete_without_target.exit_code = None;
        complete_without_target.target_exit_raw = Some("opaque unknown".into());
        cases.push(("complete target confirmation", complete_without_target));

        let mut complete_mismatch = completed.clone();
        complete_mismatch.collector_exit_code = Some(7);
        cases.push(("complete collector confirmation", complete_mismatch));

        let mut partial_without_limitation = completed.clone();
        partial_without_limitation.completeness = Some("partial".into());
        cases.push(("partial limitation", partial_without_limitation));

        let mut partial_without_raw = completed.clone();
        partial_without_raw.completeness = Some("partial".into());
        partial_without_raw.warnings = vec!["unknown target outcome".into()];
        partial_without_raw.exit_code = None;
        partial_without_raw.target_exit_raw = None;
        cases.push(("partial raw target", partial_without_raw));

        let mut partial_without_collector = completed.clone();
        partial_without_collector.completeness = Some("partial".into());
        partial_without_collector.warnings = vec!["unknown target outcome".into()];
        partial_without_collector.exit_code = None;
        partial_without_collector.collector_exit_code = None;
        cases.push(("partial collector outcome", partial_without_collector));

        let mut failed_complete = completed.clone();
        failed_complete.state = "capture_failed".into();
        failed_complete.warnings = vec!["failure".into()];
        cases.push(("failed completeness", failed_complete));

        let mut failed_without_warning = completed.clone();
        failed_without_warning.state = "capture_failed".into();
        failed_without_warning.completeness = Some("incomplete".into());
        cases.push(("failed warning", failed_without_warning));

        let mut interrupted_complete = completed.clone();
        interrupted_complete.state = "interrupted".into();
        cases.push(("interrupted completeness", interrupted_complete));

        let mut empty_command = completed.clone();
        empty_command.command.clear();
        cases.push(("empty command", empty_command));

        let mut bad_name = completed.clone();
        bad_name.name = "bad\nname".into();
        cases.push(("bad name", bad_name));

        let mut bad_environment = completed.clone();
        bad_environment
            .environment
            .as_mut()
            .unwrap()
            .insert("BAD=KEY".into(), None);
        cases.push(("bad environment", bad_environment));

        let mut unknown_state = completed;
        unknown_state.state = "future_state".into();
        cases.push(("unknown state", unknown_state));

        let mut unknown_completeness = completed_metadata_fixture("abc-1");
        unknown_completeness.completeness = Some("future_completeness".into());
        cases.push(("unknown completeness", unknown_completeness));

        for (label, metadata) in cases {
            assert!(
                validate_metadata_contract(&metadata).is_err(),
                "{label} must fail closed"
            );
        }
    }

    #[test]
    fn run_contract_rejects_count_coordinate_and_completeness_contradictions() {
        let metadata = completed_metadata_fixture("abc-1");
        let event = event_fixture(2);

        let mut wrong_count = metadata.clone();
        wrong_count.parsed_events = 2;
        assert!(validate_run_contract(&wrong_count, std::slice::from_ref(&event), true).is_err());

        for (label, line, end_line, raw_lines) in [
            ("zero line", 0, 1, 1),
            ("reversed range", 2, 1, 2),
            ("past raw trace", 1, 2, 1),
        ] {
            let mut invalid = event.clone();
            invalid.evidence.line = line;
            invalid.evidence.end_line = end_line;
            let mut metadata = metadata.clone();
            metadata.raw_lines = raw_lines;
            assert!(
                validate_run_contract(&metadata, &[invalid], true).is_err(),
                "{label} must fail closed"
            );
        }

        let mut uncertain = event;
        uncertain.context_uncertain = true;
        assert!(validate_run_contract(&metadata, &[uncertain], true).is_err());
    }

    #[test]
    fn legacy_contract_checks_only_objective_outcome_count_and_coordinates() {
        let mut legacy = completed_metadata_fixture("abc-1");
        legacy.schema_version = 1;
        legacy.environment = None;
        legacy.completeness = None;
        legacy.warnings = vec!["legacy free-form warning".into()];
        let event = event_fixture(1);
        validate_run_contract(&legacy, std::slice::from_ref(&event), false).unwrap();

        let mut contradictory = legacy.clone();
        contradictory.signal = Some(9);
        assert!(validate_metadata_contract(&contradictory).is_err());

        let mut wrong_count = legacy.clone();
        wrong_count.parsed_events = 2;
        assert!(validate_run_contract(&wrong_count, std::slice::from_ref(&event), false).is_err());

        let mut invalid_evidence = event;
        invalid_evidence.evidence.end_line = 2;
        assert!(validate_run_contract(&legacy, &[invalid_evidence], false).is_err());
    }

    #[test]
    fn recovery_accepts_only_generated_id_shape() {
        for id in ["18d6e85e2b868e79-24369", "abc-1"] {
            assert!(valid_id(id));
        }
        for id in [
            "../abc-1",
            "/abc-1",
            "abc-1/child",
            "abc-0",
            "abc--1",
            "not-a-run-id",
            "",
            "abc-999999999999999",
            "abc-1\n",
        ] {
            assert!(!valid_id(id), "{id:?}");
        }
    }

    fn validate_run_contract(
        metadata: &Metadata,
        events: &[Event],
        finalized: bool,
    ) -> ContractResult {
        let status = if finalized && metadata.schema_version == 3 {
            "verified"
        } else {
            "unverified"
        };
        validate_comparison_run(&Run {
            metadata: metadata.clone(),
            events: events.to_vec(),
            storage_integrity: StorageIntegrity {
                status: status.into(),
                detail: "pure fixture".into(),
            },
            raw_integrity: None,
        })
    }
}

/// Aggregate event facts shared by storage readback and pure comparison validation.
///
/// Flags describe saved DTOs, not reconstructed syscalls or raw authenticity.
/// Fields stay private so adapters cannot rewrite a summary after observing it.
/// Legacy metadata deliberately does not inherit current shape/completeness rules.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct EventFacts {
    count: usize,
    first_unsupported_schema: Option<u32>,
    has_non_v2: bool,
    has_non_v3: bool,
    has_v3: bool,
    invalid_evidence_order: bool,
    max_end_line: usize,
    context_uncertain: bool,
    first_invalid_current_shape: Option<&'static str>,
    exec_argv_missing: bool,
    incomplete_observation: bool,
}

impl EventFacts {
    /// Observe one DTO while retaining only facts needed by the versioned contract.
    ///
    /// An absolute spelling is not proof of physical identity; a nonabsolute or
    /// missing spelling is sufficient to rule out a complete path observation.
    pub(crate) fn observe(&mut self, event: &Event) {
        self.count += 1;
        if !matches!(event.schema_version, 1..=3) && self.first_unsupported_schema.is_none() {
            self.first_unsupported_schema = Some(event.schema_version);
        }
        self.has_non_v2 |= event.schema_version != 2;
        self.has_non_v3 |= event.schema_version != 3;
        self.has_v3 |= event.schema_version == 3;
        self.invalid_evidence_order |=
            event.evidence.line == 0 || event.evidence.line > event.evidence.end_line;
        self.max_end_line = self.max_end_line.max(event.evidence.end_line);
        self.context_uncertain |= event.context_uncertain;
        if self.first_invalid_current_shape.is_none() {
            self.first_invalid_current_shape = validate_current_event(event).err();
        }
        self.exec_argv_missing |= event.kind == "exec" && event.argv.is_none();
        self.incomplete_observation |= self.exec_argv_missing
            || (matches!(event.kind.as_str(), "file" | "exec" | "cwd")
                && event
                    .path
                    .as_deref()
                    .is_none_or(|path| !path.starts_with('/')));
    }

    /// Summarize a supplied slice without interpreting raw evidence or retaining DTOs.
    pub(crate) fn from_events(events: &[Event]) -> Self {
        let mut facts = Self::default();
        for event in events {
            facts.observe(event);
        }
        facts
    }

    /// Number of observed DTOs; this does not establish normalization completeness.
    pub(crate) fn count(&self) -> usize {
        self.count
    }

    /// First version outside the readable event-schema vocabulary.
    pub(crate) fn first_unsupported_schema(&self) -> Option<u32> {
        self.first_unsupported_schema
    }

    /// Whether a current metadata tuple contains an older or unsupported event.
    pub(crate) fn has_non_v2(&self) -> bool {
        self.has_non_v2
    }
    /// New trace contracts require every persisted event to retain its generation.
    pub(crate) fn has_non_v3(&self) -> bool {
        self.has_non_v3
    }
    /// Older formats must not silently accept generation-bearing observations.
    pub(crate) fn has_v3(&self) -> bool {
        self.has_v3
    }
}

/// Validate the same aggregate contract for a writer, reader or pure comparison.
///
/// Bounds use the recorded raw line count, never a filesystem lookup. Complete
/// current metadata cannot claim fully decoded argv/path evidence when DTO fields
/// demonstrate missing information, even if their uncertainty boolean is false.
/// Partial/incomplete records retain those observations, and legacy rules remain
/// count/coordinate compatibility checks rather than retrospective reinterpretation.
pub(crate) fn validate_run_contract_facts(
    metadata: &Metadata,
    events: &EventFacts,
    finalized: bool,
) -> ContractResult {
    validate_metadata_contract(metadata)?;
    if metadata.schema_version >= 3
        && finalized
        && StoredState::parse(&metadata.state)? == StoredState::Running
    {
        return Err(contract_violation(
            "finalization marker contradicts running metadata",
        ));
    }
    if metadata.schema_version == 3 && events.has_non_v2 {
        return Err(contract_violation(
            "current metadata schema requires only event schema 2",
        ));
    }
    if (metadata.schema_version == 4 && events.has_non_v3)
        || (metadata.schema_version < 3 && events.has_v3)
    {
        return Err(contract_violation(
            "metadata/event version tuple is inconsistent",
        ));
    }
    if metadata.parsed_events != events.count {
        return Err(contract_violation(
            "metadata parsed event count does not match events",
        ));
    }
    if events.invalid_evidence_order || events.max_end_line > metadata.raw_lines {
        return Err(contract_violation(
            "event evidence lines fall outside the recorded raw trace",
        ));
    }
    if metadata.schema_version < 3 {
        return Ok(());
    }
    if let Some(error) = events.first_invalid_current_shape {
        return Err(contract_violation(format!("current event shape: {error}")));
    }
    if StoredCompleteness::parse(metadata.completeness.as_deref())?
        == StoredCompleteness::CompleteForSupportedEvents
    {
        if events.context_uncertain {
            return Err(contract_violation(
                "complete metadata contains context-uncertain evidence",
            ));
        }
        if events.exec_argv_missing {
            return Err(contract_violation(
                "complete metadata contains exec evidence without decoded argv",
            ));
        }
        if events.incomplete_observation {
            return Err(contract_violation(
                "complete metadata contains missing or nonabsolute path evidence",
            ));
        }
    }
    Ok(())
}

/// Validate a saved event vector, including opt-in provenance references.
///
/// The trace-v1 rule is fixed over saved DTOs. This does not run a newer raw
/// parser, change generation assignments, or reinterpret historical formats.
pub(crate) fn validate_run_contract(
    metadata: &Metadata,
    events: &[Event],
    finalized: bool,
) -> ContractResult {
    validate_run_contract_facts(metadata, &EventFacts::from_events(events), finalized)?;
    if metadata.schema_version == 4 {
        validate_trace_provenance(metadata, events)?;
    }
    Ok(())
}

/// Check marker-bound receipt facts without touching the raw filesystem.
pub(crate) fn validate_raw_manifest(manifest: &RawManifest, metadata: &Metadata) -> ContractResult {
    if manifest.schema_version != 1 {
        return Err(unsupported_contract("unsupported raw manifest version"));
    }
    if metadata.schema_version != 4
        || manifest.run_id != metadata.id
        || manifest.file != "raw/trace.log"
    {
        return Err(contract_violation(
            "raw manifest identity or metadata tuple mismatch",
        ));
    }
    match &manifest.receipt {
        RawReceipt::Parsed {
            bytes,
            logical_lines,
            sha256,
        } => {
            if sha256.len() != 64
                || !sha256
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            {
                return Err(contract_violation(
                    "raw receipt SHA-256 must be lower-case hexadecimal",
                ));
            }
            if *logical_lines != metadata.raw_lines
                || (*bytes == 0) != (*logical_lines == 0)
                || (*logical_lines as u128) > (*bytes as u128)
            {
                return Err(contract_violation(
                    "raw receipt byte/line counts contradict metadata",
                ));
            }
        }
        RawReceipt::Unverified { reason } => {
            if reason.is_empty()
                || metadata.target_provenance.is_some()
                || metadata.parsed_events != 0
                || metadata.raw_lines != 0
                || metadata.completeness.as_deref() != Some("incomplete")
            {
                return Err(contract_violation(
                    "unverified raw input cannot supply parsed target evidence",
                ));
            }
        }
    }
    Ok(())
}

fn unsupported_contract(message: &str) -> ContractViolation {
    ContractViolation {
        kind: StorageLoadFailureKind::Unsupported,
        message: message.into(),
    }
}

/// Check saved reference shapes even when an unfinalized record withholds events.
/// These bounds concern metadata's own claims, not the unavailable vector or raw
/// file. Matching referenced observations remains a separate finalized check.
fn validate_target_provenance_envelope(metadata: &Metadata) -> ContractResult {
    let Some(provenance) = &metadata.target_provenance else {
        return Ok(());
    };
    if provenance.schema_version != 1
        || provenance.selection_rule != TARGET_SELECTION_RULE_V1
        || provenance.identity_contract != TASK_IDENTITY_CONTRACT_V1
    {
        return Err(unsupported_contract(
            "unsupported target provenance or identity contract",
        ));
    }
    if provenance.root.generation == 0
        || provenance.root.pid.is_empty()
        || !provenance
            .root
            .pid
            .bytes()
            .all(|byte| byte.is_ascii_digit())
        || !provenance.root.pid.parse::<u32>().is_ok_and(|pid| pid > 0)
    {
        return Err(contract_violation(
            "target provenance requires a positive numeric task lifetime",
        ));
    }
    for reference in [&provenance.root_exec, &provenance.target_exit] {
        if reference.index >= metadata.parsed_events
            || reference.evidence.file != "raw/trace.log"
            || reference.evidence.line == 0
            || reference.evidence.line > reference.evidence.end_line
            || reference.evidence.end_line > metadata.raw_lines
        {
            return Err(contract_violation(
                "target provenance references exceed saved event or evidence bounds",
            ));
        }
    }
    let order = |reference: &EventReference| {
        (
            reference.evidence.line,
            reference.evidence.end_line,
            reference.index,
        )
    };
    // A saved role cannot be both an exec and an exit. Compare the frozen
    // evidence order, not vector order: resumed calls may be inserted later.
    if provenance.root_exec.index == provenance.target_exit.index
        || order(&provenance.root_exec) >= order(&provenance.target_exit)
    {
        return Err(contract_violation(
            "target exit must follow a distinct selected exec reference",
        ));
    }
    Ok(())
}

/// Validate the stored trace-local annotations and the selected target references.
///
/// Coordinates, vector identity and persisted PID lifetime must agree. Complete
/// evidence also requires an exit for every introduced lifetime. Checking
/// them is a consistency proof for saved observations, not origin authentication
/// or a claim that an inferred generation is a physical process identity.
pub(crate) fn validate_trace_provenance(metadata: &Metadata, events: &[Event]) -> ContractResult {
    let mut ordered: Vec<usize> = (0..events.len()).collect();
    ordered.sort_by_key(|&i| (events[i].evidence.line, events[i].evidence.end_line, i));
    let mut latest = std::collections::BTreeMap::<String, usize>::new();
    let mut active = std::collections::BTreeMap::<String, (usize, bool)>::new();
    let mut missing_exits = std::collections::BTreeSet::<(String, usize)>::new();
    let mut has_initial = false;
    let mut identity_uncertain = false;
    for &index in &ordered {
        let event = &events[index];
        let actor = event.task_generation.unwrap_or(0);
        // Check the frozen saved-annotation contract, not syscall identity. An
        // actor keeps its active ordinal until exit; reappearance allocates the
        // next one. A child introduction always allocates, including collisions.
        let (expected, untrusted) = active.get(&event.pid).copied().unwrap_or_else(|| {
            let ordinal = latest
                .get(&event.pid)
                .copied()
                .unwrap_or(0)
                .saturating_add(1);
            // Only the first observed actor may be Initial. Later actors without
            // an active spawn introduction are Unintroduced or Reappeared under
            // this fixed contract; no filesystem/context inference is required.
            let untrusted = has_initial;
            has_initial = true;
            (ordinal, untrusted)
        });
        if actor == 0 || actor != expected {
            return Err(contract_violation(
                "saved actor generation contradicts its observed lifetime",
            ));
        }
        if untrusted && !event.context_uncertain {
            return Err(contract_violation(
                "untrusted saved task lifetime lost its identity uncertainty",
            ));
        }
        identity_uncertain |= untrusted;
        latest.insert(event.pid.clone(), actor);
        active.insert(event.pid.clone(), (actor, untrusted));
        missing_exits.insert((event.pid.clone(), actor));
        if event.kind == "exit" {
            active.remove(&event.pid);
            missing_exits.remove(&(event.pid.clone(), actor));
        }
        if let Some(child) = &event.child_pid {
            let ordinal = event.child_generation.unwrap_or(0);
            let expected = latest.get(child).copied().unwrap_or(0).saturating_add(1);
            if ordinal == 0 || ordinal != expected {
                return Err(contract_violation(
                    "saved child generation lacks its next introduction",
                ));
            }
            let collision = active.contains_key(child);
            if collision && !event.context_uncertain {
                return Err(contract_violation(
                    "colliding spawn lost its saved identity uncertainty",
                ));
            }
            identity_uncertain |= collision;
            latest.insert(child.clone(), ordinal);
            active.insert(child.clone(), (ordinal, collision));
            // A collision replaces the active ordinal, but cannot erase the
            // former lifetime's missing completion evidence.
            missing_exits.insert((child.clone(), ordinal));
        }
    }
    if !missing_exits.is_empty()
        && (metadata.completeness.as_deref() == Some("complete_for_supported_events")
            || metadata.warnings.is_empty())
    {
        return Err(contract_violation(
            "saved task lifetimes without exits require a warning and reduced completeness",
        ));
    }
    if identity_uncertain
        && (metadata.completeness.as_deref() == Some("complete_for_supported_events")
            || metadata.warnings.is_empty())
    {
        return Err(contract_violation(
            "saved identity limitations require a warning and reduced completeness",
        ));
    }
    let first_exec = ordered
        .iter()
        .copied()
        .find(|&i| events[i].kind == "exec" && events[i].outcome == "success");
    let observed_root_exit = first_exec.and_then(|root| {
        ordered.iter().copied().find(|&i| {
            events[i].kind == "exit"
                && events[i].pid == events[root].pid
                && events[i].task_generation == events[root].task_generation
        })
    });
    let Some(provenance) = &metadata.target_provenance else {
        if observed_root_exit.is_some() {
            return Err(contract_violation(
                "saved root exit requires target outcome and provenance",
            ));
        }
        return Ok(());
    };
    validate_target_provenance_envelope(metadata)?;
    let referenced = |reference: &EventReference| -> Result<&Event, ContractViolation> {
        let event = events
            .get(reference.index)
            .ok_or_else(|| contract_violation("target provenance index is out of bounds"))?;
        if event.evidence != reference.evidence {
            return Err(contract_violation(
                "target provenance changed original evidence coordinates",
            ));
        }
        if event.pid != provenance.root.pid
            || event.task_generation != Some(provenance.root.generation)
        {
            return Err(contract_violation(
                "target provenance references another task lifetime",
            ));
        }
        Ok(event)
    };
    let exec = referenced(&provenance.root_exec)?;
    let exit = referenced(&provenance.target_exit)?;
    if exec.kind != "exec"
        || exec.outcome != "success"
        || first_exec != Some(provenance.root_exec.index)
    {
        return Err(contract_violation(
            "target provenance contradicts frozen root exec selection",
        ));
    }
    let selected_exit = ordered.iter().copied().find(|&i| {
        events[i].kind == "exit"
            && events[i].pid == provenance.root.pid
            && events[i].task_generation == Some(provenance.root.generation)
    });
    if exit.kind != "exit"
        || selected_exit != Some(provenance.target_exit.index)
        || metadata.target_exit_raw.as_deref() != Some(exit.outcome.as_str())
    {
        return Err(contract_violation(
            "target outcome does not reference the selected lifetime exit",
        ));
    }
    Ok(())
}

/// Project a saved completion spelling under the frozen trace-v1 wire contract.
///
/// This is an I/O-free consistency rule for metadata4, not a new raw trace
/// parser or a host ABI lookup. Trace-v1's supported Linux x86_64 names map to
/// standard signals 1..=31, strace SIGRT_n to 32+n, and glibc SIGRTMIN to 34.
/// Unknown legal names retain an observed exit with no invented numeric fact.
/// Historical metadata1–3 never uses this interpretation of its opaque text.
pub(crate) fn trace_v1_target_fields(
    outcome: &str,
) -> Result<(Option<i32>, Option<i32>), ContractViolation> {
    if let Some(value) = outcome
        .strip_prefix("+++ exited with ")
        .and_then(|value| value.strip_suffix(" +++"))
    {
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(contract_violation("invalid trace-v1 exit code spelling"));
        }
        let code = value
            .parse::<u8>()
            .map_err(|_| contract_violation("trace-v1 exit code is outside 0..=255"))?;
        return Ok((Some(i32::from(code)), None));
    }
    let spelling = outcome
        .strip_prefix("+++ killed by ")
        .and_then(|value| value.strip_suffix(" +++"))
        .ok_or_else(|| contract_violation("invalid trace-v1 completion framing"))?;
    let name = spelling.strip_suffix(" (core dumped)").unwrap_or(spelling);
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_+-".contains(&byte))
    {
        return Err(contract_violation(
            "invalid trace-v1 termination signal spelling",
        ));
    }
    let signal = match name {
        "SIGHUP" => Some(1),
        "SIGINT" => Some(2),
        "SIGQUIT" => Some(3),
        "SIGILL" => Some(4),
        "SIGTRAP" => Some(5),
        "SIGABRT" => Some(6),
        "SIGBUS" => Some(7),
        "SIGFPE" => Some(8),
        "SIGKILL" => Some(9),
        "SIGUSR1" => Some(10),
        "SIGSEGV" => Some(11),
        "SIGUSR2" => Some(12),
        "SIGPIPE" => Some(13),
        "SIGALRM" => Some(14),
        "SIGTERM" => Some(15),
        "SIGSTKFLT" => Some(16),
        "SIGCHLD" => Some(17),
        "SIGCONT" => Some(18),
        "SIGSTOP" => Some(19),
        "SIGTSTP" => Some(20),
        "SIGTTIN" => Some(21),
        "SIGTTOU" => Some(22),
        "SIGURG" => Some(23),
        "SIGXCPU" => Some(24),
        "SIGXFSZ" => Some(25),
        "SIGVTALRM" => Some(26),
        "SIGPROF" => Some(27),
        "SIGWINCH" => Some(28),
        "SIGIO" => Some(29),
        "SIGPWR" => Some(30),
        "SIGSYS" => Some(31),
        "SIGRTMIN" => Some(34),
        "SIGRTMAX" => Some(64),
        _ => name
            .strip_prefix("SIGRT_")
            .and_then(|offset| offset.parse::<i32>().ok())
            .filter(|offset| (0..=32).contains(offset))
            .map(|offset| 32 + offset)
            .or_else(|| {
                name.strip_prefix("SIGRTMIN+")
                    .and_then(|offset| offset.parse::<i32>().ok())
                    .filter(|offset| (0..=30).contains(offset))
                    .map(|offset| 34 + offset)
            }),
    };
    Ok((None, signal))
}

#[cfg(test)]
pub(crate) mod trace_contract_tests {
    //! Synthetic DTO consistency and fail-closed evidence contracts, without I/O.
    use super::*;

    fn observation(
        kind: &str,
        operation: &str,
        path: Option<&str>,
        outcome: &str,
        line: usize,
    ) -> Event {
        Event {
            schema_version: 3,
            pid: "7".into(),
            kind: kind.into(),
            operation: operation.into(),
            path: path.map(str::to_owned),
            outcome: outcome.into(),
            evidence: Evidence {
                file: "raw/trace.log".into(),
                line,
                end_line: line,
            },
            argv: (kind == "exec").then(|| vec!["tool".into()]),
            child_pid: None,
            cwd_before: None,
            context_uncertain: false,
            task_generation: Some(1),
            child_generation: None,
        }
    }

    /// Hand-authored legal DTO for pure model/diff/report tests.
    ///
    /// Integrity statuses are fixture assertions, not filesystem verification.
    /// The placeholder digest represents a syntactically valid saved receipt;
    /// no test may use this DTO as proof of real raw bytes or capture origin.
    pub(crate) fn run_fixture() -> Run {
        let events = vec![
            observation("exec", "execve", Some("/tool"), "success", 1),
            observation("file", "openat", Some("/input"), "success", 2),
            observation("exit", "exit", None, "+++ exited with 0 +++", 3),
        ];
        let metadata = Metadata {
            schema_version: 4,
            tool_version: "test".into(),
            id: "abc001-101".into(),
            name: "synthetic-trace".into(),
            command: vec!["tool".into()],
            cwd: "/".into(),
            started: 1,
            ended: Some(2),
            backend: "synthetic strace fixture".into(),
            state: "completed".into(),
            exit_code: Some(0),
            signal: None,
            warnings: vec![],
            parsed_events: 3,
            raw_lines: 3,
            environment: Some(Default::default()),
            completeness: Some("complete_for_supported_events".into()),
            collector_exit_code: Some(0),
            collector_signal: None,
            target_exit_raw: Some("+++ exited with 0 +++".into()),
            evidence_contract: Some("trace-v1".into()),
            target_provenance: Some(TargetProvenance {
                schema_version: 1,
                selection_rule: TARGET_SELECTION_RULE_V1.into(),
                identity_contract: TASK_IDENTITY_CONTRACT_V1.into(),
                root: TaskIdentity {
                    pid: "7".into(),
                    generation: 1,
                },
                root_exec: EventReference {
                    index: 0,
                    evidence: events[0].evidence.clone(),
                },
                target_exit: EventReference {
                    index: 2,
                    evidence: events[2].evidence.clone(),
                },
            }),
        };
        Run {
            raw_integrity: Some(RawIntegrity {
                status: "verified".into(),
                detail: "synthetic fixture assertion; no filesystem verification".into(),
                manifest: Some(RawManifest {
                    schema_version: 1,
                    run_id: metadata.id.clone(),
                    file: "raw/trace.log".into(),
                    receipt: RawReceipt::Parsed {
                        bytes: 4096,
                        logical_lines: 3,
                        sha256: "0".repeat(64),
                    },
                }),
            }),
            metadata,
            events,
            storage_integrity: StorageIntegrity {
                status: "verified".into(),
                detail: "synthetic fixture assertion; no marker verification".into(),
            },
        }
    }

    fn refresh_counts(run: &mut Run) {
        run.metadata.parsed_events = run.events.len();
        run.metadata.raw_lines = run
            .events
            .iter()
            .map(|event| event.evidence.end_line)
            .max()
            .unwrap_or(0);
        if let RawReceipt::Parsed { logical_lines, .. } = &mut run
            .raw_integrity
            .as_mut()
            .unwrap()
            .manifest
            .as_mut()
            .unwrap()
            .receipt
        {
            *logical_lines = run.metadata.raw_lines;
        }
    }

    fn set_exit(run: &mut Run, outcome: &str, code: Option<i32>, signal: Option<i32>) {
        run.events[2].outcome = outcome.into();
        run.metadata.target_exit_raw = Some(outcome.into());
        run.metadata.exit_code = code;
        run.metadata.signal = signal;
        run.metadata.collector_exit_code = code;
        run.metadata.collector_signal = signal;
    }

    fn rejected(run: &Run, kind: StorageLoadFailureKind) {
        assert_eq!(
            validate_comparison_run(run)
                .expect_err("contradictory DTO accepted")
                .kind,
            kind
        );
    }

    #[test]
    fn explicit_trace_fixture_has_consistent_references_and_components() {
        validate_comparison_run(&run_fixture()).unwrap();
    }

    #[test]
    fn provenance_rejects_changed_indices_coordinates_and_unknown_contracts() {
        let mut run = run_fixture();
        run.metadata
            .target_provenance
            .as_mut()
            .unwrap()
            .target_exit
            .index = 1;
        rejected(&run, StorageLoadFailureKind::Corrupt);
        let mut run = run_fixture();
        run.metadata
            .target_provenance
            .as_mut()
            .unwrap()
            .root_exec
            .evidence
            .end_line = 2;
        rejected(&run, StorageLoadFailureKind::Corrupt);
        let mut run = run_fixture();
        run.metadata
            .target_provenance
            .as_mut()
            .unwrap()
            .target_exit
            .index = 100;
        rejected(&run, StorageLoadFailureKind::Corrupt);
        for selection in [true, false] {
            let mut run = run_fixture();
            let provenance = run.metadata.target_provenance.as_mut().unwrap();
            if selection {
                provenance.selection_rule = "future-selection".into();
            } else {
                provenance.identity_contract = "future-identity".into();
            }
            rejected(&run, StorageLoadFailureKind::Unsupported);
        }
    }

    #[test]
    fn reappearing_pid_gets_a_new_lifetime_and_cannot_borrow_the_closed_one() {
        let mut run = run_fixture();
        let mut later_exec = observation("exec", "execve", Some("/later"), "success", 4);
        later_exec.task_generation = Some(2);
        later_exec.context_uncertain = true;
        let mut later_exit = observation("exit", "exit", None, "+++ exited with 0 +++", 5);
        later_exit.task_generation = Some(2);
        later_exit.context_uncertain = true;
        run.events.extend([later_exec, later_exit]);
        run.metadata.completeness = Some("partial".into());
        run.metadata
            .warnings
            .push("PID 7 reappeared without a new spawn".into());
        refresh_counts(&mut run);
        validate_comparison_run(&run).unwrap();
        assert_eq!(
            run.metadata
                .target_provenance
                .as_ref()
                .unwrap()
                .target_exit
                .index,
            2
        );
        let mut borrowed = run.clone();
        borrowed.events[3].task_generation = Some(1);
        rejected(&borrowed, StorageLoadFailureKind::Corrupt);
        let mut borrowed = run.clone();
        borrowed.events[4].task_generation = Some(1);
        rejected(&borrowed, StorageLoadFailureKind::Corrupt);
        let mut borrowed = run;
        let provenance = borrowed.metadata.target_provenance.as_mut().unwrap();
        provenance.target_exit = EventReference {
            index: 4,
            evidence: borrowed.events[4].evidence.clone(),
        };
        rejected(&borrowed, StorageLoadFailureKind::Corrupt);
    }

    #[test]
    fn spawn_entry_introduces_child_before_resume_and_live_collision_allocates() {
        let mut run = run_fixture();
        let mut child_activity = observation("file", "openat", Some("/child"), "success", 3);
        child_activity.pid = "8".into();
        let mut resumed_spawn = observation("spawn", "clone", Some("8"), "success", 2);
        resumed_spawn.evidence.end_line = 4;
        resumed_spawn.child_pid = Some("8".into());
        resumed_spawn.child_generation = Some(1);
        let mut collision = observation("spawn", "fork", Some("8"), "success", 5);
        collision.child_pid = Some("8".into());
        collision.child_generation = Some(2);
        collision.context_uncertain = true;
        let mut child_exit = observation("exit", "exit", None, "+++ exited with 0 +++", 6);
        child_exit.pid = "8".into();
        child_exit.task_generation = Some(2);
        child_exit.context_uncertain = true;
        let root_exit = observation("exit", "exit", None, "+++ exited with 0 +++", 7);
        run.events = vec![
            run.events[0].clone(),
            child_activity,
            resumed_spawn,
            collision,
            child_exit,
            root_exit,
        ];
        run.metadata.target_provenance.as_mut().unwrap().target_exit = EventReference {
            index: 5,
            evidence: run.events[5].evidence.clone(),
        };
        run.metadata.completeness = Some("partial".into());
        run.metadata
            .warnings
            .push("live PID collision leaves the prior child without completion".into());
        refresh_counts(&mut run);
        validate_comparison_run(&run).unwrap();
        let mut bad = run.clone();
        bad.events[1].task_generation = Some(2);
        rejected(&bad, StorageLoadFailureKind::Corrupt);
        let mut bad = run.clone();
        bad.events[3].child_generation = Some(1);
        rejected(&bad, StorageLoadFailureKind::Corrupt);
        let mut bad = run.clone();
        bad.events[3].context_uncertain = false;
        rejected(&bad, StorageLoadFailureKind::Corrupt);
        let mut bad = run;
        bad.events[4].task_generation = Some(1);
        rejected(&bad, StorageLoadFailureKind::Corrupt);
    }

    #[test]
    fn receipt_and_component_statuses_cannot_manufacture_raw_verification() {
        let mut run = run_fixture();
        run.raw_integrity.as_mut().unwrap().status = "unverified".into();
        rejected(&run, StorageLoadFailureKind::Corrupt);
        let mut run = run_fixture();
        run.raw_integrity.as_mut().unwrap().manifest = None;
        rejected(&run, StorageLoadFailureKind::Corrupt);
        let mut run = run_fixture();
        run.events.clear();
        run.metadata.target_provenance = None;
        run.metadata.target_exit_raw = None;
        run.metadata.exit_code = None;
        run.metadata.collector_exit_code = None;
        run.metadata.parsed_events = 0;
        run.metadata.raw_lines = 0;
        run.metadata.state = "capture_failed".into();
        run.metadata.completeness = Some("incomplete".into());
        run.metadata
            .warnings
            .push("normalization read was not attempted".into());
        let raw = run.raw_integrity.as_mut().unwrap();
        raw.status = "unverified".into();
        raw.manifest.as_mut().unwrap().receipt = RawReceipt::Unverified {
            reason: "byte limit suppressed input".into(),
        };
        validate_comparison_run(&run).unwrap();
        let mut bad = run.clone();
        bad.raw_integrity.as_mut().unwrap().status = "verified".into();
        rejected(&bad, StorageLoadFailureKind::Corrupt);
        let mut bad = run;
        bad.metadata.raw_lines = 1;
        rejected(&bad, StorageLoadFailureKind::Corrupt);
    }

    #[test]
    fn unfinalized_withholding_retains_metadata_but_grants_no_component_trust() {
        let mut run = run_fixture();
        run.events.clear();
        run.storage_integrity.status = "unfinalized".into();
        run.raw_integrity.as_mut().unwrap().status = "unverified".into();
        run.raw_integrity.as_mut().unwrap().manifest = None;
        validate_comparison_run(&run).unwrap();
        let mut bad = run.clone();
        bad.raw_integrity.as_mut().unwrap().status = "verified".into();
        rejected(&bad, StorageLoadFailureKind::Corrupt);
        let mut bad = run.clone();
        bad.raw_integrity.as_mut().unwrap().manifest =
            run_fixture().raw_integrity.unwrap().manifest;
        rejected(&bad, StorageLoadFailureKind::Corrupt);
        let mut bad = run;
        bad.events = run_fixture().events;
        rejected(&bad, StorageLoadFailureKind::Corrupt);
    }

    #[test]
    fn parsed_receipt_counts_and_digest_are_independent_contract_facts() {
        for (bytes, logical_lines, sha256) in [
            (0, 3, "0".repeat(64)),
            (2, 3, "0".repeat(64)),
            (4096, 4, "0".repeat(64)),
            (4096, 3, "F".repeat(64)),
        ] {
            let mut run = run_fixture();
            run.raw_integrity
                .as_mut()
                .unwrap()
                .manifest
                .as_mut()
                .unwrap()
                .receipt = RawReceipt::Parsed {
                bytes,
                logical_lines,
                sha256,
            };
            rejected(&run, StorageLoadFailureKind::Corrupt);
        }
    }

    #[test]
    fn numeric_exit_and_signal_must_match_the_selected_saved_spelling() {
        let mut run = run_fixture();
        run.metadata.exit_code = Some(7);
        run.metadata.collector_exit_code = Some(7);
        rejected(&run, StorageLoadFailureKind::Corrupt);
        let mut run = run_fixture();
        set_exit(&mut run, "+++ killed by SIGTERM +++", Some(143), None);
        rejected(&run, StorageLoadFailureKind::Corrupt);
        let mut run = run_fixture();
        set_exit(&mut run, "+++ killed by SIGTERM +++", None, Some(9));
        rejected(&run, StorageLoadFailureKind::Corrupt);
        let mut run = run_fixture();
        set_exit(&mut run, "+++ exited with 255 +++", Some(255), None);
        validate_comparison_run(&run).unwrap();
        set_exit(&mut run, "+++ exited with 256 +++", None, None);
        rejected(&run, StorageLoadFailureKind::Corrupt);
    }

    #[test]
    fn fixed_signal_contract_accepts_standard_and_realtime_boundaries() {
        for (name, number) in [
            ("SIGHUP", 1),
            ("SIGABRT", 6),
            ("SIGIO", 29),
            ("SIGSYS", 31),
            ("SIGRT_0", 32),
            ("SIGRT_32", 64),
            ("SIGRTMIN", 34),
            ("SIGRTMIN+0", 34),
            ("SIGRTMIN+30", 64),
            ("SIGRTMAX", 64),
            ("SIGSEGV (core dumped)", 11),
        ] {
            let mut run = run_fixture();
            set_exit(
                &mut run,
                &format!("+++ killed by {name} +++"),
                None,
                Some(number),
            );
            validate_comparison_run(&run).unwrap();
        }
    }

    #[test]
    fn unknown_signals_keep_absent_numbers_but_malformed_outcomes_are_rejected() {
        for name in [
            "SIG_FUTURE",
            "99",
            "SIGRT_33",
            "SIGRT_-1",
            "SIGRTMIN+31",
            "SIGIOT",
            "SIGPOLL",
        ] {
            let mut run = run_fixture();
            set_exit(&mut run, &format!("+++ killed by {name} +++"), None, None);
            run.metadata.collector_exit_code = Some(0);
            run.metadata.completeness = Some("partial".into());
            run.metadata
                .warnings
                .push("unknown target termination signal".into());
            validate_comparison_run(&run).unwrap();
            run.metadata.signal = Some(15);
            run.metadata.collector_exit_code = None;
            run.metadata.collector_signal = Some(15);
            rejected(&run, StorageLoadFailureKind::Corrupt);
        }
        for outcome in [
            "success",
            "+++ exited with -1 +++",
            "+++ killed by SIGTERM",
            "+++ killed by SIGTERM garbage +++",
            "+++ killed by  +++",
        ] {
            assert!(trace_v1_target_fields(outcome).is_err(), "{outcome}");
        }
    }

    #[test]
    fn historical_v3_outcome_text_remains_opaque() {
        let mut run = run_fixture();
        run.metadata.schema_version = 3;
        run.metadata.evidence_contract = None;
        run.metadata.target_provenance = None;
        run.metadata.target_exit_raw = Some("opaque legacy target spelling".into());
        run.raw_integrity = None;
        for event in &mut run.events {
            event.schema_version = 2;
            event.task_generation = None;
            event.child_generation = None;
        }
        validate_comparison_run(&run).unwrap();
    }

    #[test]
    fn independent_capture_failure_cannot_erase_an_observed_root_exit() {
        for state in ["capture_failed", "interrupted"] {
            let mut run = run_fixture();
            run.metadata.state = state.into();
            run.metadata.completeness = Some("incomplete".into());
            run.metadata
                .warnings
                .push("capture ended with a separate supervision limitation".into());
            validate_comparison_run(&run).unwrap();
            run.metadata.target_provenance = None;
            run.metadata.target_exit_raw = None;
            run.metadata.exit_code = None;
            rejected(&run, StorageLoadFailureKind::Corrupt);
        }

        // A genuine missing root exit still permits a finalized incomplete run.
        // Its collector outcome does not create target completion references.
        let mut missing = run_fixture();
        missing.events.pop();
        missing.metadata.target_provenance = None;
        missing.metadata.target_exit_raw = None;
        missing.metadata.exit_code = None;
        missing.metadata.state = "capture_failed".into();
        missing.metadata.completeness = Some("incomplete".into());
        missing
            .metadata
            .warnings
            .push("root completion was not observed".into());
        refresh_counts(&mut missing);
        validate_comparison_run(&missing).unwrap();
        let mut fabricated = missing;
        fabricated.metadata.target_provenance = run_fixture().metadata.target_provenance;
        fabricated.metadata.target_exit_raw = Some("+++ exited with 0 +++".into());
        fabricated.metadata.exit_code = Some(0);
        rejected(&fabricated, StorageLoadFailureKind::Corrupt);
    }

    #[test]
    fn unintroduced_actor_must_keep_uncertainty_on_each_observation() {
        let mut run = run_fixture();
        let mut newcomer = observation("file", "openat", Some("/unintroduced"), "success", 2);
        newcomer.pid = "8".into();
        newcomer.context_uncertain = true;
        let mut newcomer_exit = observation("exit", "exit", None, "+++ exited with 0 +++", 3);
        newcomer_exit.pid = "8".into();
        newcomer_exit.context_uncertain = true;
        let root_exit = observation("exit", "exit", None, "+++ exited with 0 +++", 4);
        run.events = vec![run.events[0].clone(), newcomer, newcomer_exit, root_exit];
        run.metadata.target_provenance.as_mut().unwrap().target_exit = EventReference {
            index: 3,
            evidence: run.events[3].evidence.clone(),
        };
        run.metadata.completeness = Some("partial".into());
        run.metadata
            .warnings
            .push("task introduction evidence is missing".into());
        refresh_counts(&mut run);
        validate_comparison_run(&run).unwrap();
        for index in [1, 2] {
            let mut erased = run.clone();
            erased.events[index].context_uncertain = false;
            rejected(&erased, StorageLoadFailureKind::Corrupt);
        }
    }

    #[test]
    fn colliding_child_without_actor_rows_still_requires_identity_caution() {
        let mut run = run_fixture();
        let mut first_spawn = observation("spawn", "fork", Some("8"), "success", 2);
        first_spawn.child_pid = Some("8".into());
        first_spawn.child_generation = Some(1);
        let mut collision = observation("spawn", "fork", Some("8"), "success", 3);
        collision.child_pid = Some("8".into());
        collision.child_generation = Some(2);
        collision.context_uncertain = true;
        let root_exit = observation("exit", "exit", None, "+++ exited with 0 +++", 4);
        run.events = vec![run.events[0].clone(), first_spawn, collision, root_exit];
        run.metadata.target_provenance.as_mut().unwrap().target_exit = EventReference {
            index: 3,
            evidence: run.events[3].evidence.clone(),
        };
        run.metadata.completeness = Some("partial".into());
        run.metadata
            .warnings
            .push("child lifetime introduction collided".into());
        refresh_counts(&mut run);
        validate_comparison_run(&run).unwrap();

        let mut erased = run.clone();
        erased.metadata.completeness = Some("complete_for_supported_events".into());
        erased.metadata.warnings.clear();
        erased.events[2].context_uncertain = false;
        rejected(&erased, StorageLoadFailureKind::Corrupt);

        // Interrupted/incomplete normally permits an empty warning list, but an
        // observed identity collision remains a separate fact that needs one.
        let mut erased = run;
        erased.metadata.state = "interrupted".into();
        erased.metadata.completeness = Some("incomplete".into());
        erased.metadata.warnings.clear();
        rejected(&erased, StorageLoadFailureKind::Corrupt);
    }

    #[test]
    fn complete_trace_requires_every_introduced_lifetime_to_exit() {
        let mut run = run_fixture();
        let mut spawn = observation("spawn", "fork", Some("8"), "success", 2);
        spawn.child_pid = Some("8".into());
        spawn.child_generation = Some(1);
        let mut child_exit = observation("exit", "exit", None, "+++ exited with 0 +++", 3);
        child_exit.pid = "8".into();
        let root_exit = observation("exit", "exit", None, "+++ exited with 0 +++", 4);
        run.events = vec![run.events[0].clone(), spawn, child_exit, root_exit];
        run.metadata.target_provenance.as_mut().unwrap().target_exit = EventReference {
            index: 3,
            evidence: run.events[3].evidence.clone(),
        };
        refresh_counts(&mut run);
        validate_comparison_run(&run).unwrap();

        let mut missing = run.clone();
        missing.events.remove(2);
        missing
            .metadata
            .target_provenance
            .as_mut()
            .unwrap()
            .target_exit
            .index = 2;
        refresh_counts(&mut missing);
        rejected(&missing, StorageLoadFailureKind::Corrupt);

        missing.metadata.completeness = Some("partial".into());
        missing
            .metadata
            .warnings
            .push("child completion was not observed".into());
        validate_comparison_run(&missing).unwrap();
        missing.metadata.state = "interrupted".into();
        missing.metadata.completeness = Some("incomplete".into());
        validate_comparison_run(&missing).unwrap();
        missing.metadata.warnings.clear();
        rejected(&missing, StorageLoadFailureKind::Corrupt);

        let mut invalid_exit = run;
        invalid_exit.events[2].outcome = "success".into();
        rejected(&invalid_exit, StorageLoadFailureKind::Corrupt);
    }

    #[test]
    fn current_exit_framing_preserves_unknown_signals_and_legacy_text() {
        let mut event = observation("exit", "exit", None, "+++ killed by SIG_FUTURE +++", 1);
        validate_current_event(&event).unwrap();
        event.outcome = "success".into();
        assert!(validate_current_event(&event).is_err());
        event.schema_version = 2;
        event.task_generation = None;
        validate_current_event(&event).unwrap();
    }

    #[test]
    fn withheld_events_do_not_bypass_static_target_provenance_validation() {
        let mut run = run_fixture();
        let mut finalized_resumed_order = run.clone();
        finalized_resumed_order.events.reverse();
        let provenance = finalized_resumed_order
            .metadata
            .target_provenance
            .as_mut()
            .unwrap();
        provenance.root_exec.index = 2;
        provenance.target_exit.index = 0;
        validate_comparison_run(&finalized_resumed_order).unwrap();
        run.events.clear();
        run.storage_integrity.status = "unfinalized".into();
        let raw = run.raw_integrity.as_mut().unwrap();
        raw.status = "unverified".into();
        raw.manifest = None;
        validate_comparison_run(&run).unwrap();
        let mut resumed_order = run.clone();
        let provenance = resumed_order.metadata.target_provenance.as_mut().unwrap();
        provenance.root_exec.index = 2;
        provenance.target_exit.index = 0;
        validate_comparison_run(&resumed_order).unwrap();
        let mut same_role = run.clone();
        let provenance = same_role.metadata.target_provenance.as_mut().unwrap();
        provenance.target_exit = provenance.root_exec.clone();
        rejected(&same_role, StorageLoadFailureKind::Corrupt);
        let mut reversed = run.clone();
        let provenance = reversed.metadata.target_provenance.as_mut().unwrap();
        std::mem::swap(
            &mut provenance.root_exec.evidence,
            &mut provenance.target_exit.evidence,
        );
        rejected(&reversed, StorageLoadFailureKind::Corrupt);

        let mut unsupported = run.clone();
        unsupported
            .metadata
            .target_provenance
            .as_mut()
            .unwrap()
            .schema_version = 999;
        rejected(&unsupported, StorageLoadFailureKind::Unsupported);
        for (pid, generation) in [("0", 1), ("not-a-pid", 1), ("4294967296", 1), ("7", 0)] {
            let mut invalid = run.clone();
            invalid.metadata.target_provenance.as_mut().unwrap().root = TaskIdentity {
                pid: pid.into(),
                generation,
            };
            rejected(&invalid, StorageLoadFailureKind::Corrupt);
        }
        for (line, end_line, file, index) in [
            (0, 1, "raw/trace.log", 2),
            (2, 1, "raw/trace.log", 2),
            (1, 4, "raw/trace.log", 2),
            (1, 1, "../outside", 2),
            (1, 1, "raw/trace.log", 3),
        ] {
            let mut invalid = run.clone();
            invalid
                .metadata
                .target_provenance
                .as_mut()
                .unwrap()
                .target_exit = EventReference {
                index,
                evidence: Evidence {
                    file: file.into(),
                    line,
                    end_line,
                },
            };
            rejected(&invalid, StorageLoadFailureKind::Corrupt);
        }
    }
}
