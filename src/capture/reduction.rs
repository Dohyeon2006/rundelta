//! Pure reduction of already-observed capture facts.
//!
//! This leaf consumes a bound target spelling/outcome, collector and recorder
//! facts, limit observations and diagnostics. It neither decodes raw outcomes nor
//! reaches parser, process, filesystem or signal adapters. Warning order and
//! recorder precedence remain the saved capture contract; metadata projection
//! cannot manufacture evidence provenance or collapse a signal into an exit code.

use crate::model::{CaptureCompleteness, CaptureState, Metadata};
use std::time::Duration;

/// A process completion observed at one capture boundary.
///
/// `Unknown` keeps an observed raw target completion distinct from a missing
/// completion and from a collector/supervision failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProcessExit {
    Exited(i32),
    Signaled(i32),
    Unknown,
}

impl ProcessExit {
    /// Project a fact to shell convention without replacing the recorded fact.
    pub(super) fn cli_exit(self) -> Option<i32> {
        match self {
            Self::Exited(code) => Some(code),
            Self::Signaled(signal) => Some(128 + signal),
            Self::Unknown => None,
        }
    }

    /// Keep an exit code mutually exclusive with an explicit signal number.
    fn metadata_fields(self) -> (Option<i32>, Option<i32>) {
        match self {
            Self::Exited(code) => (Some(code), None),
            Self::Signaled(signal) => (None, Some(signal)),
            Self::Unknown => (None, None),
        }
    }
}

/// Facts available from the collector boundary after supervision finishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CollectorFacts {
    /// A usable wait result was returned for the owned collector.
    Reaped(ProcessExit),
    /// Recorder policy prevented startup; there is no collector exit fact.
    NotStarted(String),
    /// No usable status escaped supervision; parsed target facts remain separate.
    Failed(String),
}

/// Recorder control-signal observation, kept separate from process outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RecorderStop {
    None,
    Signal(i32),
}

/// Independently observed policy-limit facts.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct CaptureLimitFacts {
    duration: Option<Duration>,
    trace_bytes: Option<TraceBytesLimitFact>,
}

/// Anchored-inode length observed beyond a configured soft byte boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TraceBytesLimitFact {
    observed: u64,
    limit: u64,
}

impl CaptureLimitFacts {
    /// Borrow the observed duration boundary, not an elapsed-time measurement.
    pub(super) fn duration_limit(&self) -> Option<Duration> {
        self.duration
    }

    /// Indicate whether an anchored length has proved the strict byte boundary.
    pub(super) fn trace_bytes_exceeded(&self) -> bool {
        self.trace_bytes.is_some()
    }

    /// Copy a test observation without exposing the mutable fact owner.
    #[cfg(test)]
    pub(super) fn trace_bytes_observation(&self) -> Option<(u64, u64)> {
        self.trace_bytes.map(|fact| (fact.observed, fact.limit))
    }

    /// Keep duration and byte observations independent while querying either.
    /// No clock or trace length is inspected by this already-observed fact.
    pub(super) fn exceeded(&self) -> bool {
        self.duration.is_some() || self.trace_bytes.is_some()
    }

    /// Latch the first observation so supervision starts termination only once.
    pub(super) fn observe_duration(&mut self, limit: Duration) -> bool {
        if self.duration.is_some() {
            false
        } else {
            self.duration = Some(limit);
            true
        }
    }

    /// Retain the latest supplied excess length; equality does not cross a limit.
    /// The return value identifies the first excess observation only.
    pub(super) fn observe_trace_bytes(&mut self, observed: u64, limit: u64) -> bool {
        if observed <= limit {
            return false;
        }
        let first = self.trace_bytes.is_none();
        self.trace_bytes = Some(TraceBytesLimitFact { observed, limit });
        first
    }

    /// Project policy facts in the frozen duration-before-bytes warning order.
    fn append_warnings(&self, warnings: &mut Vec<String>) {
        if let Some(limit) = self.duration {
            warnings.push(format!(
                "capture duration limit exceeded (limit: {} ms)",
                limit.as_millis()
            ));
        }
        if let Some(fact) = self.trace_bytes {
            warnings.push(format!(
                "capture trace byte limit exceeded (observed: {} bytes, limit: {} bytes)",
                fact.observed, fact.limit
            ));
        }
    }
}

