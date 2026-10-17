//! Deterministic, I/O-free comparison of already-loaded runs.
//!
//! The result preserves the four-way contract: `0` is trusted equality, `1` is
//! a trusted observed difference, `2` is a load/tool error with no ordinary
//! comparison conclusion, and `3` is incomplete or unverified evidence. A
//! change is an investigation clue, never proof of causation or file identity.
//!
//! Loading and versioned storage validation happen before this module. Comparison
//! consumes those facts without opening raw files or re-parsing old evidence;
//! doing so here would change historical conclusions with the reader's version.
//! Deterministic ordered maps/sets define production order independently of task
//! scheduling and hash iteration. Presentation may reorder/fold the wire view,
//! but cannot decide whether evidence is trusted or recompute its exit status.
//! Explicit v6 maps changed aggregates to every saved normalized occurrence; it
//! does not claim coverage of discarded/unsupported raw syscalls or unchanged
//! aggregates, and its occurrence counts never become cross-run identity.

use crate::model::{
    EventReference, Evidence, LoadOutcome, Metadata, Run, StorageLoadFailure,
    StorageLoadFailureKind, TargetProvenance, validate_comparison_run,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

// The old flat sample cap remains the same in every report version. V6 exposes
// this cap as a computed fact; changing it requires a deliberate wire-contract
// update rather than an independent renderer limit.
const EVIDENCE_SAMPLE_LIMIT: usize = 3;

/// Presentation-neutral tier used to order complete semantic changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RankClass {
    /// Target outcome changed.
    ExitStatus,
    /// Capture state, completeness, or warning count changed.
    CaptureCompleteness,
    /// Invocation context such as command, CWD, or selected environment changed.
    InvocationContext,
    /// A uniquely successful observation became only failures.
    SuccessToFailure,
    /// A newly observed execution attempt has no successful outcome.
    FailedExecutionAttempt,
    /// A successful executable observation appeared.
    NewExecutable,
    /// A file access appeared.
    NewFileAccess,
    /// A successful executable observation disappeared.
    RemovedExecutable,
    /// An existing file or execution attempt changed its outcome set.
    OutcomeSetChange,
    /// Another complete semantic observation changed.
    OtherObservation,
}

/// Stable semantic rank plus the deterministic comparison-production order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RankKey {
    class: RankClass,
    comparison_order: usize,
}

impl RankKey {
    fn pending() -> Self {
        Self {
            class: RankClass::OtherObservation,
            comparison_order: 0,
        }
    }

    /// Semantic ranking tier, independent of text folding policy.
    pub(crate) fn class(self) -> RankClass {
        self.class
    }

    /// Original deterministic production index used to break rank ties.
    pub(crate) fn comparison_order(self) -> usize {
        self.comparison_order
    }
}

/// Semantic change identity, independent of the frozen English category spelling.
///
/// The v4 renderer retains `Change::category`; ranking consumes this enum so a
/// wording edit cannot silently change investigation priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChangeKind {
    ExitStatus,
    CaptureCompleteness,
    InitialWorkingDirectory,
    CommandArguments,
    SelectedStartupEnvironment,
    ExecutionAttempt,
    Executable,
    NewFileAccess,
    RemovedFileAccess,
    FileAccessOutcome,
}

impl ChangeKind {
    fn category(self) -> &'static str {
        match self {
            Self::ExitStatus => "Exit status",
            Self::CaptureCompleteness => "Capture completeness",
            Self::InitialWorkingDirectory => "Initial working directory",
            Self::CommandArguments => "Command arguments",
            Self::SelectedStartupEnvironment => "Selected startup environment",
            Self::ExecutionAttempt => "Execution attempt changes",
            Self::Executable => "Executable changes",
            Self::NewFileAccess => "New file access",
            Self::RemovedFileAccess => "Removed file access",
            Self::FileAccessOutcome => "File access outcome changes",
        }
    }
}

/// One aggregated change, bounded samples, and optional complete v6 references.
///
/// Aggregation identity is kind, operation, and path. PID, scheduling order,
/// duplicate count, and timing are deliberately absent from cross-run identity.
#[derive(Debug)]
pub(crate) struct Change {
    kind: ChangeKind,
    /// Stable user-facing change category.
    pub(crate) category: String,
    /// Operation/path subject of the observation.
    pub(crate) subject: String,
    /// Sorted aggregate outcome set observed on the left.
    pub(crate) left: Vec<String>,
    /// Sorted aggregate outcome set observed on the right.
    pub(crate) right: Vec<String>,
    /// Bounded raw-evidence samples from the left run.
    pub(crate) left_evidence: Vec<Evidence>,
    /// Bounded raw-evidence samples from the right run.
    pub(crate) right_evidence: Vec<Evidence>,
    /// Stable semantic ordering data; omitted from the diff v4 compatibility view.
    pub(crate) rank_key: RankKey,
    /// Structured path fact used by presentation without parsing display text.
    pub(crate) file_path: Option<String>,
    /// Complete saved-event mapping in explicit v6; absent from v4/v5.
    pub(crate) evidence_mapping: Option<ChangeEvidenceMapping>,
}

/// Complete comparison result before the selected text or JSON presentation.
pub(crate) struct Report {
    /// Diff report schema version exposed by the compatibility renderer.
    pub(crate) schema_version: u32,
    /// `None` when load/tool failure prevents an ordinary comparison conclusion.
    pub(crate) observed_differences: Option<bool>,
    /// Independent capture and storage reliability for both inputs.
    pub(crate) reliability: Reliability,
    /// Four-way business status: trusted 0/1, tool error 2, or incomplete 3.
    pub(crate) exit_code: i32,
    /// Load/tool failures retained separately from ordinary changes.
    pub(crate) errors: Vec<String>,
    /// Left record label used for display.
    pub(crate) left: String,
    /// Right record label used for display.
    pub(crate) right: String,
    /// Left record ID used to qualify evidence paths.
    pub(crate) left_id: String,
    /// Right record ID used to qualify evidence paths.
    pub(crate) right_id: String,
    /// Ranked observed changes; empty does not imply trusted equality by itself.
    pub(crate) changes: Vec<Change>,
    /// Reliability cautions that do not replace errors or change facts.
    pub(crate) warnings: Vec<String>,
    /// Explicit v5 saved target references; absent from the frozen v4 view.
    pub(crate) target_provenance: Option<TargetProvenanceSides>,
    /// Frozen meaning of explicit v6 references, absent from earlier formats.
    pub(crate) evidence_contract: Option<&'static str>,
}

/// Requested comparison view; automatic selection remains v4 or trace-v1 v5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReportFormat {
    /// Preserve the format selected by the loaded metadata versions.
    Automatic,
    /// Explicitly require trace-v1 component facts and expose schema v5.
    TraceV5,
    /// Add complete saved-event mappings to the v5 contract as schema v6.
    OutcomeV6,
}

/// Typed aggregate spelling, without asserting physical identity or causation.
#[derive(Debug, Serialize)]
pub(crate) struct ObservationKey {
    /// Normalized observation family, currently file or exec.
    pub(crate) kind: String,
    /// Saved syscall operation spelling.
    pub(crate) operation: String,
    /// Saved observed path spelling, never reconstructed from display text.
    pub(crate) path: String,
}

/// Authority for a side's references, kept separate from integrity and trust.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EvidenceSource {
    /// All saved normalized events belonging to this changed aggregate.
    NormalizedEvents,
    /// A finalized trace-v1 target exit selected by saved provenance.
    TargetProvenance,
    /// Compared metadata facts with no invented syscall attribution.
    Metadata,
    /// Saved observations withheld without verified finalization.
    Withheld,
}

/// One outcome and every supporting occurrence in the saved event vector.
#[derive(Debug, Serialize)]
pub(crate) struct OutcomeEvidenceMapping {
    /// Compared outcome spelling; target outcomes use the metadata comparison value.
    pub(crate) outcome: String,
    /// Complete occurrence count, exactly equal to events.len().
    pub(crate) event_count: usize,
    /// Untruncated references in saved vector order, including duplicate coordinates.
    pub(crate) events: Vec<EventReference>,
}

/// Complete side mapping plus the separate bounded compatibility sample accounting.
#[derive(Debug, Serialize)]
pub(crate) struct SideEvidenceMapping {
    /// Which saved facts authorize references or explain their absence.
    pub(crate) source: EvidenceSource,
    /// Metadata field paths compared for this change, when applicable.
    pub(crate) metadata_fields: Vec<String>,
    /// Complete mapped occurrence count; None is inapplicable for metadata or unknown
    /// when observations were withheld, rather than an assertion of zero events.
    pub(crate) event_count: Option<usize>,
    /// Saved vector indices in exactly the same order as the flat evidence samples.
    pub(crate) sample_event_indices: Vec<usize>,
    /// Number of bounded samples, independent of complete mapped occurrence count.
    pub(crate) sample_count: usize,
    /// Complete occurrences absent from the samples, never from the full mapping.
    pub(crate) omitted_sample_count: Option<usize>,
    /// Sorted outcomes with every saved supporting event reference.
    pub(crate) outcomes: Vec<OutcomeEvidenceMapping>,
}

/// Explicit v6 mapping for one change; it does not cover unchanged aggregate keys.
#[derive(Debug, Serialize)]
pub(crate) struct ChangeEvidenceMapping {
    /// Typed file/exec identity spelling; metadata-only changes have no observation key.
    pub(crate) observation: Option<ObservationKey>,
    /// Existing bounded flat-sample limit, applied independently to each side.
    pub(crate) sample_limit_per_side: usize,
    /// Left saved observations or explicit reason event attribution is unavailable.
    pub(crate) left: SideEvidenceMapping,
    /// Right saved observations or explicit reason event attribution is unavailable.
    pub(crate) right: SideEvidenceMapping,
}

/// Each side retains its own capture-time references, never cross-run PID identity.
#[derive(Debug, Serialize)]
pub(crate) struct TargetProvenanceSides {
    /// Saved left target selection, if observed.
    pub(crate) left: Option<TargetProvenance>,
    /// Saved right target selection, if observed.
    pub(crate) right: Option<TargetProvenance>,
}
/// Retain complete saved occurrences without a separate tree for one outcome.
///
/// Outcome strings stay borrowed from the immutable Run. A second distinct
/// spelling promotes the bucket to the established lexical tree and moves the
/// original index vector, preserving its entries and capacity. This avoids a
/// per-key inner node for singleton observations, at the cost of a larger value
/// in the outer tree. Multiple outcomes keep tree insertion complexity even for
/// unbounded, adversarial insertion order; a sorted vector would shift entries.
///
/// Empty also supplies the read-only missing-side view. Insertion may briefly
/// create an empty index vector before aggregate pushes its saved event index;
/// every completed aggregate bucket and vector is nonempty. No removal API can
/// leave a Multiple bucket empty or discard its existing occurrences.
#[derive(Default)]
enum OutcomeEvidence<'events> {
    #[default]
    Empty,
    Single(&'events str, Vec<usize>),
    Multiple(BTreeMap<&'events str, Vec<usize>>),
}

impl<'events> OutcomeEvidence<'events> {
    fn new() -> Self {
        Self::Empty
    }

    fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }

    fn contains_key(&self, outcome: &str) -> bool {
        match self {
            Self::Empty => false,
            Self::Single(key, _) => *key == outcome,
            Self::Multiple(outcomes) => outcomes.contains_key(outcome),
        }
    }

    fn indices_mut(&mut self, outcome: &'events str) -> &mut Vec<usize> {
        if matches!(self, Self::Single(key, _) if *key != outcome) {
            // The guard proves Single. Taking the value transfers ownership;
            // neither duplicate observations nor index capacity are copied.
            let Self::Single(previous, indices) = std::mem::take(self) else {
                unreachable!("promotion requires a distinct singleton outcome");
            };
            let mut outcomes = BTreeMap::new();
            outcomes.insert(previous, indices);
            *self = Self::Multiple(outcomes);
        }
        match self {
            Self::Empty => {
                *self = Self::Single(outcome, Vec::new());
                let Self::Single(_, indices) = self else {
                    unreachable!("first insertion establishes Single");
                };
                indices
            }
            Self::Single(_, indices) => indices,
            Self::Multiple(outcomes) => outcomes.entry(outcome).or_default(),
        }
    }

    fn iter(&self) -> impl Iterator<Item = (&&'events str, &Vec<usize>)> {
        let (single, multiple) = match self {
            Self::Empty => (None, std::collections::btree_map::Iter::default()),
            Self::Single(key, indices) => (
                Some((key, indices)),
                std::collections::btree_map::Iter::default(),
            ),
            Self::Multiple(outcomes) => (None, outcomes.iter()),
        };
        // Standard iterator composition preserves collect's reservation path;
        // exact size hints on a custom iterator alone would grow singleton
        // display/mapping vectors. The empty tree iterator borrows no local
        // map and allocates nothing. Only one branch supplies any occurrences.
        single.into_iter().chain(multiple)
    }

    fn keys(&self) -> impl Iterator<Item = &&'events str> {
        self.iter().map(|(key, _)| key)
    }

    fn values(&self) -> impl Iterator<Item = &Vec<usize>> {
        self.iter().map(|(_, indices)| indices)
    }
}

type Aggregate<'a> = BTreeMap<(&'a str, &'a str, &'a str), OutcomeEvidence<'a>>;

/// Select bounded examples, giving side-only outcomes evidence before shared ones.
///
/// Repeated common probes must not consume the entire sample budget and conceal
/// the new failure/success that caused an outcome-set change. The complete outcome
/// sets remain in `Change`; these coordinates are examples, not an exhaustive
/// list of every occurrence or a statement about frequency.
fn sample_event_indices(
    run: &Run,
    outcomes: &OutcomeEvidence<'_>,
    other: &OutcomeEvidence<'_>,
) -> Vec<usize> {
    let mut samples = Vec::new();
    for evidence in outcomes
        .iter()
        .filter(|(outcome, _)| !other.contains_key(outcome))
        .filter_map(|(_, evidence)| evidence.first())
    {
        if samples.len() == EVIDENCE_SAMPLE_LIMIT {
            return samples;
        }
        samples.push(*evidence);
    }
    for evidence in outcomes
        .iter()
        .filter(|(outcome, _)| other.contains_key(outcome))
        .flat_map(|(_, evidence)| evidence)
    {
        if samples.len() == EVIDENCE_SAMPLE_LIMIT {
            return samples;
        }
        samples.push(*evidence);
    }
    for evidence in outcomes.values().flatten() {
        if samples.len() == EVIDENCE_SAMPLE_LIMIT {
            break;
        }
        // The established fallback deduplicates coordinates, whereas the first
        // two passes preserve each selected occurrence. Keep that sample policy
        // unchanged; the v6 full mapping never deduplicates saved vector entries.
        if !samples
            .iter()
            .any(|&index| run.events[index].evidence == run.events[*evidence].evidence)
        {
            samples.push(*evidence);
        }
    }
    samples
}

/// Group supported file/exec observations by spelling and retain each outcome set.
///
/// PID, repetition count and order describe one run's execution, but are not the
/// current cross-run identity. Matching path strings also do not establish inode
/// or content identity. Other event families support capture/context reasoning
/// and do not become independent file/exec differences here.
///
/// Current inputs are shape-validated before aggregation. Legacy pathless events
/// remain omitted under their historical contract and can never supply trusted
/// equality; comparison does not reinterpret them with current parser behavior.
/// Explicit v6 withholds unfinalized observations at this single admission point,
/// including valid nonempty historical DTOs. Outcome sets, changes, flat samples
/// and full mappings all consume these buckets. Validation must precede this
/// filter so withholding never conceals corrupt input; the original DTO and its
/// metadata remain intact. Ordinary unverified evidence and v4/v5 keep their
/// existing admission behavior.
fn aggregate(r: &Run, format: ReportFormat) -> Aggregate<'_> {
    let mut a = Aggregate::new();
    if format == ReportFormat::OutcomeV6 && r.storage_integrity.status == "unfinalized" {
        return a;
    }
    for (index, e) in r.events.iter().enumerate() {
        if e.kind != "file" && e.kind != "exec" {
            continue;
        }
        if let Some(path) = &e.path {
            a.entry((&e.kind, &e.operation, path))
                .or_default()
                .indices_mut(&e.outcome)
                .push(index);
        }
    }
    a
}

