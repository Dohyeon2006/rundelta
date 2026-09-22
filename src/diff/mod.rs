//! Deterministic, I/O-free comparison of already-loaded runs.
//!
//! The result preserves the four-way contract: `0` is trusted equality, `1` is
//! a trusted observed difference, `2` is a load/tool error with no ordinary
//! comparison conclusion, and `3` is incomplete or unverified evidence. A
//! change is an investigation clue, never proof of causation or file identity.

use crate::model::{Evidence, LoadOutcome, Metadata, Run, StorageLoadFailureKind};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Presentation-neutral tier used to order complete semantic changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RankClass {
    /// Target outcome changed.
    ExitStatus,
    /// Capture state, completeness, or warnings changed.
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

/// One aggregated observed change and bounded evidence samples for each side.
///
/// Aggregation identity is kind, operation, and path. PID, scheduling order,
/// duplicate count, and timing are deliberately absent from cross-run identity.
#[derive(Debug)]
pub(crate) struct Change {
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
}

/// Complete comparison result before text or diff-v4 JSON presentation.
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
}
type OutcomeEvidence<'a> = BTreeMap<&'a str, Vec<&'a Evidence>>;
type Aggregate<'a> = BTreeMap<(&'a str, &'a str, &'a str), OutcomeEvidence<'a>>;

fn sample_evidence(outcomes: &OutcomeEvidence<'_>, other: &OutcomeEvidence<'_>) -> Vec<Evidence> {
    const LIMIT: usize = 3;
    let mut samples = Vec::new();
    for evidence in outcomes
        .iter()
        .filter(|(outcome, _)| !other.contains_key(*outcome))
        .filter_map(|(_, evidence)| evidence.first())
    {
        if samples.len() == LIMIT {
            return samples;
        }
        samples.push((**evidence).clone());
    }
    for evidence in outcomes
        .iter()
        .filter(|(outcome, _)| other.contains_key(*outcome))
        .flat_map(|(_, evidence)| evidence)
    {
        if samples.len() == LIMIT {
            return samples;
        }
        samples.push((**evidence).clone());
    }
    for evidence in outcomes.values().flatten() {
        if samples.len() == LIMIT {
            break;
        }
        if !samples.contains(*evidence) {
            samples.push((**evidence).clone());
        }
    }
    samples
}

fn aggregate(r: &Run) -> Aggregate<'_> {
    let mut a = Aggregate::new();
    for e in &r.events {
        if e.kind != "file" && e.kind != "exec" {
            continue;
        }
        if let Some(path) = &e.path {
            a.entry((&e.kind, &e.operation, path))
                .or_default()
                .entry(&e.outcome)
                .or_default()
                .push(&e.evidence);
        }
    }
    a
}

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