/// One observed target exit, with its exact saved spelling and decoded fact.
///
/// Presence binds both values together. Unknown decoding retains the spelling;
/// absence is represented by the outer Option. The capture adapter supplies the
/// selected contract's decoding; this owner performs no raw parsing or I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ObservedTargetExit {
    raw: String,
    outcome: ProcessExit,
}

impl ObservedTargetExit {
    /// Bind a spelling to its already-decoded outcome without reinterpreting it.
    pub(super) fn new(raw: &str, outcome: ProcessExit) -> Self {
        Self {
            raw: raw.to_owned(),
            outcome,
        }
    }
}

/// Pure capture-policy result before persistence and CLI presentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CaptureDecision {
    target: Option<ObservedTargetExit>,
    collector: CollectorFacts,
    recorder: RecorderStop,
    limits: CaptureLimitFacts,
    recorder_supervision_error: Option<String>,
    state: CaptureState,
    completeness: CaptureCompleteness,
    warnings: Vec<String>,
    cli_exit: i32,
}

impl CaptureDecision {
    /// Preserve the computed capture state without recomputing its priority.
    pub(super) fn state(&self) -> CaptureState {
        self.state
    }

    /// Return the completeness already determined from all supplied facts.
    pub(super) fn completeness(&self) -> CaptureCompleteness {
        self.completeness
    }

    /// Return the frozen CLI projection independently of stored process facts.
    pub(super) fn cli_exit(&self) -> i32 {
        self.cli_exit
    }

    /// Copy the decoded test fact; Unknown remains different from no observation.
    #[cfg(test)]
    pub(super) fn target(&self) -> Option<ProcessExit> {
        self.target.as_ref().map(|target| target.outcome)
    }

    /// Borrow the paired original spelling without decoding or manufacturing it.
    #[cfg(test)]
    pub(super) fn target_exit_raw(&self) -> Option<&str> {
        self.target.as_ref().map(|target| target.raw.as_str())
    }

    /// Borrow diagnostics in their frozen observation order for boundary tests.
    #[cfg(test)]
    pub(super) fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Copy every original result field for the independent legacy test oracle.
    /// Snapshot fields belong to this detached test value, never to the owner.
    #[cfg(test)]
    pub(super) fn snapshot(&self) -> DecisionSnapshot {
        DecisionSnapshot {
            target: self.target(),
            target_exit_raw: self.target_exit_raw().map(str::to_owned),
            collector: self.collector.clone(),
            recorder: self.recorder,
            limits: self.limits.clone(),
            recorder_supervision_error: self.recorder_supervision_error.clone(),
            state: self.state,
            completeness: self.completeness,
            warnings: self.warnings.clone(),
            cli_exit: self.cli_exit,
        }
    }

    /// Project reduced outcome facts to their unchanged metadata representations.
    /// The caller separately freezes trace-v1 references from the same parse;
    /// collector/reducer facts cannot invent target evidence provenance.
    pub(super) fn apply_to_metadata(self, metadata: &mut Metadata) {
        debug_assert_eq!(
            matches!(self.recorder, RecorderStop::Signal(_)),
            self.state == CaptureState::Interrupted
        );
        debug_assert!(!self.limits.exceeded() || self.state != CaptureState::Completed);
        (metadata.exit_code, metadata.signal) = self
            .target
            .as_ref()
            .map(|target| target.outcome.metadata_fields())
            .unwrap_or((None, None));
        (metadata.collector_exit_code, metadata.collector_signal) = match self.collector {
            CollectorFacts::Reaped(exit) => exit.metadata_fields(),
            CollectorFacts::NotStarted(_) | CollectorFacts::Failed(_) => (None, None),
        };
        metadata.target_exit_raw = self.target.map(|target| target.raw);
        metadata.state = self.state.to_string();
        metadata.completeness = Some(self.completeness.to_string());
        metadata.warnings = self.warnings;
    }
}