/// Consume the lexical key union without allocating another evidence index.
///
/// Run strings remain borrowed for the whole comparison. Buckets move with all
/// saved indices and capacity intact, and drop after their full mappings have
/// been materialized. Both maps use the same tuple order; advancing the smaller
/// key or both equal keys preserves production order and exhausted-side tails.
fn ordered_aggregates<'events>(
    left: Aggregate<'events>,
    right: Aggregate<'events>,
) -> impl Iterator<
    Item = (
        (&'events str, &'events str, &'events str),
        Option<OutcomeEvidence<'events>>,
        Option<OutcomeEvidence<'events>>,
    ),
> {
    let mut left = left.into_iter().peekable();
    let mut right = right.into_iter().peekable();
    std::iter::from_fn(move || match (left.peek(), right.peek()) {
        (Some((left_key, _)), Some((right_key, _))) => match left_key.cmp(right_key) {
            std::cmp::Ordering::Less => left.next().map(|(key, value)| (key, Some(value), None)),
            std::cmp::Ordering::Greater => {
                right.next().map(|(key, value)| (key, None, Some(value)))
            }
            std::cmp::Ordering::Equal => left
                .next()
                .zip(right.next())
                .map(|((key, left), (_, right))| (key, Some(left), Some(right))),
        },
        (Some(_), None) => left.next().map(|(key, value)| (key, Some(value), None)),
        (None, Some(_)) => right.next().map(|(key, value)| (key, None, Some(value))),
        (None, None) => None,
    })
}

/// Copy coordinates only after selecting exact saved vector entries.
fn sampled_evidence(run: &Run, indices: &[usize]) -> Vec<Evidence> {
    indices
        .iter()
        .map(|&index| run.events[index].evidence.clone())
        .collect()
}

fn metadata_mapping(run: &Run, fields: &[&str]) -> SideEvidenceMapping {
    SideEvidenceMapping {
        source: if run.storage_integrity.status == "unfinalized" {
            EvidenceSource::Withheld
        } else {
            EvidenceSource::Metadata
        },
        metadata_fields: fields.iter().map(|field| (*field).into()).collect(),
        event_count: None,
        sample_event_indices: vec![],
        sample_count: 0,
        omitted_sample_count: None,
        outcomes: vec![],
    }
}

fn target_mapping(run: &Run) -> SideEvidenceMapping {
    const FIELDS: &[&str] = &[
        "metadata.exit_code",
        "metadata.signal",
        "metadata.target_exit_raw",
    ];
    // Initial metadata can retain saved provenance after the loader withholds
    // events. It does not authorize a finalized reference or a guessed count.
    if run.storage_integrity.status == "verified"
        && run.metadata.schema_version == 4
        && let Some(provenance) = &run.metadata.target_provenance
    {
        SideEvidenceMapping {
            source: EvidenceSource::TargetProvenance,
            metadata_fields: FIELDS.iter().map(|field| (*field).into()).collect(),
            event_count: Some(1),
            sample_event_indices: vec![provenance.target_exit.index],
            sample_count: 1,
            omitted_sample_count: Some(0),
            outcomes: vec![OutcomeEvidenceMapping {
                outcome: target_exit(&run.metadata),
                event_count: 1,
                events: vec![provenance.target_exit.clone()],
            }],
        }
    } else {
        metadata_mapping(run, FIELDS)
    }
}

fn metadata_change_mapping(kind: ChangeKind, l: &Run, r: &Run) -> ChangeEvidenceMapping {
    let fields: &[&str] = match kind {
        ChangeKind::InitialWorkingDirectory => &["metadata.cwd"],
        ChangeKind::CommandArguments => &["metadata.command"],
        ChangeKind::CaptureCompleteness => &[
            "metadata.state",
            "metadata.completeness",
            "metadata.warnings",
        ],
        ChangeKind::SelectedStartupEnvironment => &["metadata.environment"],
        ChangeKind::ExitStatus => &[],
        // Only the explicit metadata-change construction sites call this helper.
        _ => unreachable!("event aggregate cannot use metadata attribution"),
    };
    let side = |run| {
        if kind == ChangeKind::ExitStatus {
            target_mapping(run)
        } else {
            metadata_mapping(run, fields)
        }
    };
    ChangeEvidenceMapping {
        observation: None,
        sample_limit_per_side: EVIDENCE_SAMPLE_LIMIT,
        left: side(l),
        right: side(r),
    }
}

/// Materialize full references only for an explicitly requested v6 changed key.
fn normalized_mapping(
    run: &Run,
    outcomes: &OutcomeEvidence<'_>,
    sample_event_indices: Vec<usize>,
) -> SideEvidenceMapping {
    if run.storage_integrity.status == "unfinalized" {
        return metadata_mapping(run, &[]);
    }
    // Each event belongs to exactly one kind/operation/path/outcome bucket, so
    // this sum is bounded by the saved vector length. Samples select occurrences
    // from those same buckets; omitted counts refer only to the bounded samples.
    let event_count: usize = outcomes.values().map(Vec::len).sum();
    let sample_count = sample_event_indices.len();
    SideEvidenceMapping {
        source: EvidenceSource::NormalizedEvents,
        metadata_fields: vec![],
        event_count: Some(event_count),
        sample_event_indices,
        sample_count,
        omitted_sample_count: Some(event_count - sample_count),
        outcomes: outcomes
            .iter()
            .map(|(outcome, indices)| OutcomeEvidenceMapping {
                outcome: (*outcome).into(),
                event_count: indices.len(),
                events: indices
                    .iter()
                    .map(|&index| EventReference {
                        index,
                        evidence: run.events[index].evidence.clone(),
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// Compare explicit code/signal facts, falling back to raw text only if both lack.
///
/// Unknown signal spellings still carry an observed difference. Once a structured
/// outcome exists, raw spelling is redundant presentation and must not create a
/// difference between equivalent explicit outcomes. Numeric shell-style statuses
/// are not decoded here into signal facts.
fn target_exit(metadata: &Metadata) -> String {
    let structured = format!(
        "code={:?}, signal={:?}",
        metadata.exit_code, metadata.signal
    );
    if metadata.exit_code.is_none() && metadata.signal.is_none() {
        match &metadata.target_exit_raw {
            Some(raw) => format!("{structured}, raw={raw:?}"),
            None => structured,
        }
    } else {
        structured
    }
}

/// Apply the existing ranking policy to a fully constructed change.
///
/// Priority reflects useful investigation clues, not causal certainty. A side
/// containing both failure and success is not the single-success-to-failure tier.
/// Semantic kind drives the policy; the English category is a compatibility
/// spelling only. Outcome sets still select the existing success/failure tiers.
fn rank_class(c: &Change) -> RankClass {
    match c.kind {
        ChangeKind::ExitStatus => RankClass::ExitStatus,
        ChangeKind::CaptureCompleteness => RankClass::CaptureCompleteness,
        ChangeKind::InitialWorkingDirectory
        | ChangeKind::CommandArguments
        | ChangeKind::SelectedStartupEnvironment => RankClass::InvocationContext,
        _ if c.left == ["success"]
            && !c.right.is_empty()
            && !c.right.iter().any(|s| s == "success") =>
        {
            RankClass::SuccessToFailure
        }
        ChangeKind::ExecutionAttempt
            if !c.right.is_empty() && !c.right.iter().any(|s| s == "success") =>
        {
            RankClass::FailedExecutionAttempt
        }
        ChangeKind::Executable if !c.right.is_empty() => RankClass::NewExecutable,
        ChangeKind::NewFileAccess => RankClass::NewFileAccess,
        ChangeKind::Executable => RankClass::RemovedExecutable,
        ChangeKind::FileAccessOutcome | ChangeKind::ExecutionAttempt => RankClass::OutcomeSetChange,
        _ => RankClass::OtherObservation,
    }
}
/// Compare two loaded runs without performing storage or presentation I/O.
///
/// Inputs carry loader integrity findings and are rechecked against pure model
/// fact contracts, including current event shape. Invalid DTOs produce tool error
/// 2 without an ordinary comparison conclusion. Differences and reliability
/// are computed separately: even an empty change set returns 3 if either input is
/// untrusted. Default v4 metadata changes retain empty per-event coordinates;
/// trace-v1 v5 target-exit changes use the saved target provenance reference.
pub(crate) fn compare(l: &Run, r: &Run) -> Report {
    compare_version(l, r, ReportFormat::Automatic)
}

/// Assemble metadata observations in the existing production order.
///
/// The caller validates the Runs and chooses the report contract first. This
/// helper only appends changes; ranking, reliability and status stay in compare.
fn append_metadata_changes(changes: &mut Vec<Change>, l: &Run, r: &Run, outcome_report: bool) {
    let mut simple = |kind: ChangeKind, left: String, right: String| {
        if left != right {
            changes.push(Change {
                kind,
                category: kind.category().into(),
                subject: String::new(),
                left: vec![left],
                right: vec![right],
                left_evidence: vec![],
                right_evidence: vec![],
                rank_key: RankKey::pending(),
                file_path: None,
                evidence_mapping: outcome_report.then(|| metadata_change_mapping(kind, l, r)),
            });
        }
    };
    simple(
        ChangeKind::ExitStatus,
        target_exit(&l.metadata),
        target_exit(&r.metadata),
    );
    simple(
        ChangeKind::InitialWorkingDirectory,
        l.metadata.cwd.clone(),
        r.metadata.cwd.clone(),
    );
    simple(
        ChangeKind::CommandArguments,
        format!("{:?}", l.metadata.command),
        format!("{:?}", r.metadata.command),
    );
    // Warning text remains stored detail. This comparison contract distinguishes
    // state, completeness and warning count, not arbitrary warning wording.
    simple(
        ChangeKind::CaptureCompleteness,
        format!(
            "{}; {}; {} warnings",
            l.metadata.state,
            l.metadata
                .completeness
                .as_deref()
                .unwrap_or("legacy/unknown"),
            l.metadata.warnings.len()
        ),
        format!(
            "{}; {}; {} warnings",
            r.metadata.state,
            r.metadata
                .completeness
                .as_deref()
                .unwrap_or("legacy/unknown"),
            r.metadata.warnings.len()
        ),
    );
    // 三种环境事实不能合并：未选择、选择但未设置、选择且有值。legacy 缺整张
    // map 仅用于枚举键时视为空；下方仍独立保留“不知道选择内容”的警告。
    let empty_environment = BTreeMap::new();
    let left_env = l
        .metadata
        .environment
        .as_ref()
        .unwrap_or(&empty_environment);
    let right_env = r
        .metadata
        .environment
        .as_ref()
        .unwrap_or(&empty_environment);
    for key in left_env
        .keys()
        .chain(right_env.keys())
        .collect::<BTreeSet<_>>()
    {
        let left = left_env.get(key);
        let right = right_env.get(key);
        if left != right {
            let display = |value: Option<&Option<String>>| match value {
                None => "(not selected/unknown)".into(),
                Some(None) => "(unset)".into(),
                Some(Some(s)) => format!("value={s:?}"),
            };
            changes.push(Change {
                kind: ChangeKind::SelectedStartupEnvironment,
                category: ChangeKind::SelectedStartupEnvironment.category().into(),
                subject: (*key).clone(),
                left: vec![display(left)],
                right: vec![display(right)],
                left_evidence: vec![],
                right_evidence: vec![],
                rank_key: RankKey::pending(),
                file_path: None,
                evidence_mapping: outcome_report
                    .then(|| metadata_change_mapping(ChangeKind::SelectedStartupEnvironment, l, r)),
            });
        }
    }
}

/// Materialize one changed observation from admitted, immutable aggregate buckets.
///
/// This helper owns semantic change facts and references, not evidence admission or
/// reliability. Outcome spelling sets determine equality; repetition count never
/// changes identity. Both samples and complete v6 references are copied before
/// the caller drops the consumed buckets. Ranking remains with report assembly,
/// preserving metadata-first production order and the established tie-break.
fn observation_change(
    l: &Run,
    r: &Run,
    key: (&str, &str, &str),
    left: &OutcomeEvidence<'_>,
    right: &OutcomeEvidence<'_>,
    outcome_report: bool,
) -> Option<Change> {
    if left.keys().eq(right.keys()) {
        return None;
    }
    let kind = if key.0 == "exec" {
        if left.keys().chain(right.keys()).any(|s| *s != "success") {
            ChangeKind::ExecutionAttempt
        } else {
            ChangeKind::Executable
        }
    } else if left.is_empty() {
        ChangeKind::NewFileAccess
    } else if right.is_empty() {
        ChangeKind::RemovedFileAccess
    } else {
        ChangeKind::FileAccessOutcome
    };
    let left_samples = sample_event_indices(l, left, right);
    let right_samples = sample_event_indices(r, right, left);
    let left_evidence = sampled_evidence(l, &left_samples);
    let right_evidence = sampled_evidence(r, &right_samples);
    let evidence_mapping = outcome_report.then(|| ChangeEvidenceMapping {
        observation: Some(ObservationKey {
            kind: key.0.into(),
            operation: key.1.into(),
            path: key.2.into(),
        }),
        sample_limit_per_side: EVIDENCE_SAMPLE_LIMIT,
        left: normalized_mapping(l, left, left_samples),
        right: normalized_mapping(r, right, right_samples),
    });
    Some(Change {
        kind,
        category: kind.category().into(),
        subject: format!("{} ({})", key.2, key.1),
        left: left.keys().map(|outcome| (*outcome).to_owned()).collect(),
        right: right.keys().map(|outcome| (*outcome).to_owned()).collect(),
        left_evidence,
        right_evidence,
        rank_key: RankKey::pending(),
        file_path: (key.0 == "file").then(|| key.2.to_owned()),
        evidence_mapping,
    })
}

fn compare_version(l: &Run, r: &Run, format: ReportFormat) -> Report {
    let trace_report = format != ReportFormat::Automatic
        || l.metadata.schema_version == 4
        || r.metadata.schema_version == 4;
    let outcome_report = format == ReportFormat::OutcomeV6;
    // Internal callers can construct wire DTOs directly. Rechecking their pure
    // facts closes that seam without coupling comparison to filesystem storage.
    let checked_left = checked_run(l);
    let checked_right = checked_run(r);
    if matches!(checked_left, LoadOutcome::Failed(_))
        || matches!(checked_right, LoadOutcome::Failed(_))
        || raw_failed(l)
        || raw_failed(r)
    {
        return error_report(
            &l.metadata.name,
            &r.metadata.name,
            checked_left,
            checked_right,
            trace_report,
            outcome_report,
        );
    }
    let mut report = Report {
        schema_version: 4,
        observed_differences: None,
        reliability: Reliability {
            left: Quality::of(l),
            right: Quality::of(r),
        },
        exit_code: 3,
        errors: vec![],
        left: l.metadata.name.clone(),
        right: r.metadata.name.clone(),
        left_id: l.metadata.id.clone(),
        right_id: r.metadata.id.clone(),
        changes: vec![],
        warnings: vec![],
        target_provenance: None,
        evidence_contract: None,
    };
    if trace_report {
        apply_trace_report(&mut report, Some(l), Some(r));
    }
    if outcome_report {
        apply_outcome_report(&mut report);
    }
    append_metadata_changes(&mut report.changes, l, r, outcome_report);
    // Consume each ordered bucket after its samples/full mappings are owned by
    // the report. Outcome sets still erase duplicate count from identity, while
    // every saved index survives reference materialization before bucket drop.
    let a = aggregate(l, format);
    let b = aggregate(r, format);
    let empty_outcomes = OutcomeEvidence::new();
    for (key, left_owned, right_owned) in ordered_aggregates(a, b) {
        let left = left_owned.as_ref().unwrap_or(&empty_outcomes);
        let right = right_owned.as_ref().unwrap_or(&empty_outcomes);
        if let Some(change) = observation_change(l, r, key, left, right, outcome_report) {
            report.changes.push(change);
        }
    }
    for run in [l, r] {
        if run.storage_integrity.status != "verified" {
            report.warnings.push(format!(
                "{}: {}",
                run.metadata.name, run.storage_integrity.detail
            ));
        }
        if run.metadata.environment.is_none() {
            report.warnings.push(format!(
                "{}: legacy record has no environment selection data",
                run.metadata.name
            ));
        }
        if run.metadata.state != "completed"
            || !run.metadata.warnings.is_empty()
            || run.metadata.completeness.as_deref() != Some("complete_for_supported_events")
        {
            report.warnings.push(format!(
                "{}: state={}, {} parser warnings; inspect metadata.json",
                run.metadata.name,
                run.metadata.state,
                run.metadata.warnings.len()
            ));
        }
    }
    // Preserve comparison-production order as an explicit tiebreak before
    // sorting by semantic rank. Presentation may add a compatibility view, but
    // it never needs to infer this order from display strings.
    for (comparison_order, change) in report.changes.iter_mut().enumerate() {
        change.rank_key = RankKey {
            class: rank_class(change),
            comparison_order,
        };
    }
    report.changes.sort_by_key(|change| change.rank_key);
    if trace_report {
        for change in &mut report.changes {
            if change.kind == ChangeKind::ExitStatus {
                let target_samples = |run: &Run| {
                    if outcome_report && run.storage_integrity.status != "verified" {
                        vec![]
                    } else {
                        run.metadata
                            .target_provenance
                            .as_ref()
                            .map(|p| vec![p.target_exit.evidence.clone()])
                            .unwrap_or_default()
                    }
                };
                change.left_evidence = target_samples(l);
                change.right_evidence = target_samples(r);
            }
        }
    }
    report.observed_differences = Some(!report.changes.is_empty());
    report.exit_code = if report.reliability.left.trusted() && report.reliability.right.trusted() {
        i32::from(!report.changes.is_empty())
    } else {
        3
    };
    report
}

/// Independent capture and storage quality for one comparison side.
#[derive(Debug, Serialize)]
pub(crate) struct Quality {
    /// Capture reliability classification independent of storage integrity.
    pub(crate) capture: String,
    /// Storage integrity classification independent of capture outcome.
    pub(crate) storage: String,
    /// Independent raw integrity in v5; absent in legacy v4 JSON.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) raw: Option<String>,
    /// Validated saved target provenance in v5, never origin authentication.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) provenance: Option<String>,
}
impl Quality {
    /// Summarize recorded capture facts without promoting readable legacy data.
    ///
    /// Unfinalized storage cannot supply a final capture conclusion. A completed
    /// target alone is insufficient: its saved completeness must also be complete.
    /// This relies on the caller's validated metadata; it is not a second parser.
    fn of(run: &Run) -> Self {
        Self {
            capture: if run.storage_integrity.status == "unfinalized" {
                "unknown"
            } else if run.metadata.state == "completed"
                && run.metadata.completeness.as_deref() == Some("complete_for_supported_events")
            {
                "complete"
            } else if run.metadata.completeness.is_none() {
                "unknown"
            } else {
                "incomplete"
            }
            .into(),
            storage: run.storage_integrity.status.clone(),
            raw: None,
            provenance: None,
        }
    }
    /// Require the selected contract's independent claims before trusted status 0/1.
    fn trusted(&self) -> bool {
        self.capture == "complete"
            && self.storage == "verified"
            && self
                .raw
                .as_deref()
                .is_none_or(|status| status == "verified")
            && self
                .provenance
                .as_deref()
                .is_none_or(|status| status == "verified")
    }
}

/// Per-side reliability facts in the selected diff compatibility view.
#[derive(Debug, Serialize)]
pub(crate) struct Reliability {
    /// Reliability of the left loaded input.
    pub(crate) left: Quality,
    /// Reliability of the right loaded input.
    pub(crate) right: Quality,
}

/// Evaluate loaded-or-failed inputs while preserving the four-way diff status.
///
/// Load failures produce status 2, `observed_differences = None`, and no ordinary
/// changes. Successfully loaded but untrusted evidence remains a status-3 report.
/// A failed side is never replaced with an empty run: absence of loaded evidence
/// cannot establish which observations appeared or disappeared. Neutral failure
/// kinds keep this policy independent of concrete storage/I/O error types.
#[cfg(test)]
pub(crate) fn evaluate(
    left: &str,
    right: &str,
    l: LoadOutcome<Run>,
    r: LoadOutcome<Run>,
) -> Report {
    evaluate_report(left, right, l, r, ReportFormat::Automatic)
}

/// Select the explicitly requested or loaded new contract without performing I/O.
/// Explicit v5 also fixes the error format when neither damaged side can load.
#[cfg(test)]
pub(crate) fn evaluate_version(
    left: &str,
    right: &str,
    l: LoadOutcome<Run>,
    r: LoadOutcome<Run>,
    force_v5: bool,
) -> Report {
    evaluate_report(
        left,
        right,
        l,
        r,
        if force_v5 {
            ReportFormat::TraceV5
        } else {
            ReportFormat::Automatic
        },
    )
}

/// Evaluate one explicitly selected or automatic version without performing I/O.
///
/// V6 adds complete reference coverage and withholds valid unfinalized DTO
/// observations after validation. It preserves v5's independent reliability
/// requirements and never promotes readable historical evidence. Explicit format
/// selection also applies when neither input loads, so error reports are stable.
pub(crate) fn evaluate_report(
    left: &str,
    right: &str,
    l: LoadOutcome<Run>,
    r: LoadOutcome<Run>,
    format: ReportFormat,
) -> Report {
    let is_new = |result: &LoadOutcome<Run>| matches!(result, LoadOutcome::Loaded(run) if run.metadata.schema_version == 4);
    let trace_report = format != ReportFormat::Automatic || is_new(&l) || is_new(&r);
    let outcome_report = format == ReportFormat::OutcomeV6;
    match (l, r) {
        (LoadOutcome::Loaded(l), LoadOutcome::Loaded(r)) => {
            if format == ReportFormat::Automatic {
                compare(&l, &r)
            } else {
                compare_version(&l, &r, format)
            }
        }
        (l, r) => {
            let l = match &l {
                LoadOutcome::Loaded(run) => checked_run(run),
                LoadOutcome::Failed(failure) => LoadOutcome::Failed(failure.clone()),
            };
            let r = match &r {
                LoadOutcome::Loaded(run) => checked_run(run),
                LoadOutcome::Failed(failure) => LoadOutcome::Failed(failure.clone()),
            };
            error_report(left, right, l, r, trace_report, outcome_report)
        }
    }
}

/// Translate direct-construction contract failures into the ordinary tool-error path.
fn checked_run(run: &Run) -> LoadOutcome<&Run> {
    match validate_comparison_run(run) {
        Ok(()) => LoadOutcome::Loaded(run),
        Err(error) => LoadOutcome::Failed(StorageLoadFailure {
            kind: error.kind,
            message: format!("comparison input contract: {error}"),
        }),
    }
}

fn error_report(
    left: &str,
    right: &str,
    l: LoadOutcome<&Run>,
    r: LoadOutcome<&Run>,
    trace_report: bool,
    outcome_report: bool,
) -> Report {
    let mut errors = vec![];
    let mut quality = |side: &str, result: &LoadOutcome<&Run>| match result {
        LoadOutcome::Loaded(run) => {
            if raw_failed(run) {
                errors.push(format!(
                    "{side}: raw evidence: {}",
                    run.raw_integrity.as_ref().unwrap().detail
                ));
            }
            Quality::of(run)
        }
        LoadOutcome::Failed(failure) => {
            errors.push(format!("{side}: {}", failure.message));
            Quality {
                capture: "unknown".into(),
                storage: match failure.kind {
                    StorageLoadFailureKind::Corrupt => "corrupt",
                    StorageLoadFailureKind::Unsupported
                    | StorageLoadFailureKind::Unavailable
                    | StorageLoadFailureKind::Busy => "unavailable",
                }
                .into(),
                raw: None,
                provenance: None,
            }
        }
    };
    let reliability = Reliability {
        left: quality("left", &l),
        right: quality("right", &r),
    };
    let mut report = Report {
        schema_version: 4,
        observed_differences: None,
        reliability,
        exit_code: 2,
        errors,
        left: left.into(),
        right: right.into(),
        left_id: String::new(),
        right_id: String::new(),
        changes: vec![],
        warnings: vec![],
        target_provenance: None,
        evidence_contract: None,
    };
    if trace_report {
        let loaded = |side| match side {
            LoadOutcome::Loaded(run) => Some(run),
            LoadOutcome::Failed(_) => None,
        };
        apply_trace_report(&mut report, loaded(l), loaded(r));
    }
    if outcome_report {
        apply_outcome_report(&mut report);
    }
    report
}

fn raw_failed(run: &Run) -> bool {
    run.raw_integrity
        .as_ref()
        .is_some_and(|raw| matches!(raw.status.as_str(), "corrupt" | "unavailable"))
}

/// V5 strengthens trust only by inspecting already-computed component facts.
fn apply_trace_report(report: &mut Report, left: Option<&Run>, right: Option<&Run>) {
    report.schema_version = 5;
    for (run, quality) in [
        (left, &mut report.reliability.left),
        (right, &mut report.reliability.right),
    ] {
        let new = run.filter(|r| r.metadata.schema_version == 4);
        quality.raw = Some(
            new.and_then(|r| r.raw_integrity.as_ref())
                .map_or("unverified", |r| r.status.as_str())
                .into(),
        );
        quality.provenance = Some(
            match new {
                Some(r) if r.storage_integrity.status == "unfinalized" => "unverified",
                Some(r) if r.metadata.target_provenance.is_some() => "verified",
                Some(_) => "missing",
                None => "unverified",
            }
            .into(),
        );
    }
    report.target_provenance = Some(TargetProvenanceSides {
        left: left.and_then(|r| r.metadata.target_provenance.clone()),
        right: right.and_then(|r| r.metadata.target_provenance.clone()),
    });
    if report.exit_code != 2 {
        for (label, quality) in [
            (&report.left, &report.reliability.left),
            (&report.right, &report.reliability.right),
        ] {
            if quality.raw.as_deref() != Some("verified")
                || quality.provenance.as_deref() != Some("verified")
            {
                report.warnings.push(format!(
                    "{label}: trace-v1 raw/provenance requirements are unverified or missing"
                ));
            }
        }
    }
}

/// Announce the exhaustive changed-key mapping without changing trust or status.
fn apply_outcome_report(report: &mut Report) {
    report.schema_version = 6;
    report.evidence_contract = Some("outcome_event_references_v1");
}

#[cfg(test)]
pub(crate) mod field_regressions {
    use super::*;
    // Synthetic trace events retain their original coordinates. Metadata now
    // satisfies the real current-record contract; the fixture is still not proof
    // of on-disk checksums or actual capture origin.
    pub(crate) fn run(raw: &str) -> Run {
        let parsed = crate::parse::parse_with_cwd(raw, "raw/trace.log", Some("/work"));
        assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
        Run {
            metadata: serde_json::from_value(serde_json::json!({
                "schema_version":3,"tool_version":"test","id":"abc001-101","name":"test",
                "command":["tool"],"cwd":"/work","started":0,"ended":1,
                "backend":"fixture","state":"completed","exit_code":0,"signal":null,
                "warnings":[],"parsed_events":parsed.events.len(),"raw_lines":parsed.lines,
                "environment":{},"completeness":"complete_for_supported_events",
                "collector_exit_code":0,"collector_signal":null,"target_exit_raw":"+++ exited with 0 +++"
            }))
            .unwrap(),
            events: parsed.events,
            storage_integrity: crate::model::StorageIntegrity {
                status: "verified".into(),
                detail: String::new(),
            },
            raw_integrity: None,
        }
    }
    #[test]
    fn valid_comparison_fixture_satisfies_current_contract() {
        validate_comparison_run(&run(concat!(
            "1 execve(\"/tool\", [], 0) = 0\n",
            "1 openat(AT_FDCWD, \"/input\", O_RDONLY) = 3\n",
            "1 clone(flags=SIGCHLD) = 2\n",
            "2 +++ exited with 0 +++\n",
            "1 +++ exited with 0 +++\n",
        )))
        .unwrap();
    }

    #[test]
    fn invalid_current_inputs_cannot_be_trusted_equality() {
        for field in ["id", "collector", "path", "family", "operation"] {
            let mut invalid = run("1 openat(AT_FDCWD, \"/input\", O_RDONLY) = 3\n");
            match field {
                "id" => invalid.metadata.id = "test".into(),
                "collector" => invalid.metadata.collector_exit_code = None,
                "path" => invalid.events[0].path = None,
                "family" => invalid.events[0].kind = "unknown".into(),
                "operation" => invalid.events[0].operation = "exit".into(),
                _ => unreachable!(),
            }
            let healthy = run("");
            let report = compare(&invalid, &healthy);
            assert_eq!(report.exit_code, 2, "{field}");
            assert_eq!(report.observed_differences, None, "{field}");
            assert!(report.changes.is_empty(), "{field}");
            assert_eq!(report.reliability.left.storage, "corrupt");
            assert_eq!(report.reliability.right.storage, "verified");
            assert_eq!(report.errors.len(), 1);
        }
    }

    #[test]
    fn unfinalized_loader_withholding_is_not_a_count_corruption() {
        let mut run = run("1 openat(AT_FDCWD, \"/input\", O_RDONLY) = 3\n");
        assert_eq!(run.metadata.parsed_events, 1);
        run.events.clear();
        run.storage_integrity.status = "unfinalized".into();
        let report = compare(&run, &run);
        assert_eq!(report.exit_code, 3);
        assert_eq!(report.observed_differences, Some(false));
        assert!(report.errors.is_empty());
        assert_eq!(report.reliability.left.capture, "unknown");
    }

    #[test]
    fn unfinalized_nonempty_and_unsupported_dtos_are_still_checked() {
        let mut invalid = run("1 openat(AT_FDCWD, \"/input\", O_RDONLY) = 3\n");
        invalid.storage_integrity.status = "unfinalized".into();
        invalid.events[0].path = None;
        assert_eq!(compare(&invalid, &invalid).exit_code, 2);
        invalid.events.clear();
        invalid.metadata.schema_version = 999;
        invalid.storage_integrity.status = "unverified".into();
        let report = compare(&invalid, &invalid);
        assert_eq!(report.exit_code, 2);
        assert_eq!(report.observed_differences, None);
        assert_eq!(report.reliability.left.storage, "unavailable");
        invalid.metadata.schema_version = 1;
        invalid.storage_integrity.status = "unfinalized".into();
        // A completed legacy record is loaded with its events. Its claimed
        // count cannot use the current/running withholding exception.
        assert_eq!(compare(&invalid, &invalid).exit_code, 2);
    }

    #[test]
    fn failed_integrity_facts_cannot_masquerade_as_loaded_observations() {
        for status in ["corrupt", "unsupported", "unavailable", "future_status"] {
            let mut invalid = run("");
            invalid.storage_integrity.status = status.into();
            let report = compare(&invalid, &run(""));
            assert_eq!(report.exit_code, 2, "{status}");
            assert_eq!(report.observed_differences, None);
            assert!(report.changes.is_empty());
            assert_eq!(report.reliability.right.storage, "verified");
        }
    }

    #[test]
    fn ranking_uses_semantic_kind_independent_of_english_wording() {
        let cases = [
            (ChangeKind::ExitStatus, RankClass::ExitStatus),
            (
                ChangeKind::CaptureCompleteness,
                RankClass::CaptureCompleteness,
            ),
            (
                ChangeKind::InitialWorkingDirectory,
                RankClass::InvocationContext,
            ),
            (ChangeKind::CommandArguments, RankClass::InvocationContext),
            (
                ChangeKind::SelectedStartupEnvironment,
                RankClass::InvocationContext,
            ),
            (
                ChangeKind::ExecutionAttempt,
                RankClass::FailedExecutionAttempt,
            ),
            (ChangeKind::Executable, RankClass::NewExecutable),
            (ChangeKind::NewFileAccess, RankClass::NewFileAccess),
            (ChangeKind::RemovedFileAccess, RankClass::OtherObservation),
            (ChangeKind::FileAccessOutcome, RankClass::OutcomeSetChange),
        ];
        for (kind, expected) in cases {
            let right = if kind == ChangeKind::ExecutionAttempt {
                "ENOENT"
            } else {
                "success"
            };
            let mut change = Change {
                kind,
                category: kind.category().into(),
                subject: String::new(),
                left: vec![],
                right: vec![right.into()],
                left_evidence: vec![],
                right_evidence: vec![],
                rank_key: RankKey::pending(),
                file_path: None,
                evidence_mapping: None,
            };
            assert_eq!(rank_class(&change), expected);
            change.category = "arbitrary presentation wording".into();
            assert_eq!(rank_class(&change), expected);
        }
    }

    #[test]
    fn impossible_wait_outcomes_are_rejected_and_numeric_boundaries_are_accepted() {
        for (code, signal, accepted) in [
            (Some(0), None, true),
            (Some(255), None, true),
            (Some(-1), None, false),
            (Some(256), None, false),
            (None, Some(1), true),
            (None, Some(64), true),
            (None, Some(-1), false),
            (None, Some(0), false),
            (None, Some(65), false),
        ] {
            let mut run = run("");
            run.metadata.exit_code = code;
            run.metadata.collector_exit_code = code;
            run.metadata.signal = signal;
            run.metadata.collector_signal = signal;
            assert_eq!(
                validate_comparison_run(&run).is_ok(),
                accepted,
                "{code:?}/{signal:?}"
            );
            assert_eq!(compare(&run, &run).exit_code, if accepted { 0 } else { 2 });
        }
    }
    #[test]
    fn failed_exec_is_visible_and_never_called_successful_execution() {
        let l = run("");
        let r = run("2 execve(\"/tools/unavailable\", [], 0x0) = -1 ENOENT (No such file)\n");
        let report = compare(&l, &r);
        assert_eq!(report.exit_code, 1);
        let c = report
            .changes
            .iter()
            .find(|c| c.subject.contains("/tools/unavailable"))
            .unwrap();
        assert_eq!(c.category, "Execution attempt changes");
        assert_eq!(c.right, ["ENOENT"]);
        assert_eq!(c.right_evidence[0].line, 1);
    }
    #[test]
    fn stable_input_failure_precedes_probes_and_downstream_exec() {
        let l = run(
            "1 open(\"/z/settings.ini\", O_RDONLY) = 3\n1 access(\"/a/probe\", F_OK) = -1 ENOENT (No file)\n1 execve(\"/a/child\", [], 0) = 0\n",
        );
        let r = run("2 open(\"/z/settings.ini\", O_RDONLY) = -1 EACCES (Denied)\n");
        let report = compare(&l, &r);
        assert!(report.changes[0].subject.contains("/z/settings.ini"));
    }
    #[test]
    fn mixed_probe_outcomes_do_not_outrank_a_single_success_to_failure() {
        let l = run(
            "1 open(\"/a/cache\", O_RDONLY) = -1 ENOENT (No file)\n1 open(\"/a/cache\", O_RDONLY) = 3\n1 open(\"/z/input\", O_RDONLY) = 4\n",
        );
        let r = run(
            "2 open(\"/a/cache\", O_RDONLY) = -1 ENOENT (No file)\n2 open(\"/z/input\", O_RDONLY) = -1 ENOENT (No file)\n",
        );
        let report = compare(&l, &r);
        assert_eq!(report.changes.len(), 2);
        assert!(report.changes[0].subject.contains("/z/input"));
        assert_eq!(report.changes[1].left, ["ENOENT", "success"]);
        assert_eq!(
            report.changes[1].rank_key.class(),
            RankClass::OutcomeSetChange
        );
    }
    #[test]
    fn failed_then_successful_exec_is_an_attempt_change_not_disappearance() {
        let l = run("1 execve(\"/tool\", [], 0) = 0\n");
        let r = run(
            "2 execve(\"/tool\", [], 0) = -1 EACCES (Denied)\n2 execve(\"/tool\", [], 0) = 0\n",
        );
        let report = compare(&l, &r);
        assert_eq!(report.changes[0].category, "Execution attempt changes");
        assert_eq!(report.changes[0].right, ["EACCES", "success"]);
    }

    #[test]
    fn changed_outcomes_receive_evidence_before_common_samples() {
        let l = run(concat!(
            "1 open(\"/input\", O_RDONLY) = -1 EACCES (Denied)\n",
            "1 open(\"/input\", O_RDONLY) = -1 EACCES (Denied)\n",
            "1 open(\"/input\", O_RDONLY) = -1 EACCES (Denied)\n",
            "1 open(\"/input\", O_RDONLY) = 3\n",
        ));
        let r = run(concat!(
            "2 open(\"/input\", O_RDONLY) = -1 EACCES (Denied)\n",
            "2 open(\"/input\", O_RDONLY) = -1 EACCES (Denied)\n",
            "2 open(\"/input\", O_RDONLY) = -1 EACCES (Denied)\n",
            "2 open(\"/input\", O_RDONLY) = -1 ENOENT (Missing)\n",
        ));

        let report = compare(&l, &r);
        let change = report
            .changes
            .iter()
            .find(|change| change.subject.contains("/input"))
            .unwrap();

        assert!(
            change
                .left_evidence
                .iter()
                .any(|evidence| evidence.line == 4),
            "left-only success evidence was omitted: {change:?}"
        );
        assert!(
            change
                .right_evidence
                .iter()
                .any(|evidence| evidence.line == 4),
            "right-only ENOENT evidence was omitted: {change:?}"
        );
    }

    #[test]
    fn raw_target_outcome_is_compared_only_when_structured_status_is_unknown() {
        let mut l = run("");
        let mut r = run("");
        for (run, raw) in [
            (&mut l, "+++ killed by SIGFUTURE_A +++"),
            (&mut r, "+++ killed by SIGFUTURE_B +++"),
        ] {
            run.metadata.exit_code = None;
            run.metadata.signal = None;
            run.metadata.target_exit_raw = Some(raw.into());
            run.metadata.completeness = Some("partial".into());
            run.metadata.warnings = vec!["unrecognized target termination signal".into()];
        }

        let unknown = compare(&l, &r);
        assert_eq!(unknown.exit_code, 3);
        assert_eq!(unknown.observed_differences, Some(true));
        assert!(
            unknown
                .changes
                .iter()
                .any(|change| change.category == "Exit status")
        );

        r.metadata.target_exit_raw = l.metadata.target_exit_raw.clone();
        assert_eq!(compare(&l, &r).observed_differences, Some(false));

        l.metadata.exit_code = Some(7);
        r.metadata.exit_code = Some(7);
        l.metadata.collector_exit_code = Some(7);
        r.metadata.collector_exit_code = Some(7);
        r.metadata.target_exit_raw = Some("different presentation".into());
        assert_eq!(compare(&l, &r).observed_differences, Some(false));
    }
    #[test]
    fn resumed_and_unsplit_execution_have_no_semantic_difference() {
        let l = run(
            "1 execve(\"/tool\", [], 0 <unfinished ...>\n1 <... execve resumed>)             = 0\n",
        );
        let r = run("99 execve(\"/tool\", [], 0) = 0\n");
        assert_eq!(compare(&l, &r).observed_differences, Some(false));
    }
}

#[cfg(test)]
mod presentation_tests {
    use super::*;
    use crate::model::StorageLoadFailure;

    #[test]
    fn neutral_load_failure_matrix_preserves_the_four_way_diff_contract() {
        for (kind, storage) in [
            (StorageLoadFailureKind::Corrupt, "corrupt"),
            (StorageLoadFailureKind::Unsupported, "unavailable"),
            (StorageLoadFailureKind::Unavailable, "unavailable"),
            (StorageLoadFailureKind::Busy, "unavailable"),
        ] {
            let report = evaluate(
                "failed-side",
                "loaded-side",
                LoadOutcome::Failed(StorageLoadFailure {
                    kind,
                    message: format!("{kind:?} load failure"),
                }),
                LoadOutcome::Loaded(super::field_regressions::run("")),
            );
            assert_eq!(report.exit_code, 2);
            assert_eq!(report.observed_differences, None);
            assert!(report.changes.is_empty());
            assert_eq!(report.reliability.left.capture, "unknown");
            assert_eq!(report.reliability.left.storage, storage);
            assert_eq!(report.reliability.right.storage, "verified");
            assert_eq!(report.errors, [format!("left: {kind:?} load failure")]);
        }
    }

    #[test]
    fn loaded_unverified_and_unfinalized_evidence_remains_diff_status_three() {
        for status in ["unverified", "unfinalized"] {
            let mut left = super::field_regressions::run("");
            left.storage_integrity.status = status.into();
            left.storage_integrity.detail = format!("{status} evidence");
            let report = evaluate(
                "left",
                "right",
                LoadOutcome::Loaded(left),
                LoadOutcome::Loaded(super::field_regressions::run("")),
            );
            assert_eq!(report.exit_code, 3, "{status}");
            assert_eq!(report.observed_differences, Some(false), "{status}");
            assert!(report.errors.is_empty(), "{status}");
            assert_eq!(report.reliability.left.storage, status);
        }
    }
}

#[cfg(test)]
mod trace_report_tests {
    //! New trust requirements are separate from the historical comparison identity.
    use super::*;
    use crate::model::trace_contract_tests::run_fixture;

    #[test]
    fn trace_report_requires_raw_and_target_provenance_for_trusted_status() {
        let left = run_fixture();
        let right = left.clone();
        let equal = compare(&left, &right);
        assert_eq!(
            (
                equal.schema_version,
                equal.exit_code,
                equal.observed_differences
            ),
            (5, 0, Some(false))
        );
        let mut changed = right;
        let reference = changed
            .metadata
            .target_provenance
            .as_ref()
            .unwrap()
            .target_exit
            .index;
        changed.events[reference].outcome = "+++ exited with 7 +++".into();
        changed.metadata.target_exit_raw = Some(changed.events[reference].outcome.clone());
        changed.metadata.exit_code = Some(7);
        changed.metadata.collector_exit_code = Some(7);
        let difference = compare(&left, &changed);
        assert_eq!(
            (difference.exit_code, difference.observed_differences),
            (1, Some(true))
        );
        let exit = difference
            .changes
            .iter()
            .find(|c| c.kind == ChangeKind::ExitStatus)
            .unwrap();
        assert_eq!(
            exit.left_evidence,
            vec![left.events[reference].evidence.clone()]
        );
        assert_eq!(
            exit.right_evidence,
            vec![changed.events[reference].evidence.clone()]
        );

        // These are loader-supplied facts: corrupt raw must not erase already
        // verified structured data or leak a partial ordinary conclusion.
        for status in ["corrupt", "unavailable"] {
            let mut damaged = left.clone();
            damaged.raw_integrity.as_mut().unwrap().status = status.into();
            let error = compare(&damaged, &changed);
            assert_eq!((error.exit_code, error.observed_differences), (2, None));
            assert!(error.changes.is_empty());
            assert_eq!(error.reliability.left.storage, "verified");
            assert_eq!(error.reliability.left.capture, "complete");
            assert_eq!(error.reliability.left.raw.as_deref(), Some(status));
            assert_eq!(error.reliability.right.raw.as_deref(), Some("verified"));
        }
    }

    #[test]
    fn mixed_and_explicit_legacy_reports_never_promote_raw_or_provenance() {
        let old = super::field_regressions::run("");
        assert_eq!(
            (
                compare(&old, &old).schema_version,
                compare(&old, &old).exit_code
            ),
            (4, 0)
        );
        let forced = evaluate_version(
            "old",
            "old",
            LoadOutcome::Loaded(old.clone()),
            LoadOutcome::Loaded(old.clone()),
            true,
        );
        assert_eq!(
            (
                forced.schema_version,
                forced.exit_code,
                forced.observed_differences
            ),
            (5, 3, Some(false))
        );
        assert_eq!(forced.reliability.left.raw.as_deref(), Some("unverified"));
        assert_eq!(
            forced.reliability.left.provenance.as_deref(),
            Some("unverified")
        );
        let mixed = compare(&run_fixture(), &old);
        assert_eq!((mixed.schema_version, mixed.exit_code), (5, 3));
        assert_eq!(
            mixed.reliability.left.provenance.as_deref(),
            Some("verified")
        );
        assert_eq!(mixed.reliability.right.raw.as_deref(), Some("unverified"));
    }

    #[test]
    fn explicitly_selected_v5_survives_two_load_failures() {
        let failure = || {
            LoadOutcome::Failed(StorageLoadFailure {
                kind: StorageLoadFailureKind::Corrupt,
                message: "damaged structured input".into(),
            })
        };
        let report = evaluate_version("left", "right", failure(), failure(), true);
        assert_eq!(
            (
                report.schema_version,
                report.exit_code,
                report.observed_differences
            ),
            (5, 2, None)
        );
        assert_eq!(report.errors.len(), 2);
        assert!(report.changes.is_empty());
        let provenance = report.target_provenance.unwrap();
        assert!(provenance.left.is_none() && provenance.right.is_none());
    }
}

#[cfg(test)]
mod outcome_report_tests {
    //! V6 freezes full saved-reference coverage without changing observation identity.
    use super::*;
    use crate::model::{RawReceipt, trace_contract_tests::run_fixture};

    /// Synthetic DTO assertions exercise reference contracts, not actual raw hashes.
    fn trace_files(path: &str, observations: &[(&str, usize, usize)]) -> Run {
        let mut run = run_fixture();
        let file = run.events[1].clone();
        let mut exit = run.events[2].clone();
        run.events.truncate(1);
        for &(outcome, line, end_line) in observations {
            let mut event = file.clone();
            event.path = Some(path.into());
            event.outcome = outcome.into();
            event.evidence.line = line;
            event.evidence.end_line = end_line;
            run.events.push(event);
        }
        let last_line = observations
            .iter()
            .map(|&(_, _, end_line)| end_line)
            .max()
            .unwrap_or(1)
            + 1;
        exit.evidence.line = last_line;
        exit.evidence.end_line = last_line;
        let reference = EventReference {
            index: run.events.len(),
            evidence: exit.evidence.clone(),
        };
        run.events.push(exit);
        run.metadata.target_provenance.as_mut().unwrap().target_exit = reference;
        run.metadata.parsed_events = run.events.len();
        run.metadata.raw_lines = last_line;
        if let RawReceipt::Parsed { logical_lines, .. } = &mut run
            .raw_integrity
            .as_mut()
            .unwrap()
            .manifest
            .as_mut()
            .unwrap()
            .receipt
        {
            *logical_lines = last_line;
        }
        validate_comparison_run(&run).unwrap();
        run
    }

    fn report(left: &Run, right: &Run) -> Report {
        evaluate_report(
            "left",
            "right",
            LoadOutcome::Loaded(left.clone()),
            LoadOutcome::Loaded(right.clone()),
            ReportFormat::OutcomeV6,
        )
    }

    #[test]
    fn v6_nonempty_unfinalized_v3_uses_one_withholding_rule_for_all_event_views() {
        // A direct DTO may retain valid v3 events even though the storage loader
        // normally withholds them. Keep this accepted model boundary intact.
        let mut withheld = super::field_regressions::run(concat!(
            "1 openat(AT_FDCWD, \"/admitted\", O_RDONLY) = -1 EACCES (denied)\n",
            "1 openat(AT_FDCWD, \"/withheld-only\", O_RDONLY) = 3\n",
            "1 execve(\"/withheld-exec\", [], 0) = -1 ENOENT (missing)\n",
        ));
        withheld.storage_integrity.status = "unfinalized".into();
        let admitted =
            super::field_regressions::run("1 openat(AT_FDCWD, \"/admitted\", O_RDONLY) = 3\n");
        validate_comparison_run(&withheld).unwrap();
        validate_comparison_run(&admitted).unwrap();
        assert_eq!(withheld.metadata.schema_version, 3);
        assert!(!withheld.events.is_empty());
        let before = serde_json::to_value(&withheld).unwrap();
        let result = report(&withheld, &admitted);
        // Retained raw output freezes both the legal input and the contradictory
        // pre-fix report; the test does not rewrite the DTO to hide its evidence.
        eprintln!(
            "withheld DTO: {}",
            serde_json::to_string(&withheld).unwrap()
        );
        eprintln!(
            "admitted DTO: {}",
            serde_json::to_string(&admitted).unwrap()
        );
        eprintln!("v6 report: {}", crate::report::json(&result).unwrap());
        assert_eq!(result.exit_code, 3);
        for change in &result.changes {
            let side = &change.evidence_mapping.as_ref().unwrap().left;
            assert_eq!(side.source, EvidenceSource::Withheld);
            assert!(
                change.left_evidence.is_empty(),
                "withheld mapping must agree with the flat evidence view"
            );
            assert!(
                change.left.is_empty(),
                "withheld events cannot supply outcomes"
            );
            assert_eq!((side.event_count, side.omitted_sample_count), (None, None));
            assert_eq!(side.sample_count, 0);
            assert!(side.outcomes.is_empty() && side.sample_event_indices.is_empty());
        }
        assert_eq!(
            result.changes.len(),
            1,
            "withheld-only keys cannot create changes"
        );
        let change = &result.changes[0];
        assert_eq!(change.kind, ChangeKind::NewFileAccess);
        assert_eq!(
            change
                .evidence_mapping
                .as_ref()
                .unwrap()
                .observation
                .as_ref()
                .unwrap()
                .path,
            "/admitted"
        );
        assert_eq!(change.right, ["success"]);
        assert_eq!(serde_json::to_value(&withheld).unwrap(), before);
        // If neither side admits events, their DTO payloads cannot manufacture
        // an event difference. Metadata remains compared as saved.
        let mut other_withheld = admitted.clone();
        other_withheld.storage_integrity.status = "unfinalized".into();
        let both = report(&withheld, &other_withheld);
        assert_eq!(
            (both.exit_code, both.observed_differences),
            (3, Some(false))
        );
        assert!(both.changes.is_empty());
    }

    #[test]
    fn v6_empty_and_nonempty_valid_unfinalized_v3_have_the_same_event_view() {
        let mut left =
            super::field_regressions::run("1 openat(AT_FDCWD, \"/withheld-only\", O_RDONLY) = 3\n");
        left.storage_integrity.status = "unfinalized".into();
        let right =
            super::field_regressions::run("1 openat(AT_FDCWD, \"/admitted\", O_RDONLY) = 3\n");
        let nonempty = report(&left, &right);
        let reversed = report(&right, &left);
        left.events.clear();
        // Existing model rules allow a withheld empty vector to retain its saved
        // metadata counter. Do not change that compatibility rule to fix diff.
        assert!(left.metadata.parsed_events > 0);
        validate_comparison_run(&left).unwrap();
        assert_eq!(
            crate::report::json(&nonempty).unwrap(),
            crate::report::json(&report(&left, &right)).unwrap()
        );
        assert_eq!(
            crate::report::json(&reversed).unwrap(),
            crate::report::json(&report(&right, &left)).unwrap()
        );
        assert_eq!(
            (reversed.exit_code, reversed.observed_differences),
            (3, Some(true))
        );
        assert_eq!(reversed.changes.len(), 1);
        let change = &reversed.changes[0];
        assert!(change.right.is_empty() && change.right_evidence.is_empty());
        let side = &change.evidence_mapping.as_ref().unwrap().right;
        assert_eq!(side.source, EvidenceSource::Withheld);
        assert_eq!((side.event_count, side.omitted_sample_count), (None, None));
        assert_eq!(side.sample_count, 0);
        assert!(side.outcomes.is_empty() && side.sample_event_indices.is_empty());
    }

    #[test]
    fn v6_nonempty_corrupt_unfinalized_v3_is_validated_before_admission() {
        let healthy =
            super::field_regressions::run("1 openat(AT_FDCWD, \"/input\", O_RDONLY) = 3\n");
        for fault in ["coordinates", "path", "family", "count"] {
            let mut invalid = healthy.clone();
            invalid.storage_integrity.status = "unfinalized".into();
            match fault {
                "coordinates" => invalid.events[0].evidence.line = 0,
                "path" => invalid.events[0].path = None,
                "family" => invalid.events[0].kind = "unknown".into(),
                "count" => invalid.metadata.parsed_events += 1,
                _ => unreachable!(),
            }
            assert!(validate_comparison_run(&invalid).is_err(), "{fault}");
            let before = serde_json::to_value(&invalid).unwrap();
            for (left, right) in [(&invalid, &healthy), (&healthy, &invalid)] {
                let result = report(left, right);
                assert_eq!((result.schema_version, result.exit_code), (6, 2), "{fault}");
                assert_eq!(result.observed_differences, None, "{fault}");
                assert!(result.changes.is_empty(), "{fault}");
                assert!(!result.errors.is_empty(), "{fault}");
            }
            assert_eq!(serde_json::to_value(&invalid).unwrap(), before);
        }
    }

    #[test]
    fn v6_unverified_v3_keeps_all_observations_without_component_trust() {
        let mut left = super::field_regressions::run(
            "1 openat(AT_FDCWD, \"/input\", O_RDONLY) = -1 EACCES (denied)\n",
        );
        left.storage_integrity.status = "unverified".into();
        // Identical coordinates still describe three distinct saved occurrences.
        left.events
            .extend([left.events[0].clone(), left.events[0].clone()]);
        left.metadata.parsed_events = left.events.len();
        let right = super::field_regressions::run("1 openat(AT_FDCWD, \"/input\", O_RDONLY) = 3\n");
        validate_comparison_run(&left).unwrap();
        let before = serde_json::to_value(&left).unwrap();
        let result = report(&left, &right);
        assert_eq!(
            (result.exit_code, result.observed_differences),
            (3, Some(true))
        );
        assert_eq!(result.reliability.left.storage, "unverified");
        assert_eq!(result.reliability.left.raw.as_deref(), Some("unverified"));
        assert_eq!(
            result.reliability.left.provenance.as_deref(),
            Some("unverified")
        );
        assert_eq!(result.changes.len(), 1);
        let change = &result.changes[0];
        assert_eq!(change.left, ["EACCES"]);
        let mapping = change.evidence_mapping.as_ref().unwrap();
        assert_normalized_mapping(
            &mapping.left,
            &change.left_evidence,
            &left,
            mapping.observation.as_ref().unwrap(),
        );
        assert_eq!(mapping.left.event_count, Some(3));
        assert_eq!(mapping.left.sample_event_indices, [0]);
        assert_eq!(
            (mapping.left.sample_count, mapping.left.omitted_sample_count),
            (1, Some(2))
        );
        assert_eq!(
            mapping.left.outcomes[0]
                .events
                .iter()
                .map(|event| event.index)
                .collect::<Vec<_>>(),
            [0, 1, 2]
        );
        assert_eq!(serde_json::to_value(&left).unwrap(), before);
    }

    #[test]
    fn v6_withholds_events_without_suppressing_saved_metadata_changes() {
        let mut left =
            super::field_regressions::run("1 openat(AT_FDCWD, \"/withheld-only\", O_RDONLY) = 3\n");
        left.storage_integrity.status = "unfinalized".into();
        let mut right = left.clone();
        right.metadata.command.push("saved argument".into());
        let before = (
            serde_json::to_value(&left).unwrap(),
            serde_json::to_value(&right).unwrap(),
        );
        let result = report(&left, &right);
        assert_eq!(
            (result.exit_code, result.observed_differences),
            (3, Some(true))
        );
        assert_eq!(result.changes.len(), 1);
        let change = &result.changes[0];
        assert_eq!(change.kind, ChangeKind::CommandArguments);
        assert_eq!(change.left, [format!("{:?}", left.metadata.command)]);
        assert_eq!(change.right, [format!("{:?}", right.metadata.command)]);
        let mapping = change.evidence_mapping.as_ref().unwrap();
        assert!(mapping.observation.is_none());
        for side in [&mapping.left, &mapping.right] {
            assert_eq!(side.source, EvidenceSource::Withheld);
            assert_eq!(side.metadata_fields, ["metadata.command"]);
            assert_eq!((side.event_count, side.omitted_sample_count), (None, None));
            assert!(side.outcomes.is_empty() && side.sample_event_indices.is_empty());
        }
        assert!(change.left_evidence.is_empty() && change.right_evidence.is_empty());
        assert_eq!(
            (
                serde_json::to_value(&left).unwrap(),
                serde_json::to_value(&right).unwrap()
            ),
            before
        );
    }

    #[test]
    fn v6_admission_keeps_historical_v4_and_v5_nonempty_dto_observations() {
        let mut left = super::field_regressions::run(concat!(
            "1 openat(AT_FDCWD, \"/admitted\", O_RDONLY) = -1 EACCES (denied)\n",
            "1 openat(AT_FDCWD, \"/withheld-only\", O_RDONLY) = 3\n",
            "1 execve(\"/withheld-exec\", [], 0) = -1 ENOENT (missing)\n",
        ));
        left.storage_integrity.status = "unfinalized".into();
        let right =
            super::field_regressions::run("1 openat(AT_FDCWD, \"/admitted\", O_RDONLY) = 3\n");
        validate_comparison_run(&left).unwrap();
        for (label, format, version) in [
            ("V4", ReportFormat::Automatic, 4),
            ("V5", ReportFormat::TraceV5, 5),
        ] {
            let result = evaluate_report(
                "left",
                "right",
                LoadOutcome::Loaded(left.clone()),
                LoadOutcome::Loaded(right.clone()),
                format,
            );
            assert_eq!((result.schema_version, result.exit_code), (version, 3));
            assert_eq!(result.observed_differences, Some(true));
            assert_eq!(result.changes.len(), 3);
            let expected = [
                ("/admitted (openat)", "EACCES", 0),
                ("/withheld-only (openat)", "success", 1),
                ("/withheld-exec (execve)", "ENOENT", 2),
            ];
            for (subject, outcome, index) in expected {
                let change = result
                    .changes
                    .iter()
                    .find(|change| change.subject == subject)
                    .unwrap();
                assert_eq!(change.left, [outcome]);
                assert_eq!(change.left_evidence, [left.events[index].evidence.clone()]);
                assert!(change.evidence_mapping.is_none());
            }
            // Private pre-fix/fixed-tree replay compares these exact renderer
            // payloads, including whitespace and ordering, on the same DTO.
            print!("DTO_COMPAT_BEGIN_{label}_TEXT");
            print!("{}", crate::report::text(&result));
            println!("DTO_COMPAT_END_{label}_TEXT");
            print!("DTO_COMPAT_BEGIN_{label}_JSON");
            print!("{}", crate::report::json(&result).unwrap());
            println!("DTO_COMPAT_END_{label}_JSON");
        }
    }

    fn set_exit(run: &mut Run, code: i32) {
        let index = run
            .metadata
            .target_provenance
            .as_ref()
            .unwrap()
            .target_exit
            .index;
        run.events[index].outcome = format!("+++ exited with {code} +++");
        run.metadata.target_exit_raw = Some(run.events[index].outcome.clone());
        run.metadata.exit_code = Some(code);
        run.metadata.collector_exit_code = Some(code);
        validate_comparison_run(run).unwrap();
    }

    fn assert_normalized_mapping(
        mapping: &SideEvidenceMapping,
        samples: &[Evidence],
        run: &Run,
        key: &ObservationKey,
    ) {
        assert_eq!(mapping.source, EvidenceSource::NormalizedEvents);
        assert!(mapping.metadata_fields.is_empty());
        let count: usize = mapping
            .outcomes
            .iter()
            .map(|outcome| outcome.event_count)
            .sum();
        assert_eq!(mapping.event_count, Some(count));
        assert_eq!(mapping.sample_count, samples.len());
        assert_eq!(mapping.sample_count, mapping.sample_event_indices.len());
        assert_eq!(mapping.omitted_sample_count, Some(count - samples.len()));
        for (&index, evidence) in mapping.sample_event_indices.iter().zip(samples) {
            assert_eq!(&run.events[index].evidence, evidence);
            assert!(mapping.outcomes.iter().any(|outcome| {
                outcome
                    .events
                    .iter()
                    .any(|reference| reference.index == index)
            }));
        }
        let mut indices = BTreeSet::new();
        for outcome in &mapping.outcomes {
            assert_eq!(outcome.event_count, outcome.events.len());
            assert!(
                outcome
                    .events
                    .windows(2)
                    .all(|pair| pair[0].index < pair[1].index)
            );
            for reference in &outcome.events {
                assert!(indices.insert(reference.index), "occurrence mapped twice");
                let saved = &run.events[reference.index];
                assert_eq!(reference.evidence, saved.evidence);
                assert_eq!(outcome.outcome, saved.outcome);
                assert_eq!(key.kind, saved.kind);
                assert_eq!(key.operation, saved.operation);
                assert_eq!(Some(key.path.as_str()), saved.path.as_deref());
            }
        }
        let expected = run
            .events
            .iter()
            .enumerate()
            .filter(|(_, event)| {
                event.kind == key.kind
                    && event.operation == key.operation
                    && event.path.as_deref() == Some(key.path.as_str())
            })
            .map(|(index, _)| index)
            .collect::<BTreeSet<_>>();
        assert_eq!(indices, expected, "full mapping dropped a saved occurrence");
    }

    #[test]
    fn changed_and_common_outcomes_keep_every_reference_and_account_for_samples() {
        let left = trace_files(
            "/input",
            &[
                ("EACCES", 2, 2),
                ("EACCES", 3, 3),
                ("EACCES", 4, 4),
                ("EACCES", 5, 5),
                ("success", 6, 6),
            ],
        );
        let right = trace_files(
            "/input",
            &[
                ("EACCES", 2, 2),
                ("EACCES", 3, 3),
                ("EACCES", 4, 4),
                ("ENOENT", 5, 5),
            ],
        );
        let result = report(&left, &right);
        assert_eq!((result.schema_version, result.exit_code), (6, 1));
        assert_eq!(
            result.evidence_contract,
            Some("outcome_event_references_v1")
        );
        assert_eq!(result.changes.len(), 1);
        let change = &result.changes[0];
        let mapping = change.evidence_mapping.as_ref().unwrap();
        let key = mapping.observation.as_ref().unwrap();
        assert_eq!(mapping.sample_limit_per_side, 3);
        assert_eq!(mapping.left.sample_event_indices, [5, 1, 2]);
        assert_eq!(mapping.right.sample_event_indices, [4, 1, 2]);
        assert_eq!(mapping.left.omitted_sample_count, Some(2));
        assert_eq!(mapping.right.omitted_sample_count, Some(1));
        assert_normalized_mapping(&mapping.left, &change.left_evidence, &left, key);
        assert_normalized_mapping(&mapping.right, &change.right_evidence, &right, key);
        // The complete view and sample choice remain deterministic for the same DTOs.
        assert_eq!(
            serde_json::to_value(mapping).unwrap(),
            serde_json::to_value(
                report(&left, &right).changes[0]
                    .evidence_mapping
                    .as_ref()
                    .unwrap()
            )
            .unwrap()
        );
    }

    #[test]
    fn more_than_three_side_only_outcomes_have_full_maps_and_an_explicit_empty_side() {
        let path = "/input (openat)";
        let left = trace_files(
            path,
            &[
                ("success", 2, 2),
                ("EPERM", 3, 3),
                ("EACCES", 4, 4),
                ("ENOENT", 5, 5),
                ("EINVAL", 6, 6),
            ],
        );
        let right = trace_files(path, &[]);
        let result = report(&left, &right);
        let change = &result.changes[0];
        let mapping = change.evidence_mapping.as_ref().unwrap();
        let key = mapping.observation.as_ref().unwrap();
        assert_eq!(key.path, path);
        assert_eq!((&*key.kind, &*key.operation), ("file", "openat"));
        assert_eq!(mapping.left.outcomes.len(), 5);
        assert_eq!(mapping.left.sample_event_indices, [3, 5, 4]);
        assert_eq!(mapping.left.omitted_sample_count, Some(2));
        assert_eq!(mapping.right.source, EvidenceSource::NormalizedEvents);
        assert_eq!(mapping.right.event_count, Some(0));
        assert_eq!(mapping.right.omitted_sample_count, Some(0));
        assert!(mapping.right.outcomes.is_empty());
        assert_normalized_mapping(&mapping.left, &change.left_evidence, &left, key);
        assert_normalized_mapping(&mapping.right, &change.right_evidence, &right, key);
    }

    #[test]
    fn identical_coordinates_remain_distinct_full_occurrences_with_the_old_sample_policy() {
        let left = trace_files(
            "/input",
            &[("EACCES", 2, 2), ("EACCES", 2, 2), ("success", 2, 2)],
        );
        let right = trace_files("/input", &[("EINVAL", 3, 3)]);
        let result = report(&left, &right);
        let change = &result.changes[0];
        let mapping = change.evidence_mapping.as_ref().unwrap();
        assert_eq!(mapping.left.event_count, Some(3));
        assert_eq!(mapping.left.sample_event_indices, [1, 3]);
        assert_eq!(mapping.left.sample_count, 2);
        assert_eq!(mapping.left.omitted_sample_count, Some(1));
        assert_eq!(change.left_evidence[0], change.left_evidence[1]);
        assert_eq!(mapping.left.outcomes[0].events[0].index, 1);
        assert_eq!(mapping.left.outcomes[0].events[1].index, 2);
        assert_eq!(
            mapping.left.outcomes[0].events[0].evidence,
            mapping.left.outcomes[0].events[1].evidence
        );
        assert_normalized_mapping(
            &mapping.left,
            &change.left_evidence,
            &left,
            mapping.observation.as_ref().unwrap(),
        );
    }

    #[test]
    fn unfinished_calls_preserve_both_lines_and_saved_vector_indices() {
        let left = super::field_regressions::run(concat!(
            "1 clone(flags=SIGCHLD) = 2\n",
            "1 open(\"/input\", O_RDONLY <unfinished ...>\n",
            "2 open(\"/input\", O_RDONLY) = 3\n",
            "1 <... open resumed>) = 4\n",
        ));
        assert_eq!(left.events[1].pid, "2");
        assert_eq!(
            (
                left.events[2].evidence.line,
                left.events[2].evidence.end_line
            ),
            (2, 4)
        );
        let right =
            super::field_regressions::run("9 open(\"/input\", O_RDONLY) = -1 ENOENT (Missing)\n");
        let result = report(&left, &right);
        assert_eq!(
            result.exit_code, 3,
            "old records must stay raw/provenance-unverified"
        );
        let change = &result.changes[0];
        let mapping = change.evidence_mapping.as_ref().unwrap();
        let refs = &mapping.left.outcomes[0].events;
        assert_eq!(
            refs.iter().map(|event| event.index).collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!((refs[0].evidence.line, refs[0].evidence.end_line), (3, 3));
        assert_eq!((refs[1].evidence.line, refs[1].evidence.end_line), (2, 4));
        assert_normalized_mapping(
            &mapping.left,
            &change.left_evidence,
            &left,
            mapping.observation.as_ref().unwrap(),
        );
    }

    #[test]
    fn occurrence_counts_and_task_spelling_do_not_become_cross_run_identity() {
        let left = trace_files("/input", &[("success", 2, 2)]);
        let mut right = trace_files(
            "/input",
            &[("success", 2, 2), ("success", 3, 3), ("success", 4, 4)],
        );
        for event in &mut right.events {
            event.pid = "42".into();
        }
        right.metadata.target_provenance.as_mut().unwrap().root.pid = "42".into();
        validate_comparison_run(&right).unwrap();
        let result = report(&left, &right);
        assert_eq!(
            (result.exit_code, result.observed_differences),
            (0, Some(false))
        );
        assert!(result.changes.is_empty());
    }

    #[test]
    fn metadata_changes_have_field_sources_and_target_changes_use_only_saved_provenance() {
        let left = run_fixture();
        let mut right = left.clone();
        right.metadata.cwd = "/elsewhere".into();
        right.metadata.command.push("argument".into());
        right
            .metadata
            .environment
            .as_mut()
            .unwrap()
            .insert("FLAG".into(), Some("set".into()));
        right
            .metadata
            .warnings
            .push("saved capture limitation".into());
        right.metadata.completeness = Some("partial".into());
        set_exit(&mut right, 7);
        let result = report(&left, &right);
        assert_eq!(result.exit_code, 3);
        for change in &result.changes {
            let mapping = change.evidence_mapping.as_ref().unwrap();
            assert!(mapping.observation.is_none());
            if change.kind == ChangeKind::ExitStatus {
                for (side, run, samples) in [
                    (&mapping.left, &left, &change.left_evidence),
                    (&mapping.right, &right, &change.right_evidence),
                ] {
                    let saved = &run.metadata.target_provenance.as_ref().unwrap().target_exit;
                    assert_eq!(side.source, EvidenceSource::TargetProvenance);
                    assert_eq!(
                        (
                            side.event_count,
                            side.sample_count,
                            side.omitted_sample_count
                        ),
                        (Some(1), 1, Some(0))
                    );
                    assert_eq!(side.sample_event_indices, [saved.index]);
                    assert_eq!(side.outcomes[0].outcome, target_exit(&run.metadata));
                    assert_eq!(
                        side.outcomes[0].events.as_slice(),
                        std::slice::from_ref(saved)
                    );
                    assert_eq!(samples.as_slice(), std::slice::from_ref(&saved.evidence));
                }
            } else {
                for side in [&mapping.left, &mapping.right] {
                    assert_eq!(side.source, EvidenceSource::Metadata);
                    assert!(
                        side.metadata_fields
                            .iter()
                            .all(|field| field.starts_with("metadata."))
                    );
                    assert!(!side.metadata_fields.is_empty());
                    assert_eq!(
                        (
                            side.event_count,
                            side.sample_count,
                            side.omitted_sample_count
                        ),
                        (None, 0, None)
                    );
                    assert!(side.outcomes.is_empty() && side.sample_event_indices.is_empty());
                }
                assert!(change.left_evidence.is_empty() && change.right_evidence.is_empty());
            }
        }
        assert_eq!(result.changes.len(), 5);
        let mut old = super::field_regressions::run("1 +++ exited with 0 +++\n");
        let mut old_changed = old.clone();
        old_changed.metadata.exit_code = Some(7);
        old_changed.metadata.collector_exit_code = Some(7);
        old_changed.metadata.target_exit_raw = Some("+++ exited with 7 +++".into());
        // Current metadata v3 intentionally treats raw completion spelling as opaque.
        // A present exit event does not authorize selecting a historical target.
        old.metadata.name = "old".into();
        let legacy = report(&old, &old_changed);
        let exit = legacy
            .changes
            .iter()
            .find(|change| change.kind == ChangeKind::ExitStatus)
            .unwrap();
        let mapping = exit.evidence_mapping.as_ref().unwrap();
        assert_eq!(mapping.left.source, EvidenceSource::Metadata);
        assert_eq!(mapping.right.source, EvidenceSource::Metadata);
        assert!(mapping.left.outcomes.is_empty() && mapping.right.outcomes.is_empty());
        assert!(exit.left_evidence.is_empty() && exit.right_evidence.is_empty());
    }

    #[test]
    fn withheld_events_do_not_turn_saved_metadata_counts_or_provenance_into_refs() {
        let mut left = run_fixture();
        let mut right = left.clone();
        set_exit(&mut right, 7);
        left.events.clear();
        left.storage_integrity.status = "unfinalized".into();
        left.raw_integrity.as_mut().unwrap().status = "unverified".into();
        left.raw_integrity.as_mut().unwrap().manifest = None;
        assert!(left.metadata.parsed_events > 0 && left.metadata.target_provenance.is_some());
        let result = report(&left, &right);
        assert_eq!(
            (result.exit_code, result.observed_differences),
            (3, Some(true))
        );
        assert_eq!(
            result.reliability.left.provenance.as_deref(),
            Some("unverified")
        );
        for change in &result.changes {
            let side = &change.evidence_mapping.as_ref().unwrap().left;
            assert_eq!(side.source, EvidenceSource::Withheld);
            assert_eq!(
                (
                    side.event_count,
                    side.sample_count,
                    side.omitted_sample_count
                ),
                (None, 0, None)
            );
            assert!(side.outcomes.is_empty() && side.sample_event_indices.is_empty());
            assert!(change.left_evidence.is_empty());
        }
    }

    #[test]
    fn invalid_dtos_and_raw_damage_fail_before_any_ordinary_mapping() {
        let healthy = run_fixture();
        for fault in ["coordinates", "index", "schema", "raw", "outcome"] {
            let mut invalid = healthy.clone();
            match fault {
                "coordinates" => invalid.events[1].evidence.line = 0,
                "index" => {
                    invalid
                        .metadata
                        .target_provenance
                        .as_mut()
                        .unwrap()
                        .target_exit
                        .index = 1
                }
                "schema" => invalid.events[1].schema_version = 999,
                "raw" => invalid.raw_integrity.as_mut().unwrap().status = "unknown".into(),
                "outcome" => invalid.metadata.exit_code = Some(7),
                _ => unreachable!(),
            }
            let result = report(&invalid, &healthy);
            assert_eq!(
                (
                    result.schema_version,
                    result.exit_code,
                    result.observed_differences
                ),
                (6, 2, None),
                "{fault}"
            );
            assert_eq!(
                result.evidence_contract,
                Some("outcome_event_references_v1")
            );
            assert!(result.changes.is_empty());
            assert_eq!(result.errors.len(), 1);
        }
        for status in ["corrupt", "unavailable"] {
            let mut damaged = healthy.clone();
            damaged.raw_integrity.as_mut().unwrap().status = status.into();
            let result = report(&damaged, &healthy);
            assert_eq!(
                (
                    result.schema_version,
                    result.exit_code,
                    result.observed_differences
                ),
                (6, 2, None)
            );
            assert!(result.changes.is_empty());
            assert_eq!(result.reliability.left.storage, "verified");
            assert_eq!(result.reliability.left.capture, "complete");
            assert_eq!(result.reliability.left.raw.as_deref(), Some(status));
        }
    }

    #[test]
    fn full_mapping_is_explicit_and_never_promotes_old_or_mixed_evidence() {
        let old = super::field_regressions::run("1 openat(AT_FDCWD, \"/input\", O_RDONLY) = 3\n");
        let changed = super::field_regressions::run(
            "2 openat(AT_FDCWD, \"/input\", O_RDONLY) = -1 ENOENT (Missing)\n",
        );
        for format in [ReportFormat::Automatic, ReportFormat::TraceV5] {
            let result = evaluate_report(
                "old",
                "changed",
                LoadOutcome::Loaded(old.clone()),
                LoadOutcome::Loaded(changed.clone()),
                format,
            );
            assert_eq!(
                result.schema_version,
                if format == ReportFormat::Automatic {
                    4
                } else {
                    5
                }
            );
            assert!(result.evidence_contract.is_none());
            assert!(
                result
                    .changes
                    .iter()
                    .all(|change| change.evidence_mapping.is_none())
            );
        }
        let new = run_fixture();
        assert_eq!(
            (
                compare(&new, &new).schema_version,
                compare(&new, &new).exit_code
            ),
            (5, 0)
        );
        let equal = report(&new, &new);
        assert_eq!((equal.schema_version, equal.exit_code), (6, 0));
        let old_equal = report(&old, &old);
        assert_eq!(
            (
                old_equal.schema_version,
                old_equal.exit_code,
                old_equal.observed_differences
            ),
            (6, 3, Some(false))
        );
        assert_eq!(
            old_equal.reliability.left.raw.as_deref(),
            Some("unverified")
        );
        let mixed = report(&new, &old);
        assert_eq!((mixed.schema_version, mixed.exit_code), (6, 3));
        assert_eq!(
            mixed.reliability.left.provenance.as_deref(),
            Some("verified")
        );
        assert_eq!(
            mixed.reliability.right.provenance.as_deref(),
            Some("unverified")
        );
    }

    #[test]
    fn explicit_v6_error_reports_keep_the_contract_even_if_neither_side_loads() {
        for kind in [
            StorageLoadFailureKind::Corrupt,
            StorageLoadFailureKind::Unsupported,
            StorageLoadFailureKind::Unavailable,
            StorageLoadFailureKind::Busy,
        ] {
            let failure = || {
                LoadOutcome::Failed(StorageLoadFailure {
                    kind,
                    message: "saved input cannot load".into(),
                })
            };
            let result = evaluate_report(
                "left",
                "right",
                failure(),
                failure(),
                ReportFormat::OutcomeV6,
            );
            assert_eq!(
                (
                    result.schema_version,
                    result.exit_code,
                    result.observed_differences
                ),
                (6, 2, None)
            );
            assert_eq!(
                result.evidence_contract,
                Some("outcome_event_references_v1")
            );
            assert!(result.changes.is_empty());
            assert_eq!(result.errors.len(), 2);
            assert_eq!(result.reliability.left.raw.as_deref(), Some("unverified"));
            let earlier =
                evaluate_report("left", "right", failure(), failure(), ReportFormat::TraceV5);
            assert_eq!((earlier.schema_version, earlier.exit_code), (5, 2));
            assert!(earlier.evidence_contract.is_none());
        }
    }

    /// The raw order deliberately disagrees with tuple order. Shared spelling
    /// comes from separately owned strings, and repeated coordinates still name
    /// distinct saved occurrences. This is a DTO contract fixture, not raw proof.
    fn merge_boundary_runs() -> (Run, Run) {
        let mut left = super::field_regressions::run(concat!(
            "1 execve(\"/z-shared\", [], 0) = 0\n",
            "1 execve(\"/a-left\", [], 0) = -1 ENOENT (missing)\n",
            "1 openat(AT_FDCWD, \"/same\", O_RDONLY) = 3\n",
            "1 access(\"/same\", R_OK) = 0\n",
            "1 open(\"/same\", O_RDONLY) = 4\n",
            "1 openat(AT_FDCWD, \"/common\", O_RDONLY) = 5\n",
            "1 openat(AT_FDCWD, \"/common\", O_RDONLY) = 6\n",
            "1 openat(AT_FDCWD, \"/z-left\", O_RDONLY) = -1 ENOENT (missing)\n",
            "1 openat(AT_FDCWD, \"/mixed\", O_RDONLY) = -1 EACCES (denied)\n",
            "1 openat(AT_FDCWD, \"/mixed\", O_RDONLY) = 7\n",
            "1 openat(AT_FDCWD, \"/mixed\", O_RDONLY) = -1 EACCES (denied)\n",
        ));
        let right = super::field_regressions::run(concat!(
            "1 execve(\"/z-shared\", [], 0) = -1 ENOENT (missing)\n",
            "1 execve(\"/b-right\", [], 0) = -1 ENOENT (missing)\n",
            "1 openat(AT_FDCWD, \"/same\", O_RDONLY) = -1 EACCES (denied)\n",
            "1 access(\"/same\", R_OK) = -1 ENOENT (missing)\n",
            "1 open(\"/same\", O_RDONLY) = -1 ENOENT (missing)\n",
            "1 openat(AT_FDCWD, \"/common\", O_RDONLY) = 3\n",
            "1 openat(AT_FDCWD, \"/a-right\", O_RDONLY) = 4\n",
            "1 openat(AT_FDCWD, \"/mixed\", O_RDONLY) = -1 EACCES (denied)\n",
            "1 openat(AT_FDCWD, \"/mixed\", O_RDONLY) = -1 ENOENT (missing)\n",
            "1 openat(AT_FDCWD, \"/z-right\", O_RDONLY) = 5\n",
        ));
        left.events[10].evidence = left.events[8].evidence.clone();
        validate_comparison_run(&left).unwrap();
        validate_comparison_run(&right).unwrap();
        (left, right)
    }

    fn merge_boundary_formats() -> [(ReportFormat, &'static str, u32); 3] {
        [
            (ReportFormat::Automatic, "v4", 4),
            (ReportFormat::TraceV5, "v5", 5),
            (ReportFormat::OutcomeV6, "v6", 6),
        ]
    }

    /// Restore the recorded production ordinal for this assertion. The returned
    /// changes are intentionally ranked, so their final order is a different fact.
    fn assert_merge_production_subjects(result: &Report, expected: &[&str]) {
        let mut produced = result
            .changes
            .iter()
            .map(|change| (change.rank_key.comparison_order(), change.subject.as_str()))
            .collect::<Vec<_>>();
        produced.sort_by_key(|&(order, _)| order);
        assert_eq!(
            produced,
            expected.iter().copied().enumerate().collect::<Vec<_>>()
        );
    }

    #[test]
    fn merge_boundaries_keep_tuple_production_order_and_saved_occurrences() {
        let (left, right) = merge_boundary_runs();
        let expected = [
            "/a-left (execve)",
            "/b-right (execve)",
            "/z-shared (execve)",
            "/same (access)",
            "/same (open)",
            "/a-right (openat)",
            "/mixed (openat)",
            "/same (openat)",
            "/z-left (openat)",
            "/z-right (openat)",
        ];
        for (l, r) in [(&left, &right), (&right, &left)] {
            for (format, _, schema) in merge_boundary_formats() {
                let result = compare_version(l, r, format);
                assert_eq!(result.schema_version, schema);
                assert_eq!(result.exit_code, if schema == 4 { 1 } else { 3 });
                assert_eq!(result.observed_differences, Some(true));
                assert!(result.errors.is_empty());
                assert_merge_production_subjects(&result, &expected);
                assert!(
                    !result
                        .changes
                        .iter()
                        .any(|c| c.subject == "/common (openat)")
                );
                for change in &result.changes {
                    if schema == 6 {
                        let mapping = change.evidence_mapping.as_ref().unwrap();
                        let key = mapping.observation.as_ref().unwrap();
                        assert_normalized_mapping(&mapping.left, &change.left_evidence, l, key);
                        assert_normalized_mapping(&mapping.right, &change.right_evidence, r, key);
                    } else {
                        assert!(change.evidence_mapping.is_none());
                    }
                }
            }
        }
        let result = compare_version(&left, &right, ReportFormat::OutcomeV6);
        assert_eq!(
            result
                .changes
                .iter()
                .map(|c| c.subject.as_str())
                .collect::<Vec<_>>(),
            [
                "/z-shared (execve)",
                "/same (access)",
                "/same (open)",
                "/same (openat)",
                "/b-right (execve)",
                "/a-right (openat)",
                "/z-right (openat)",
                "/a-left (execve)",
                "/mixed (openat)",
                "/z-left (openat)",
            ]
        );
        let mixed = result
            .changes
            .iter()
            .find(|c| c.subject == "/mixed (openat)")
            .unwrap();
        let mapping = mixed.evidence_mapping.as_ref().unwrap();
        assert_eq!(mapping.left.sample_event_indices, [9, 8, 10]);
        assert_eq!(mapping.right.sample_event_indices, [8, 7]);
        assert_eq!(mapping.left.event_count, Some(3));
        assert_eq!(mapping.right.event_count, Some(2));
        assert_eq!(mapping.left.omitted_sample_count, Some(0));
        assert_eq!(mapping.right.omitted_sample_count, Some(0));
        assert_eq!(mixed.left_evidence[1], mixed.left_evidence[2]);
        assert_eq!(
            mapping.left.outcomes[0]
                .events
                .iter()
                .map(|r| r.index)
                .collect::<Vec<_>>(),
            [8, 10]
        );
        assert_eq!(mapping.left.outcomes[0].events[0].evidence.line, 9);
        assert_eq!(mapping.left.outcomes[0].events[1].evidence.line, 9);

        // Aligned operation sets can hide a path-only merge defect. These heads
        // deliberately share path spelling across different operation/family
        // tuples; each remains a distinct one-sided observation.
        let cross_left = super::field_regressions::run(concat!(
            "1 execve(\"/cross-kind\", [], 0) = -1 ENOENT (missing)\n",
            "1 access(\"/cross-op\", R_OK) = 0\n",
        ));
        let cross_right = super::field_regressions::run(concat!(
            "1 access(\"/cross-kind\", R_OK) = 0\n",
            "1 open(\"/cross-op\", O_RDONLY) = 3\n",
        ));
        validate_comparison_run(&cross_left).unwrap();
        validate_comparison_run(&cross_right).unwrap();
        for (l, r) in [(&cross_left, &cross_right), (&cross_right, &cross_left)] {
            for (format, _, schema) in merge_boundary_formats() {
                let result = compare_version(l, r, format);
                assert_eq!(result.exit_code, if schema == 4 { 1 } else { 3 });
                assert_merge_production_subjects(
                    &result,
                    &[
                        "/cross-kind (execve)",
                        "/cross-kind (access)",
                        "/cross-op (access)",
                        "/cross-op (open)",
                    ],
                );
                if schema == 6 {
                    for change in &result.changes {
                        let mapping = change.evidence_mapping.as_ref().unwrap();
                        let key = mapping.observation.as_ref().unwrap();
                        assert_normalized_mapping(&mapping.left, &change.left_evidence, l, key);
                        assert_normalized_mapping(&mapping.right, &change.right_evidence, r, key);
                    }
                }
            }
        }
    }

    #[test]
    fn merge_boundaries_keep_empty_sides_and_exhausted_tails() {
        let (left, right) = merge_boundary_runs();
        let empty = super::field_regressions::run("");
        let left_subjects = [
            "/a-left (execve)",
            "/z-shared (execve)",
            "/same (access)",
            "/same (open)",
            "/common (openat)",
            "/mixed (openat)",
            "/same (openat)",
            "/z-left (openat)",
        ];
        let right_subjects = [
            "/b-right (execve)",
            "/z-shared (execve)",
            "/same (access)",
            "/same (open)",
            "/a-right (openat)",
            "/common (openat)",
            "/mixed (openat)",
            "/same (openat)",
            "/z-right (openat)",
        ];
        for (l, r, expected) in [
            (&left, &empty, left_subjects.as_slice()),
            (&empty, &left, left_subjects.as_slice()),
            (&right, &empty, right_subjects.as_slice()),
            (&empty, &right, right_subjects.as_slice()),
            (&empty, &empty, &[]),
        ] {
            for (format, _, schema) in merge_boundary_formats() {
                let result = compare_version(l, r, format);
                assert_merge_production_subjects(&result, expected);
                assert_eq!(result.observed_differences, Some(!expected.is_empty()));
                assert_eq!(
                    result.exit_code,
                    if schema == 4 {
                        i32::from(!expected.is_empty())
                    } else {
                        3
                    }
                );
                if schema == 6 {
                    for change in &result.changes {
                        let mapping = change.evidence_mapping.as_ref().unwrap();
                        let key = mapping.observation.as_ref().unwrap();
                        assert_normalized_mapping(&mapping.left, &change.left_evidence, l, key);
                        assert_normalized_mapping(&mapping.right, &change.right_evidence, r, key);
                    }
                }
            }
        }
    }

    #[test]
    fn merge_boundaries_validate_nonempty_dtos_before_v6_withholding() {
        let (mut left, right) = merge_boundary_runs();
        left.storage_integrity.status = "unfinalized".into();
        validate_comparison_run(&left).unwrap();
        let original = serde_json::to_value(&left).unwrap();
        let result = compare_version(&left, &right, ReportFormat::OutcomeV6);
        assert_eq!(result.exit_code, 3);
        assert_eq!(result.observed_differences, Some(true));
        assert_eq!(result.reliability.left.capture, "unknown");
        assert_merge_production_subjects(
            &result,
            &[
                "/b-right (execve)",
                "/z-shared (execve)",
                "/same (access)",
                "/same (open)",
                "/a-right (openat)",
                "/common (openat)",
                "/mixed (openat)",
                "/same (openat)",
                "/z-right (openat)",
            ],
        );
        for change in &result.changes {
            let mapping = change.evidence_mapping.as_ref().unwrap();
            assert_eq!(mapping.left.source, EvidenceSource::Withheld);
            assert_eq!(mapping.left.event_count, None);
            assert_eq!(mapping.left.omitted_sample_count, None);
            assert_eq!(mapping.left.sample_count, 0);
            assert!(mapping.left.sample_event_indices.is_empty());
            assert!(mapping.left.outcomes.is_empty());
            assert!(change.left.is_empty());
            assert!(change.left_evidence.is_empty());
            assert_normalized_mapping(
                &mapping.right,
                &change.right_evidence,
                &right,
                mapping.observation.as_ref().unwrap(),
            );
        }
        assert_eq!(serde_json::to_value(&left).unwrap(), original);
        left.events[8].path = None;
        for (format, _, _) in merge_boundary_formats() {
            let failed = compare_version(&left, &right, format);
            assert_eq!(failed.exit_code, 2);
            assert_eq!(failed.observed_differences, None);
            assert!(failed.changes.is_empty());
            assert_eq!(failed.errors.len(), 1);
            assert_eq!(failed.reliability.left.storage, "corrupt");
        }
    }

    #[test]
    fn merge_boundary_versioned_wire_snapshots() {
        // Root freezes these exact bodies against the accepted algorithm before
        // replacing traversal, then independently compares the candidate output.
        // This test alone asserts facts, not that an external snapshot was kept.
        let (left, right) = merge_boundary_runs();
        let empty = super::field_regressions::run("");
        let mut withheld = left.clone();
        withheld.storage_integrity.status = "unfinalized".into();
        let mut corrupt = withheld.clone();
        corrupt.events[8].path = None;
        for (case, l, r) in [
            ("interleaved", &left, &right),
            ("interleaved-swapped", &right, &left),
            ("empty-left", &empty, &right),
            ("empty-right", &left, &empty),
            ("both-empty", &empty, &empty),
            ("withheld-left", &withheld, &right),
            ("corrupt-withheld-left", &corrupt, &right),
        ] {
            for (format, spelling, schema) in merge_boundary_formats() {
                let result = compare_version(l, r, format);
                let expected_code = match case {
                    "corrupt-withheld-left" => 2,
                    "withheld-left" => 3,
                    "both-empty" if schema == 4 => 0,
                    _ if schema == 4 => 1,
                    _ => 3,
                };
                assert_eq!(result.schema_version, schema);
                assert_eq!(result.exit_code, expected_code);
                let body = crate::report::json(&result).unwrap();
                let wire: serde_json::Value = serde_json::from_str(&body).unwrap();
                assert_eq!(wire["schema_version"], schema);
                assert_eq!(wire["exit_code"], expected_code);
                let snapshot = serde_json::json!({
                    "case": case,
                    "format": spelling,
                    "exit_code": result.exit_code,
                    "json_body": body,
                    "text_body": crate::report::text(&result),
                });
                println!(
                    "RUNDELTA_AGGREGATION_SNAPSHOT_V1 {}",
                    serde_json::to_string(&snapshot).unwrap()
                );
            }
        }
    }

    /// Make valid metadata3 DTOs whose saved order can differ from outcome order.
    /// The trace template supplies shapes/counts/coordinates. Replacing normalized
    /// outcome text here exercises DTO contracts, not raw capture authenticity.
    fn outcome_bucket_fixture(outcomes: &[&str]) -> Run {
        let mut run = super::field_regressions::run(
            &"1 openat(AT_FDCWD, \"/dense\", O_RDONLY) = 3\n".repeat(outcomes.len()),
        );
        for (event, &outcome) in run.events.iter_mut().zip(outcomes) {
            event.outcome = outcome.into();
        }
        validate_comparison_run(&run).unwrap();
        run
    }

    fn outcome_bucket_dense_pair() -> (Run, Run) {
        let mut left =
            outcome_bucket_fixture(&["success", "EROFS", "success", "ENOENT", "EACCES", "EROFS"]);
        let mut right = outcome_bucket_fixture(&["EROFS", "EPERM", "EROFS"]);
        // Full maps follow saved indices, including a nonmonotonic coordinate pair
        // and different saved entries with identical coordinates. Neither is a PID
        // or content identity claim, and schema3 does not reorder these saved DTOs.
        left.events[0].evidence.line = 6;
        left.events[0].evidence.end_line = 6;
        left.events[5].evidence = left.events[1].evidence.clone();
        right.events[2].evidence = right.events[0].evidence.clone();
        validate_comparison_run(&left).unwrap();
        validate_comparison_run(&right).unwrap();
        (left, right)
    }

    fn outcome_bucket_tuple_pair() -> (Run, Run) {
        let left = super::field_regressions::run(concat!(
            "1 execve(\"/same\", [], 0) = -1 ENOENT (missing)\n",
            "1 access(\"/same\", R_OK) = 0\n",
            "1 openat(AT_FDCWD, \"/same\", O_RDONLY) = 3\n",
            "1 openat(AT_FDCWD, \"/same\", O_RDONLY) = -1 EROFS (read-only)\n",
        ));
        let right = super::field_regressions::run(concat!(
            "1 execve(\"/same\", [], 0) = 0\n",
            "1 open(\"/same\", O_RDONLY) = -1 EACCES (denied)\n",
            "1 openat(AT_FDCWD, \"/same\", O_RDONLY) = -1 EPERM (denied)\n",
            "1 openat(AT_FDCWD, \"/same\", O_RDONLY) = 4\n",
        ));
        validate_comparison_run(&left).unwrap();
        validate_comparison_run(&right).unwrap();
        (left, right)
    }

    fn outcome_bucket_rows<'events>(
        outcomes: &OutcomeEvidence<'events>,
    ) -> Vec<(&'events str, Vec<usize>)> {
        outcomes
            .iter()
            .map(|(outcome, indices)| (*outcome, indices.clone()))
            .collect()
    }

    fn outcome_bucket_reference_rows(mapping: &SideEvidenceMapping) -> Vec<(&str, Vec<usize>)> {
        mapping
            .outcomes
            .iter()
            .map(|outcome| {
                (
                    outcome.outcome.as_str(),
                    outcome.events.iter().map(|event| event.index).collect(),
                )
            })
            .collect()
    }

    fn outcome_bucket_formats() -> [(ReportFormat, &'static str, u32); 3] {
        [
            (ReportFormat::Automatic, "v4", 4),
            (ReportFormat::TraceV5, "v5", 5),
            (ReportFormat::OutcomeV6, "v6", 6),
        ]
    }

    #[test]
    fn observation_change_empty_and_same_outcome_sets_have_no_semantic_change() {
        let empty = outcome_bucket_fixture(&[]);
        let repeated = outcome_bucket_fixture(&["success", "success"]);
        let single = outcome_bucket_fixture(&["success"]);
        let empty_buckets = aggregate(&empty, ReportFormat::OutcomeV6);
        let repeated_buckets = aggregate(&repeated, ReportFormat::OutcomeV6);
        let single_buckets = aggregate(&single, ReportFormat::OutcomeV6);
        let absent = OutcomeEvidence::new();
        assert!(empty_buckets.is_empty());
        assert!(
            observation_change(
                &empty,
                &empty,
                ("file", "openat", "/dense"),
                &absent,
                &absent,
                true
            )
            .is_none()
        );
        // Duplicate count describes occurrences in one Run, not cross-run identity.
        assert!(
            observation_change(
                &repeated,
                &single,
                ("file", "openat", "/dense"),
                repeated_buckets.get(&("file", "openat", "/dense")).unwrap(),
                single_buckets.get(&("file", "openat", "/dense")).unwrap(),
                true
            )
            .is_none()
        );
    }

    #[test]
    fn observation_change_materializes_complete_occurrences_before_buckets_drop() {
        let (left, right) = outcome_bucket_dense_pair();
        let change = {
            let a = aggregate(&left, ReportFormat::OutcomeV6);
            let b = aggregate(&right, ReportFormat::OutcomeV6);
            observation_change(
                &left,
                &right,
                ("file", "openat", "/dense"),
                a.get(&("file", "openat", "/dense")).unwrap(),
                b.get(&("file", "openat", "/dense")).unwrap(),
                true,
            )
            .unwrap()
        };
        // All owned change facts remain usable after both borrowed-string/index
        // containers have dropped; equal coordinates must retain distinct indices.
        assert_eq!(change.kind, ChangeKind::FileAccessOutcome);
        assert_eq!(change.category, "File access outcome changes");
        assert_eq!(change.left, ["EACCES", "ENOENT", "EROFS", "success"]);
        assert_eq!(change.right, ["EPERM", "EROFS"]);
        let mapping = change.evidence_mapping.as_ref().unwrap();
        assert_eq!(
            outcome_bucket_reference_rows(&mapping.left),
            [
                ("EACCES", vec![4]),
                ("ENOENT", vec![3]),
                ("EROFS", vec![1, 5]),
                ("success", vec![0, 2]),
            ]
        );
        assert_eq!(
            outcome_bucket_reference_rows(&mapping.right),
            [("EPERM", vec![1]), ("EROFS", vec![0, 2]),]
        );
        assert_eq!(
            (mapping.left.event_count, mapping.right.event_count),
            (Some(6), Some(3))
        );
        assert_eq!(mapping.left.sample_event_indices, [4, 3, 0]);
        assert_eq!(mapping.right.sample_event_indices, [1, 0, 2]);
        assert_eq!(
            (
                mapping.left.omitted_sample_count,
                mapping.right.omitted_sample_count
            ),
            (Some(3), Some(0))
        );
        assert_eq!(
            mapping.left.outcomes[2].events[0].evidence,
            mapping.left.outcomes[2].events[1].evidence
        );
        assert_eq!(
            mapping.right.outcomes[1].events[0].evidence,
            mapping.right.outcomes[1].events[1].evidence
        );
        for (&index, evidence) in mapping
            .left
            .sample_event_indices
            .iter()
            .zip(&change.left_evidence)
        {
            assert_eq!(evidence, &left.events[index].evidence);
        }
        for (&index, evidence) in mapping
            .right
            .sample_event_indices
            .iter()
            .zip(&change.right_evidence)
        {
            assert_eq!(evidence, &right.events[index].evidence);
        }
        let absent = OutcomeEvidence::new();
        let a = aggregate(&left, ReportFormat::Automatic);
        let v4 = observation_change(
            &left,
            &right,
            ("file", "openat", "/dense"),
            a.get(&("file", "openat", "/dense")).unwrap(),
            &absent,
            false,
        )
        .unwrap();
        assert_eq!(v4.kind, ChangeKind::RemovedFileAccess);
        assert!(v4.evidence_mapping.is_none());
    }

    #[test]
    fn outcome_bucket_prefixes_preserve_empty_single_multiple_and_repeated_indices() {
        let empty = OutcomeEvidence::new();
        assert!(empty.is_empty());
        assert!(empty.keys().next().is_none());
        assert!(empty.values().next().is_none());
        assert!(empty.iter().next().is_none());

        let stream = ["success", "EROFS", "success", "ENOENT", "EACCES", "EROFS"];
        // Handwritten prefixes freeze insertion at the beginning/middle/end and
        // returning to the first outcome after a second distinct outcome appeared.
        let expected: &[&[(&str, &[usize])]] = &[
            &[],
            &[("success", &[0])],
            &[("EROFS", &[1]), ("success", &[0])],
            &[("EROFS", &[1]), ("success", &[0, 2])],
            &[("ENOENT", &[3]), ("EROFS", &[1]), ("success", &[0, 2])],
            &[
                ("EACCES", &[4]),
                ("ENOENT", &[3]),
                ("EROFS", &[1]),
                ("success", &[0, 2]),
            ],
            &[
                ("EACCES", &[4]),
                ("ENOENT", &[3]),
                ("EROFS", &[1, 5]),
                ("success", &[0, 2]),
            ],
        ];
        for (prefix, wanted) in expected.iter().enumerate() {
            let run = outcome_bucket_fixture(&stream[..prefix]);
            for (format, _, _) in outcome_bucket_formats() {
                let grouped = aggregate(&run, format);
                if prefix == 0 {
                    assert!(grouped.is_empty());
                    continue;
                }
                assert_eq!(
                    grouped.keys().copied().collect::<Vec<_>>(),
                    [("file", "openat", "/dense")]
                );
                let bucket = grouped.get(&("file", "openat", "/dense")).unwrap();
                assert_eq!(
                    outcome_bucket_rows(bucket),
                    wanted
                        .iter()
                        .map(|&(outcome, indices)| (outcome, indices.to_vec()))
                        .collect::<Vec<_>>(),
                    "prefix {prefix}",
                );
                assert_eq!(bucket.values().map(Vec::len).sum::<usize>(), prefix);
            }
        }
    }

    #[test]
    fn outcome_bucket_dense_maps_keep_spelling_order_samples_and_all_saved_coordinates() {
        let (left, right) = outcome_bucket_dense_pair();
        let result = compare_version(&left, &right, ReportFormat::OutcomeV6);
        assert_eq!((result.schema_version, result.exit_code), (6, 3));
        assert_eq!(result.observed_differences, Some(true));
        assert_eq!(result.changes.len(), 1);
        let change = &result.changes[0];
        assert_eq!(change.left, ["EACCES", "ENOENT", "EROFS", "success"]);
        assert_eq!(change.right, ["EPERM", "EROFS"]);
        let mapping = change.evidence_mapping.as_ref().unwrap();
        let key = mapping.observation.as_ref().unwrap();
        assert_eq!(
            (&*key.kind, &*key.operation, &*key.path),
            ("file", "openat", "/dense")
        );
        assert_eq!(
            outcome_bucket_reference_rows(&mapping.left),
            [
                ("EACCES", vec![4]),
                ("ENOENT", vec![3]),
                ("EROFS", vec![1, 5]),
                ("success", vec![0, 2]),
            ],
        );
        assert_eq!(
            outcome_bucket_reference_rows(&mapping.right),
            [("EPERM", vec![1]), ("EROFS", vec![0, 2])],
        );
        assert_eq!(mapping.left.sample_event_indices, [4, 3, 0]);
        assert_eq!(mapping.right.sample_event_indices, [1, 0, 2]);
        assert_eq!(
            (
                mapping.left.event_count,
                mapping.left.sample_count,
                mapping.left.omitted_sample_count
            ),
            (Some(6), 3, Some(3))
        );
        assert_eq!(
            (
                mapping.right.event_count,
                mapping.right.sample_count,
                mapping.right.omitted_sample_count
            ),
            (Some(3), 3, Some(0))
        );
        assert_eq!(change.right_evidence[1], change.right_evidence[2]);
        let repeated = &mapping.left.outcomes[2].events;
        assert_eq!(repeated[0].evidence, repeated[1].evidence);
        let success = &mapping.left.outcomes[3].events;
        assert_eq!((success[0].index, success[0].evidence.line), (0, 6));
        assert_eq!((success[1].index, success[1].evidence.line), (2, 3));
        assert_normalized_mapping(&mapping.left, &change.left_evidence, &left, key);
        assert_normalized_mapping(&mapping.right, &change.right_evidence, &right, key);

        // The fallback's established coordinate deduplication differs from the
        // common-outcome pass above. Retain both policies when changing the buckets.
        let mut fallback = outcome_bucket_fixture(&["ENOENT", "ENOENT", "ENOENT"]);
        fallback.events[1].evidence = fallback.events[0].evidence.clone();
        validate_comparison_run(&fallback).unwrap();
        let grouped = aggregate(&fallback, ReportFormat::OutcomeV6);
        let bucket = grouped.get(&("file", "openat", "/dense")).unwrap();
        assert_eq!(
            sample_event_indices(&fallback, bucket, &OutcomeEvidence::new()),
            [0, 2]
        );
    }

    #[test]
    fn outcome_bucket_equal_sets_ignore_insertion_order_and_duplicate_counts() {
        let (left, _) = outcome_bucket_dense_pair();
        let right = outcome_bucket_fixture(&["EROFS", "EACCES", "success", "ENOENT"]);
        for (format, _, schema) in outcome_bucket_formats() {
            let result = compare_version(&left, &right, format);
            assert_eq!(result.schema_version, schema);
            assert_eq!(result.exit_code, if schema == 4 { 0 } else { 3 });
            assert_eq!(result.observed_differences, Some(false));
            assert!(result.changes.is_empty());
            assert!(result.errors.is_empty());
        }
    }

    #[test]
    fn outcome_bucket_identity_keeps_same_path_across_kinds_and_operations() {
        let (left, right) = outcome_bucket_tuple_pair();
        let a = aggregate(&left, ReportFormat::OutcomeV6);
        let b = aggregate(&right, ReportFormat::OutcomeV6);
        assert_eq!(
            a.keys().copied().collect::<Vec<_>>(),
            [
                ("exec", "execve", "/same"),
                ("file", "access", "/same"),
                ("file", "openat", "/same")
            ],
        );
        assert_eq!(
            b.keys().copied().collect::<Vec<_>>(),
            [
                ("exec", "execve", "/same"),
                ("file", "open", "/same"),
                ("file", "openat", "/same")
            ],
        );
        assert_eq!(
            outcome_bucket_rows(a.get(&("file", "openat", "/same")).unwrap()),
            [("EROFS", vec![3]), ("success", vec![2])],
        );
        assert_eq!(
            outcome_bucket_rows(b.get(&("file", "openat", "/same")).unwrap()),
            [("EPERM", vec![2]), ("success", vec![3])],
        );
        for (l, r) in [(&left, &right), (&right, &left)] {
            for (format, _, schema) in outcome_bucket_formats() {
                let result = compare_version(l, r, format);
                assert_eq!(result.exit_code, if schema == 4 { 1 } else { 3 });
                assert_merge_production_subjects(
                    &result,
                    &[
                        "/same (execve)",
                        "/same (access)",
                        "/same (open)",
                        "/same (openat)",
                    ],
                );
                if schema == 6 {
                    for change in &result.changes {
                        let mapping = change.evidence_mapping.as_ref().unwrap();
                        let key = mapping.observation.as_ref().unwrap();
                        assert_normalized_mapping(&mapping.left, &change.left_evidence, l, key);
                        assert_normalized_mapping(&mapping.right, &change.right_evidence, r, key);
                    }
                }
            }
        }
    }

    #[test]
    fn outcome_bucket_dense_admission_retains_unverified_and_validates_before_withholding() {
        let (left, right) = outcome_bucket_dense_pair();
        for schema in [2, 3] {
            let mut unverified = left.clone();
            unverified.metadata.schema_version = schema;
            unverified.storage_integrity.status = "unverified".into();
            unverified.storage_integrity.detail = "synthetic ordinary unverified evidence".into();
            validate_comparison_run(&unverified).unwrap();
            for (format, _, report_schema) in outcome_bucket_formats() {
                let result = compare_version(&unverified, &right, format);
                assert_eq!(result.exit_code, 3);
                assert_eq!(result.reliability.left.storage, "unverified");
                assert_eq!(result.observed_differences, Some(true));
                assert_eq!(
                    result.changes[0].left,
                    ["EACCES", "ENOENT", "EROFS", "success"]
                );
                if report_schema == 6 {
                    let change = &result.changes[0];
                    let mapping = change.evidence_mapping.as_ref().unwrap();
                    let key = mapping.observation.as_ref().unwrap();
                    assert_normalized_mapping(
                        &mapping.left,
                        &change.left_evidence,
                        &unverified,
                        key,
                    );
                    assert_normalized_mapping(&mapping.right, &change.right_evidence, &right, key);
                }
            }
        }

        let mut withheld = left.clone();
        withheld.storage_integrity.status = "unfinalized".into();
        withheld.storage_integrity.detail = "synthetic legal nonempty metadata3 DTO".into();
        validate_comparison_run(&withheld).unwrap();
        assert!(aggregate(&withheld, ReportFormat::OutcomeV6).is_empty());
        assert_eq!(withheld.events.len(), 6);
        for (format, _, schema) in outcome_bucket_formats() {
            let result = compare_version(&withheld, &right, format);
            assert_eq!(result.exit_code, 3);
            assert_eq!(result.observed_differences, Some(true));
            let change = &result.changes[0];
            if schema == 6 {
                assert!(change.left.is_empty());
                assert!(change.left_evidence.is_empty());
                let mapping = change.evidence_mapping.as_ref().unwrap();
                assert_eq!(mapping.left.source, EvidenceSource::Withheld);
                assert_eq!(mapping.left.event_count, None);
                assert_eq!(mapping.left.sample_count, 0);
                assert_eq!(mapping.left.omitted_sample_count, None);
                assert!(mapping.left.sample_event_indices.is_empty());
                assert!(mapping.left.outcomes.is_empty());
                assert_normalized_mapping(
                    &mapping.right,
                    &change.right_evidence,
                    &right,
                    mapping.observation.as_ref().unwrap(),
                );
            } else {
                assert_eq!(change.left, ["EACCES", "ENOENT", "EROFS", "success"]);
                assert!(!change.left_evidence.is_empty());
            }
        }
        let reversed = compare_version(&right, &withheld, ReportFormat::OutcomeV6);
        let reverse_change = &reversed.changes[0];
        let reverse_mapping = reverse_change.evidence_mapping.as_ref().unwrap();
        assert_eq!(reverse_mapping.right.source, EvidenceSource::Withheld);
        assert_eq!(reverse_mapping.right.event_count, None);
        assert!(reverse_mapping.right.outcomes.is_empty());
        assert!(reverse_change.right_evidence.is_empty());

        let mut corrupt = withheld.clone();
        corrupt.events[2].outcome.clear();
        assert!(validate_comparison_run(&corrupt).is_err());
        for (l, r) in [(&corrupt, &right), (&right, &corrupt)] {
            for (format, _, schema) in outcome_bucket_formats() {
                let result = compare_version(l, r, format);
                assert_eq!((result.schema_version, result.exit_code), (schema, 2));
                assert_eq!(result.observed_differences, None);
                assert!(result.changes.is_empty());
                assert!(!result.errors.is_empty());
            }
        }
    }

    #[test]
    fn outcome_bucket_versioned_wire_snapshots() {
        // Freeze facts here; an independently bound old/candidate runner compares
        // all 39 exact text/JSON bodies and status rows emitted by actual tests.
        // This assertion suite alone does not prove that byte comparison occurred.
        let (left, right) = outcome_bucket_dense_pair();
        let single = outcome_bucket_fixture(&["success"]);
        let same_set = outcome_bucket_fixture(&["EROFS", "EACCES", "success", "ENOENT"]);
        let empty = outcome_bucket_fixture(&[]);
        let (tuple_left, tuple_right) = outcome_bucket_tuple_pair();
        let mut unverified_v2 = left.clone();
        unverified_v2.metadata.schema_version = 2;
        unverified_v2.storage_integrity.status = "unverified".into();
        unverified_v2.storage_integrity.detail = "synthetic ordinary unverified evidence".into();
        let mut unverified_v3 = unverified_v2.clone();
        unverified_v3.metadata.schema_version = 3;
        let mut withheld = left.clone();
        withheld.storage_integrity.status = "unfinalized".into();
        withheld.storage_integrity.detail = "synthetic legal nonempty metadata3 DTO".into();
        let mut empty_withheld = empty.clone();
        empty_withheld.storage_integrity.status = "unfinalized".into();
        empty_withheld.storage_integrity.detail = "synthetic empty withheld DTO".into();
        let mut corrupt = withheld.clone();
        corrupt.events[2].outcome.clear();
        for (case, l, r, v4_code, observed) in [
            ("single-left", &single, &empty, 1, Some(true)),
            ("multi-common-unique", &left, &right, 1, Some(true)),
            ("multi-swapped", &right, &left, 1, Some(true)),
            (
                "same-set-different-counts",
                &left,
                &same_set,
                0,
                Some(false),
            ),
            ("empty-left", &empty, &right, 1, Some(true)),
            ("both-empty", &empty, &empty, 0, Some(false)),
            ("tuple-identity", &tuple_left, &tuple_right, 1, Some(true)),
            ("unverified-v2", &unverified_v2, &right, 3, Some(true)),
            ("unverified-v3", &unverified_v3, &right, 3, Some(true)),
            ("withheld-left", &withheld, &right, 3, Some(true)),
            ("withheld-right", &right, &withheld, 3, Some(true)),
            (
                "withheld-empty-left",
                &empty_withheld,
                &right,
                3,
                Some(true),
            ),
            ("corrupt-withheld-left", &corrupt, &right, 2, None),
        ] {
            for (format, spelling, schema) in outcome_bucket_formats() {
                let result = compare_version(l, r, format);
                let expected_code = if v4_code == 2 || schema == 4 {
                    v4_code
                } else {
                    3
                };
                assert_eq!(
                    (result.schema_version, result.exit_code),
                    (schema, expected_code),
                    "{case}/{spelling}"
                );
                assert_eq!(result.observed_differences, observed, "{case}/{spelling}");
                let body = crate::report::json(&result).unwrap();
                let wire: serde_json::Value = serde_json::from_str(&body).unwrap();
                assert_eq!(wire["schema_version"], schema);
                assert_eq!(wire["exit_code"], expected_code);
                let snapshot = serde_json::json!({
                    "case": case,
                    "format": spelling,
                    "exit_code": result.exit_code,
                    "json_body": body,
                    "text_body": crate::report::text(&result),
                });
                println!(
                    "RUNDELTA_OUTCOME_SNAPSHOT_V1 {}",
                    serde_json::to_string(&snapshot).unwrap()
                );
            }
        }
    }

    #[test]
    fn outcome_bucket_private_states_move_indices_and_keep_exact_iterator_lengths() {
        fn exact<'map, 'events: 'map>(
            mut iter: impl Iterator<Item = (&'map &'events str, &'map Vec<usize>)>,
            expected: &[(&str, &[usize])],
        ) {
            for (position, &(key, indices)) in expected.iter().enumerate() {
                let remaining = expected.len() - position;
                assert_eq!(iter.size_hint(), (remaining, Some(remaining)));
                let (actual_key, actual_indices) = iter.next().unwrap();
                assert_eq!((*actual_key, actual_indices.as_slice()), (key, indices));
            }
            assert_eq!(iter.size_hint(), (0, Some(0)));
            assert!(iter.next().is_none());
            assert!(iter.next().is_none());
            assert_eq!(iter.size_hint(), (0, Some(0)));
        }

        let mut bucket = OutcomeEvidence::new();
        assert!(matches!(&bucket, OutcomeEvidence::Empty));
        assert!(!bucket.contains_key("success"));
        exact(bucket.iter(), &[]);

        // Deliberately retain spare capacity: a nonempty allocation has an actual
        // pointer, and a clone would normally discard this retained spare capacity.
        let (saved_pointer, saved_capacity) = {
            let indices = bucket.indices_mut("success");
            indices.reserve_exact(12);
            indices.extend([3, 7]);
            (indices.as_ptr(), indices.capacity())
        };
        assert!(saved_capacity >= 12);
        assert!(matches!(&bucket, OutcomeEvidence::Single("success", _)));
        assert!(bucket.contains_key("success"));
        assert!(!bucket.contains_key("EROFS"));
        exact(bucket.iter(), &[("success", &[3, 7])]);

        // The former tree's collect specializations use its exact cardinality.
        // Accurate size_hint alone does not promise the same Vec reservation;
        // freeze both collection paths to expose singleton presentation growth.
        let mut old_control = BTreeMap::new();
        old_control.insert("success", vec![3_usize, 7]);
        assert_eq!(
            (
                old_control
                    .keys()
                    .map(|s| (*s).to_owned())
                    .collect::<Vec<_>>()
                    .capacity(),
                old_control
                    .iter()
                    .map(|(key, indices)| ((*key).to_owned(), indices.len()))
                    .collect::<Vec<_>>()
                    .capacity(),
            ),
            (1, 1),
        );
        assert_eq!(
            (
                bucket
                    .keys()
                    .map(|s| (*s).to_owned())
                    .collect::<Vec<_>>()
                    .capacity(),
                bucket
                    .iter()
                    .map(|(key, indices)| ((*key).to_owned(), indices.len()))
                    .collect::<Vec<_>>()
                    .capacity(),
            ),
            (1, 1),
            "singleton collection capacity must retain the old exact reservation",
        );

        bucket.indices_mut("success").push(11);
        let OutcomeEvidence::Single("success", singleton) = &bucket else {
            panic!("equal outcome spelling must remain Single");
        };
        assert_eq!(singleton.as_ptr(), saved_pointer);
        assert_eq!(singleton.capacity(), saved_capacity);
        exact(bucket.iter(), &[("success", &[3, 7, 11])]);

        // Promotion owns the old vector; it must not copy, replace, or deduplicate
        // saved occurrences. The new outcome sorts before the original spelling.
        bucket.indices_mut("EROFS").push(17);
        let OutcomeEvidence::Multiple(outcomes) = &bucket else {
            panic!("a second distinct spelling must establish Multiple");
        };
        let original = outcomes.get("success").unwrap();
        assert_eq!(original.as_ptr(), saved_pointer);
        assert_eq!(original.capacity(), saved_capacity);
        assert_eq!(original, &[3, 7, 11]);
        assert!(bucket.contains_key("EROFS"));
        assert!(bucket.contains_key("success"));
        assert!(!bucket.contains_key("ENOENT"));
        exact(bucket.iter(), &[("EROFS", &[17]), ("success", &[3, 7, 11])]);

        bucket.indices_mut("success").push(29);
        bucket.indices_mut("ENOENT").push(23);
        assert!(matches!(&bucket, OutcomeEvidence::Multiple(_)));
        exact(
            bucket.iter(),
            &[
                ("ENOENT", &[23]),
                ("EROFS", &[17]),
                ("success", &[3, 7, 11, 29]),
            ],
        );
    }
}