fn rank_class(c: &Change) -> RankClass {
    match c.category.as_str() {
        "Exit status" => RankClass::ExitStatus,
        "Capture completeness" => RankClass::CaptureCompleteness,
        "Initial working directory" | "Command arguments" | "Selected startup environment" => {
            RankClass::InvocationContext
        }
        _ if c.left == ["success"]
            && !c.right.is_empty()
            && !c.right.iter().any(|s| s == "success") =>
        {
            RankClass::SuccessToFailure
        }
        "Execution attempt changes"
            if !c.right.is_empty() && !c.right.iter().any(|s| s == "success") =>
        {
            RankClass::FailedExecutionAttempt
        }
        "Executable changes" if !c.right.is_empty() => RankClass::NewExecutable,
        "New file access" => RankClass::NewFileAccess,
        "Executable changes" => RankClass::RemovedExecutable,
        "File access outcome changes" | "Execution attempt changes" => RankClass::OutcomeSetChange,
        _ => RankClass::OtherObservation,
    }
}
/// Compare two loaded runs without performing storage or presentation I/O.
pub(crate) fn compare(l: &Run, r: &Run) -> Report {
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
    };
    let mut simple = |category: &str, left: String, right: String| {
        if left != right {
            report.changes.push(Change {
                category: category.into(),
                subject: String::new(),
                left: vec![left],
                right: vec![right],
                left_evidence: vec![],
                right_evidence: vec![],
                rank_key: RankKey::pending(),
                file_path: None,
            });
        }
    };
    simple(
        "Exit status",
        target_exit(&l.metadata),
        target_exit(&r.metadata),
    );
    simple(
        "Initial working directory",
        l.metadata.cwd.clone(),
        r.metadata.cwd.clone(),
    );
    simple(
        "Command arguments",
        format!("{:?}", l.metadata.command),
        format!("{:?}", r.metadata.command),
    );
    simple(
        "Capture completeness",
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
            report.changes.push(Change {
                category: "Selected startup environment".into(),
                subject: (*key).clone(),
                left: vec![display(left)],
                right: vec![display(right)],
                left_evidence: vec![],
                right_evidence: vec![],
                rank_key: RankKey::pending(),
                file_path: None,
            });
        }
    }
    let a = aggregate(l);
    let b = aggregate(r);
    let empty_outcomes = OutcomeEvidence::new();
    for key in a.keys().chain(b.keys()).collect::<BTreeSet<_>>() {
        let left = a.get(key).unwrap_or(&empty_outcomes);
        let right = b.get(key).unwrap_or(&empty_outcomes);
        if left.keys().eq(right.keys()) {
            continue;
        }
        let category = if key.0 == "exec" {
            if left.keys().chain(right.keys()).any(|s| *s != "success") {
                "Execution attempt changes"
            } else {
                "Executable changes"
            }
        } else if left.is_empty() {
            "New file access"
        } else if right.is_empty() {
            "Removed file access"
        } else {
            "File access outcome changes"
        };
        report.changes.push(Change {
            category: category.into(),
            subject: format!("{} ({})", key.2, key.1),
            left: left.keys().map(|outcome| (*outcome).to_owned()).collect(),
            right: right.keys().map(|outcome| (*outcome).to_owned()).collect(),
            left_evidence: sample_evidence(left, right),
            right_evidence: sample_evidence(right, left),
            rank_key: RankKey::pending(),
            file_path: (key.0 == "file").then(|| key.2.to_owned()),
        });
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
}
impl Quality {
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
        }
    }
    fn trusted(&self) -> bool {
        self.capture == "complete" && self.storage == "verified"
    }
}

/// Per-side reliability facts serialized by the diff-v4 compatibility view.
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
pub(crate) fn evaluate(
    left: &str,
    right: &str,
    l: LoadOutcome<Run>,
    r: LoadOutcome<Run>,
) -> Report {
    match (l, r) {
        (LoadOutcome::Loaded(l), LoadOutcome::Loaded(r)) => compare(&l, &r),
        (l, r) => {
            let mut errors = vec![];
            let mut quality = |side: &str, result: &LoadOutcome<Run>| match result {
                LoadOutcome::Loaded(run) => Quality::of(run),
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
                    }
                }
            };
            let reliability = Reliability {
                left: quality("left", &l),
                right: quality("right", &r),
            };
            Report {
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
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod field_regressions {
    use super::*;
    pub(crate) fn run(raw: &str) -> Run {
        let parsed = crate::parse::parse_with_cwd(raw, "raw/trace.log", Some("/work"));
        assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
        Run {
            metadata: serde_json::from_value(serde_json::json!({
                "schema_version":3,"tool_version":"test","id":"test","name":"test",
                "command":["tool"],"cwd":"/work","started":0,"ended":1,
                "backend":"fixture","state":"completed","exit_code":0,"signal":null,
                "warnings":[],"parsed_events":parsed.events.len(),"raw_lines":parsed.lines,
                "environment":{},"completeness":"complete_for_supported_events"
            }))
            .unwrap(),
            events: parsed.events,
            storage_integrity: crate::model::StorageIntegrity {
                status: "verified".into(),
                detail: String::new(),
            },
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