/// Detached test-only projection of all ten fields in the prior result.
///
/// The legacy oracle constructs this fixture independently; full equality keeps
/// recorder, limits, diagnostics and CLI facts visible beyond persisted metadata.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DecisionSnapshot {
    pub(super) target: Option<ProcessExit>,
    pub(super) target_exit_raw: Option<String>,
    pub(super) collector: CollectorFacts,
    pub(super) recorder: RecorderStop,
    pub(super) limits: CaptureLimitFacts,
    pub(super) recorder_supervision_error: Option<String>,
    pub(super) state: CaptureState,
    pub(super) completeness: CaptureCompleteness,
    pub(super) warnings: Vec<String>,
    pub(super) cli_exit: i32,
}

/// Independent observations supplied to the pure reducer after trace reading.
/// The caller owns startup, cleanup, parsing, and persistence decisions.
pub(super) struct CaptureFactsInput<'a> {
    target: Option<ObservedTargetExit>,
    missing_generations: &'a [String],
    collector: CollectorFacts,
    recorder: RecorderStop,
    limits: CaptureLimitFacts,
    recorder_supervision_error: Option<String>,
    warnings: Vec<String>,
    trace_read_error: Option<String>,
}

impl<'a> CaptureFactsInput<'a> {
    /// Assemble independent observations without a target decoder or I/O seam.
    /// Errors default to absence and can be supplied together before reduction.
    pub(super) fn new(
        target: Option<ObservedTargetExit>,
        missing_generations: &'a [String],
        collector: CollectorFacts,
        recorder: RecorderStop,
        limits: CaptureLimitFacts,
        warnings: Vec<String>,
    ) -> Self {
        Self {
            target,
            missing_generations,
            collector,
            recorder,
            limits,
            recorder_supervision_error: None,
            warnings,
            trace_read_error: None,
        }
    }

    /// Retain supervision and raw-read errors as separate facts; reduction owns
    /// their existing warning positions and never replaces collector outcomes.
    pub(super) fn with_observation_errors(
        mut self,
        recorder_supervision_error: Option<String>,
        trace_read_error: Option<String>,
    ) -> Self {
        self.recorder_supervision_error = recorder_supervision_error;
        self.trace_read_error = trace_read_error;
        self
    }
}

/// Classify already-observed facts without I/O or inventing process outcomes.
///
/// Recorder cancellation takes precedence over capture failure for `state` and
/// CLI exit projection, but neither removes target/collector facts or warnings.
/// Parser limitations lower completeness even when process completion succeeds.
pub(super) fn reduce_capture_facts(input: CaptureFactsInput<'_>) -> CaptureDecision {
    let CaptureFactsInput {
        target,
        missing_generations,
        collector,
        recorder,
        limits,
        recorder_supervision_error,
        mut warnings,
        trace_read_error,
    } = input;
    if let Some(error) = trace_read_error {
        warnings.push(format!("cannot read trace: {error}"));
    }

    let target_outcome = target.as_ref().map(|target| target.outcome);
    if target
        .as_ref()
        .is_some_and(|target| target.raw.starts_with("+++ killed by "))
        && target_outcome == Some(ProcessExit::Unknown)
    {
        warnings.push("unrecognized target termination signal".into());
    }
    if !missing_generations.is_empty() {
        warnings.push(format!(
            "process completion not observed for PIDs: {}",
            missing_generations.join(", ")
        ));
    }
    if target.is_none() {
        warnings.push("root process completion not observed; capture incomplete".into());
    }

    let (confirmation, collector_failed, collector_unknown, collector_exit) = match &collector {
        CollectorFacts::Reaped(exit) => {
            let unknown = *exit == ProcessExit::Unknown;
            let confirmation = match target_outcome {
                Some(ProcessExit::Exited(code)) => Some(*exit == ProcessExit::Exited(code)),
                Some(ProcessExit::Signaled(signal)) => Some(*exit == ProcessExit::Signaled(signal)),
                Some(ProcessExit::Unknown) | None => None,
            };
            if unknown || confirmation == Some(false) {
                warnings.push("collector status does not confirm target completion".into());
            }
            (confirmation, false, unknown, Some(*exit))
        }
        CollectorFacts::NotStarted(reason) | CollectorFacts::Failed(reason) => {
            // A collector boundary fact cannot erase independently observed target facts.
            warnings.push(reason.clone());
            (None, true, false, None)
        }
    };
    limits.append_warnings(&mut warnings);
    if let Some(error) = &recorder_supervision_error {
        warnings.push(error.clone());
    }
    let state = if matches!(recorder, RecorderStop::Signal(_)) {
        CaptureState::Interrupted
    } else if target.is_none()
        || collector_failed
        || collector_unknown
        || limits.exceeded()
        || recorder_supervision_error.is_some()
        || confirmation == Some(false)
    {
        CaptureState::CaptureFailed
    } else {
        CaptureState::Completed
    };
    let completeness = if state != CaptureState::Completed {
        CaptureCompleteness::Incomplete
    } else if warnings.is_empty() {
        CaptureCompleteness::CompleteForSupportedEvents
    } else {
        CaptureCompleteness::Partial
    };
    let cli_exit = match recorder {
        RecorderStop::Signal(signal) => 128 + signal,
        RecorderStop::None if state == CaptureState::CaptureFailed => 125,
        RecorderStop::None => target_outcome
            .and_then(ProcessExit::cli_exit)
            .or_else(|| collector_exit.and_then(ProcessExit::cli_exit))
            .unwrap_or(125),
    };

    CaptureDecision {
        target,
        collector,
        recorder,
        limits,
        recorder_supervision_error,
        state,
        completeness,
        warnings,
        cli_exit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // These numbers are supplied facts for the reducer, not host ABI decoding.
    // The platform adapter separately tests how each vocabulary produces them.
    const OBSERVED_TERM: i32 = 15;
    const OBSERVED_INT: i32 = 2;
    const OBSERVED_HUP: i32 = 1;

    fn reduce_capture(
        target: Option<ObservedTargetExit>,
        missing_generations: &[String],
        collector: CollectorFacts,
        recorder: RecorderStop,
        recorder_supervision_error: Option<String>,
        warnings: Vec<String>,
        trace_read_error: Option<String>,
    ) -> CaptureDecision {
        reduce_capture_facts(CaptureFactsInput {
            target,
            missing_generations,
            collector,
            recorder,
            limits: CaptureLimitFacts::default(),
            recorder_supervision_error,
            warnings,
            trace_read_error,
        })
    }

    #[test]
    fn normal_facts_have_the_frozen_decision() {
        let decision = reduce_capture(
            Some(ObservedTargetExit::new(
                "+++ exited with 0 +++",
                ProcessExit::Exited(0),
            )),
            &[],
            CollectorFacts::Reaped(ProcessExit::Exited(0)),
            RecorderStop::None,
            None,
            vec![],
            None,
        );
        assert_eq!(
            decision,
            CaptureDecision {
                target: Some(ObservedTargetExit::new(
                    "+++ exited with 0 +++",
                    ProcessExit::Exited(0)
                )),
                collector: CollectorFacts::Reaped(ProcessExit::Exited(0)),
                recorder: RecorderStop::None,
                limits: CaptureLimitFacts::default(),
                recorder_supervision_error: None,
                state: CaptureState::Completed,
                completeness: CaptureCompleteness::CompleteForSupportedEvents,
                warnings: vec![],
                cli_exit: 0,
            }
        );
    }

    #[test]
    fn metadata_projection_keeps_target_and_collector_fields_separate() {
        let decision = reduce_capture(
            Some(ObservedTargetExit::new(
                "+++ killed by SIGTERM +++",
                ProcessExit::Signaled(OBSERVED_TERM),
            )),
            &[],
            CollectorFacts::Failed("collector wait failed".into()),
            RecorderStop::None,
            None,
            vec![],
            None,
        );
        let mut metadata = Metadata {
            schema_version: 3,
            evidence_contract: None,
            target_provenance: None,
            tool_version: "test".into(),
            id: "id".into(),
            name: "name".into(),
            command: vec![],
            cwd: "/".into(),
            started: 0,
            ended: Some(1),
            backend: "test".into(),
            state: "running".into(),
            exit_code: Some(99),
            signal: None,
            warnings: vec![],
            parsed_events: 0,
            raw_lines: 0,
            environment: None,
            completeness: Some("pending".into()),
            collector_exit_code: Some(99),
            collector_signal: Some(99),
            target_exit_raw: None,
        };

        decision.apply_to_metadata(&mut metadata);

        assert_eq!(
            (metadata.exit_code, metadata.signal),
            (None, Some(OBSERVED_TERM))
        );
        assert_eq!(
            (metadata.collector_exit_code, metadata.collector_signal),
            (None, None)
        );
        assert_eq!(
            metadata.target_exit_raw.as_deref(),
            Some("+++ killed by SIGTERM +++")
        );
        assert_eq!(metadata.state, "capture_failed");
        assert_eq!(metadata.completeness.as_deref(), Some("incomplete"));
        assert_eq!(metadata.warnings, ["collector wait failed"]);
    }

    #[test]
    fn unknown_signal_and_missing_root_keep_distinct_semantics() {
        let unknown = reduce_capture(
            Some(ObservedTargetExit::new(
                "+++ killed by SIGFUTURE +++",
                ProcessExit::Unknown,
            )),
            &[],
            CollectorFacts::Reaped(ProcessExit::Exited(9)),
            RecorderStop::None,
            None,
            vec![],
            None,
        );
        assert_eq!(unknown.target(), Some(ProcessExit::Unknown));
        assert_eq!(unknown.state, CaptureState::Completed);
        assert_eq!(unknown.completeness, CaptureCompleteness::Partial);
        assert_eq!(unknown.cli_exit, 9);
        assert_eq!(unknown.warnings, ["unrecognized target termination signal"]);

        let missing = reduce_capture(
            None,
            &[],
            CollectorFacts::Reaped(ProcessExit::Exited(0)),
            RecorderStop::None,
            None,
            vec![],
            None,
        );
        assert_eq!(missing.target(), None);
        assert_eq!(missing.state, CaptureState::CaptureFailed);
        assert_eq!(missing.completeness, CaptureCompleteness::Incomplete);
        assert_eq!(missing.cli_exit, 125);
        assert_eq!(
            missing.warnings,
            ["root process completion not observed; capture incomplete"]
        );
    }

    #[test]
    fn missing_generation_is_partial_without_changing_target_success() {
        let decision = reduce_capture(
            Some(ObservedTargetExit::new(
                "+++ exited with 0 +++",
                ProcessExit::Exited(0),
            )),
            &["21#2".into()],
            CollectorFacts::Reaped(ProcessExit::Exited(0)),
            RecorderStop::None,
            None,
            vec![],
            None,
        );
        assert_eq!(decision.state, CaptureState::Completed);
        assert_eq!(decision.completeness, CaptureCompleteness::Partial);
        assert_eq!(decision.cli_exit, 0);
        assert_eq!(
            decision.warnings,
            ["process completion not observed for PIDs: 21#2"]
        );
    }

    #[test]
    fn raw_read_error_and_missing_root_fail_with_ordered_warnings() {
        let decision = reduce_capture(
            None,
            &[],
            CollectorFacts::Reaped(ProcessExit::Exited(0)),
            RecorderStop::None,
            None,
            vec!["parser warning".into()],
            Some("trace read failed".into()),
        );
        assert_eq!(decision.state, CaptureState::CaptureFailed);
        assert_eq!(decision.completeness, CaptureCompleteness::Incomplete);
        assert_eq!(decision.cli_exit, 125);
        assert_eq!(
            decision.warnings,
            [
                "parser warning",
                "cannot read trace: trace read failed",
                "root process completion not observed; capture incomplete",
            ]
        );
    }

    #[test]
    fn signal_facts_are_not_replaced_by_numeric_cli_equivalence() {
        let decision = reduce_capture(
            Some(ObservedTargetExit::new(
                "+++ exited with 143 +++",
                ProcessExit::Exited(143),
            )),
            &[],
            CollectorFacts::Reaped(ProcessExit::Signaled(OBSERVED_TERM)),
            RecorderStop::None,
            None,
            vec![],
            None,
        );
        assert_eq!(decision.target(), Some(ProcessExit::Exited(143)));
        assert_eq!(
            decision.collector,
            CollectorFacts::Reaped(ProcessExit::Signaled(OBSERVED_TERM))
        );
        assert_eq!(decision.state, CaptureState::CaptureFailed);
        assert_eq!(decision.cli_exit, 125);
        assert_eq!(
            decision.warnings,
            ["collector status does not confirm target completion"]
        );
    }

    #[test]
    fn recorder_signal_is_independent_of_target_and_collector() {
        let decision = reduce_capture(
            Some(ObservedTargetExit::new(
                "+++ exited with 7 +++",
                ProcessExit::Exited(7),
            )),
            &[],
            CollectorFacts::Reaped(ProcessExit::Exited(7)),
            RecorderStop::Signal(OBSERVED_INT),
            None,
            vec![],
            None,
        );
        assert_eq!(decision.target(), Some(ProcessExit::Exited(7)));
        assert_eq!(
            decision.collector,
            CollectorFacts::Reaped(ProcessExit::Exited(7))
        );
        assert_eq!(decision.recorder, RecorderStop::Signal(OBSERVED_INT));
        assert_eq!(decision.state, CaptureState::Interrupted);
        assert_eq!(decision.completeness, CaptureCompleteness::Incomplete);
        assert_eq!(decision.cli_exit, 128 + OBSERVED_INT);
    }

    #[test]
    fn collector_failure_preserves_independent_target_facts() {
        let decision = reduce_capture(
            Some(ObservedTargetExit::new(
                "+++ killed by SIGTERM +++",
                ProcessExit::Signaled(OBSERVED_TERM),
            )),
            &["2".into()],
            CollectorFacts::Failed("collector wait failed".into()),
            RecorderStop::None,
            None,
            vec!["parser warning".into()],
            Some("trace read failed".into()),
        );
        assert_eq!(
            decision.target(),
            Some(ProcessExit::Signaled(OBSERVED_TERM))
        );
        assert_eq!(
            decision.target_exit_raw(),
            Some("+++ killed by SIGTERM +++")
        );
        assert_eq!(
            decision.collector,
            CollectorFacts::Failed("collector wait failed".into())
        );
        assert_eq!(decision.state, CaptureState::CaptureFailed);
        assert_eq!(decision.completeness, CaptureCompleteness::Incomplete);
        assert_eq!(decision.cli_exit, 125);
        assert_eq!(
            decision.warnings,
            [
                "parser warning",
                "cannot read trace: trace read failed",
                "process completion not observed for PIDs: 2",
                "collector wait failed",
            ]
        );
    }

    #[test]
    fn collector_failure_without_root_never_invents_target_facts() {
        let decision = reduce_capture(
            None,
            &[],
            CollectorFacts::Failed("collector wait failed".into()),
            RecorderStop::None,
            None,
            vec![],
            None,
        );
        assert_eq!(decision.target(), None);
        assert_eq!(decision.target_exit_raw(), None);
        assert_eq!(decision.state, CaptureState::CaptureFailed);
        assert_eq!(decision.completeness, CaptureCompleteness::Incomplete);
        assert_eq!(decision.cli_exit, 125);
        assert_eq!(
            decision.warnings,
            [
                "root process completion not observed; capture incomplete",
                "collector wait failed",
            ]
        );
    }

    #[test]
    fn recorder_signal_has_precedence_over_collector_failure() {
        let decision = reduce_capture(
            Some(ObservedTargetExit::new(
                "+++ exited with 4 +++",
                ProcessExit::Exited(4),
            )),
            &[],
            CollectorFacts::Failed("collector wait failed".into()),
            RecorderStop::Signal(OBSERVED_TERM),
            None,
            vec![],
            None,
        );
        assert_eq!(decision.target(), Some(ProcessExit::Exited(4)));
        assert_eq!(decision.state, CaptureState::Interrupted);
        assert_eq!(decision.completeness, CaptureCompleteness::Incomplete);
        assert_eq!(decision.cli_exit, 128 + OBSERVED_TERM);
    }

    #[test]
    fn recorder_supervision_failure_preserves_reaped_collector_fact() {
        let decision = reduce_capture(
            Some(ObservedTargetExit::new(
                "+++ exited with 0 +++",
                ProcessExit::Exited(0),
            )),
            &[],
            CollectorFacts::Reaped(ProcessExit::Exited(0)),
            RecorderStop::None,
            Some("recorder signal supervision failed: injected read error".into()),
            vec![],
            None,
        );
        assert_eq!(
            decision.collector,
            CollectorFacts::Reaped(ProcessExit::Exited(0))
        );
        assert_eq!(decision.state, CaptureState::CaptureFailed);
        assert_eq!(decision.completeness, CaptureCompleteness::Incomplete);
        assert_eq!(decision.cli_exit, 125);
        assert_eq!(
            decision.warnings,
            ["recorder signal supervision failed: injected read error"]
        );

        let mut metadata = Metadata {
            schema_version: 3,
            evidence_contract: None,
            target_provenance: None,
            tool_version: "test".into(),
            id: "id".into(),
            name: "name".into(),
            command: vec![],
            cwd: "/".into(),
            started: 0,
            ended: Some(1),
            backend: "test".into(),
            state: "running".into(),
            exit_code: None,
            signal: None,
            warnings: vec![],
            parsed_events: 0,
            raw_lines: 0,
            environment: None,
            completeness: Some("pending".into()),
            collector_exit_code: None,
            collector_signal: None,
            target_exit_raw: None,
        };
        decision.apply_to_metadata(&mut metadata);
        assert_eq!(metadata.exit_code, Some(0));
        assert_eq!(metadata.collector_exit_code, Some(0));
        assert_eq!(metadata.state, "capture_failed");
        assert_eq!(metadata.completeness.as_deref(), Some("incomplete"));

        let interrupted = reduce_capture(
            Some(ObservedTargetExit::new(
                "+++ exited with 0 +++",
                ProcessExit::Exited(0),
            )),
            &[],
            CollectorFacts::Reaped(ProcessExit::Exited(0)),
            RecorderStop::Signal(OBSERVED_TERM),
            Some("recorder signal supervision failed: injected read error".into()),
            vec![],
            None,
        );
        assert_eq!(interrupted.state, CaptureState::Interrupted);
        assert_eq!(interrupted.cli_exit, 128 + OBSERVED_TERM);
        assert_eq!(
            interrupted.collector,
            CollectorFacts::Reaped(ProcessExit::Exited(0))
        );
        assert_eq!(
            interrupted.recorder_supervision_error.as_deref(),
            Some("recorder signal supervision failed: injected read error")
        );
    }

    #[test]
    fn cancellation_before_spawn_is_not_a_collector_failure() {
        let decision = reduce_capture(
            None,
            &[],
            CollectorFacts::NotStarted(
                "collector was not started because recorder cancellation was observed".into(),
            ),
            RecorderStop::Signal(OBSERVED_HUP),
            None,
            vec![],
            None,
        );
        assert_eq!(decision.target(), None);
        assert_eq!(decision.target_exit_raw(), None);
        assert!(matches!(decision.collector, CollectorFacts::NotStarted(_)));
        assert_eq!(decision.state, CaptureState::Interrupted);
        assert_eq!(decision.completeness, CaptureCompleteness::Incomplete);
        assert_eq!(decision.cli_exit, 128 + OBSERVED_HUP);
        assert_eq!(
            decision.warnings,
            [
                "root process completion not observed; capture incomplete",
                "collector was not started because recorder cancellation was observed",
            ]
        );
    }

    #[test]
    fn capture_limit_priority_is_signal_then_limit_then_process_facts() {
        let duration = Duration::from_millis(17);
        let limits = CaptureLimitFacts {
            duration: Some(duration),
            trace_bytes: Some(TraceBytesLimitFact {
                observed: 33,
                limit: 32,
            }),
        };
        let limited = reduce_capture_facts(CaptureFactsInput {
            target: Some(ObservedTargetExit::new(
                "+++ exited with 0 +++",
                ProcessExit::Exited(0),
            )),
            missing_generations: &[],
            collector: CollectorFacts::Reaped(ProcessExit::Exited(0)),
            recorder: RecorderStop::None,
            limits: limits.clone(),
            recorder_supervision_error: None,
            warnings: vec![],
            trace_read_error: None,
        });
        assert_eq!(limited.target(), Some(ProcessExit::Exited(0)));
        assert_eq!(
            limited.collector,
            CollectorFacts::Reaped(ProcessExit::Exited(0))
        );
        assert_eq!(limited.state, CaptureState::CaptureFailed);
        assert_eq!(limited.completeness, CaptureCompleteness::Incomplete);
        assert_eq!(limited.cli_exit, 125);
        assert_eq!(
            limited.warnings,
            [
                "capture duration limit exceeded (limit: 17 ms)",
                "capture trace byte limit exceeded (observed: 33 bytes, limit: 32 bytes)",
            ]
        );

        let interrupted = reduce_capture_facts(CaptureFactsInput {
            target: Some(ObservedTargetExit::new(
                "+++ exited with 0 +++",
                ProcessExit::Exited(0),
            )),
            missing_generations: &[],
            collector: CollectorFacts::Failed("collector wait failed".into()),
            recorder: RecorderStop::Signal(OBSERVED_TERM),
            limits,
            recorder_supervision_error: None,
            warnings: vec![],
            trace_read_error: None,
        });
        assert_eq!(interrupted.state, CaptureState::Interrupted);
        assert_eq!(interrupted.completeness, CaptureCompleteness::Incomplete);
        assert_eq!(interrupted.cli_exit, 128 + OBSERVED_TERM);
        assert!(
            interrupted
                .warnings
                .iter()
                .any(|warning| warning.contains("duration limit"))
        );
        assert!(
            interrupted
                .warnings
                .iter()
                .any(|warning| warning.contains("trace byte limit"))
        );
        assert!(
            interrupted
                .warnings
                .iter()
                .any(|warning| warning == "collector wait failed")
        );
    }

    #[test]
    fn trace_byte_limit_is_strictly_greater_than_the_soft_bound() {
        let mut facts = CaptureLimitFacts::default();
        assert!(!facts.observe_trace_bytes(8, 8));
        assert_eq!(facts.trace_bytes, None);
        assert!(facts.observe_trace_bytes(9, 8));
        assert_eq!(
            facts.trace_bytes,
            Some(TraceBytesLimitFact {
                observed: 9,
                limit: 8
            })
        );
        assert!(!facts.observe_trace_bytes(12, 8));
        assert_eq!(facts.trace_bytes.unwrap().observed, 12);
    }

    #[test]
    fn snapshot_preserves_bound_unknown_and_independent_limit_diagnostics() {
        let limits = CaptureLimitFacts {
            duration: Some(Duration::from_millis(17)),
            trace_bytes: Some(TraceBytesLimitFact {
                observed: 9,
                limit: 8,
            }),
        };
        let decision = reduce_capture_facts(
            CaptureFactsInput::new(
                Some(ObservedTargetExit::new(
                    "+++ killed by SIGFUTURE +++",
                    ProcessExit::Unknown,
                )),
                &["9#2".into()],
                CollectorFacts::Failed("collector failure".into()),
                RecorderStop::Signal(OBSERVED_INT),
                limits.clone(),
                vec!["parser warning".into()],
            )
            .with_observation_errors(
                Some("supervision failure".into()),
                Some("raw read failure".into()),
            ),
        );
        assert_eq!(
            decision.snapshot(),
            DecisionSnapshot {
                target: Some(ProcessExit::Unknown),
                target_exit_raw: Some("+++ killed by SIGFUTURE +++".into()),
                collector: CollectorFacts::Failed("collector failure".into()),
                recorder: RecorderStop::Signal(OBSERVED_INT),
                limits,
                recorder_supervision_error: Some("supervision failure".into()),
                state: CaptureState::Interrupted,
                completeness: CaptureCompleteness::Incomplete,
                warnings: vec![
                    "parser warning".into(),
                    "cannot read trace: raw read failure".into(),
                    "unrecognized target termination signal".into(),
                    "process completion not observed for PIDs: 9#2".into(),
                    "collector failure".into(),
                    "capture duration limit exceeded (limit: 17 ms)".into(),
                    "capture trace byte limit exceeded (observed: 9 bytes, limit: 8 bytes)".into(),
                    "supervision failure".into(),
                ],
                cli_exit: 128 + OBSERVED_INT,
            }
        );
        let missing = reduce_capture(
            None,
            &[],
            CollectorFacts::Reaped(ProcessExit::Exited(0)),
            RecorderStop::None,
            None,
            vec![],
            None,
        )
        .snapshot();
        assert_eq!((missing.target, missing.target_exit_raw), (None, None));
    }
}
