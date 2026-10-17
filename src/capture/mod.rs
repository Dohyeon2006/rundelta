//! One-record workflow facade at the Linux process boundary.
//!
//! The facade owns validation, storage, normalization and fact publication.
//! Private completion selects evidence references; reduction classifies supplied
//! facts; platform_exit decodes observed exits; policy owns limit configuration.
//! Private supervision owns Linux children, signals, pidfds, pipes and deadlines.
//! The facade depends on these children, parse, store and model. Supervision uses
//! policy/reduction/platform_exit and a length callback, never parser or storage
//! internals. Storage and output failures stay outside pure reduction. Persisted
//! default metadata stays on schema v3. Explicit trace-v1 recording freezes task
//! generation and target-outcome references under metadata v4 / Event v3;
//! storage binds their structured commit to the same raw byte stream.
//!
//! The CLI first replaces itself with a private, clean-loader image at the same
//! PID. That removes old host threads/reapers before acquiring capture resources;
//! raw target environment and inherited signals remain separate from the owner's
//! exclusive waitability. This is a binary process boundary, not a returning
//! multithreaded embedding API. Kernel deadlines and arbitrary native interference
//! are not established by the image boundary or the cooperating storage lock.

mod completion;
mod owner;
mod platform_exit;
mod policy;
mod reduction;
mod supervision;

use completion::Completion;
use platform_exit::decode_target;
pub(crate) use policy::CaptureLimits;
use reduction::{CaptureFactsInput, CaptureLimitFacts, CollectorFacts, reduce_capture_facts};
use supervision::{
    ChildLifecycleScope, CollectorLaunch, SignalScope, VersionProbeOutcome, capture_collector,
    is_unquiesced, probe_backend, require_single_threaded_recorder,
};

#[cfg(test)]
use platform_exit::legacy_signal_number as signal_number;
#[cfg(test)]
use reduction::{CaptureDecision, DecisionSnapshot, ProcessExit, RecorderStop};

#[cfg(test)]
use crate::model::{CaptureCompleteness, CaptureState};
use crate::{
    model::{Metadata, RecordResult, RecordingEvidenceContract, bytes},
    parse, store,
};

use anyhow::{Context, Result, bail};
#[cfg(test)]
use std::time::Duration;
use std::{
    io,
    os::unix::ffi::OsStrExt,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

/// Wall-clock metadata timestamp, independent of the monotonic policy clock.
///
/// Pre-epoch readings retain the historical zero fallback. These timestamps do
/// not establish elapsed-time ordering when the system clock changes.
fn now() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
fn reduce_capture(
    root_outcome: Option<&str>,
    missing_generations: &[String],
    collector: CollectorFacts,
    recorder: RecorderStop,
    recorder_supervision_error: Option<String>,
    warnings: Vec<String>,
    trace_read_error: Option<String>,
) -> CaptureDecision {
    reduce_capture_facts(
        CaptureFactsInput::new(
            root_outcome.map(|raw| decode_target(raw, RecordingEvidenceContract::StructuredV3)),
            missing_generations,
            collector,
            recorder,
            CaptureLimitFacts::default(),
            warnings,
        )
        .with_observation_errors(recorder_supervision_error, trace_read_error),
    )
}

/// Retain errors from distinct supervision stages in their observation order.
fn append_recorder_supervision_error(slot: &mut Option<String>, error: String) {
    *slot = Some(match slot.take() {
        Some(previous) => format!("{previous}; additionally {error}"),
        None => error,
    });
}

/// Incrementally parse one trace, discarding every prefix fact on read failure.
///
/// `feed` is the I/O boundary: production streams logical lines from the
/// anchored writer, while tests can inject a late failure after accepted lines.
/// Finishing a fresh parser on error preserves the historical whole-read
/// contract—no prefix events, warnings, task generations, or line count escape.
fn parse_trace_stream<F>(initial_cwd: &str, feed: F) -> (parse::Parsed, Option<String>)
where
    F: FnOnce(&mut parse::TraceParser<'_>) -> io::Result<()>,
{
    const EVIDENCE_FILE: &str = "raw/trace.log";
    let mut parser = parse::TraceParser::new(EVIDENCE_FILE, Some(initial_cwd));
    match feed(&mut parser) {
        Ok(()) => (parser.finish(), None),
        Err(error) => (
            parse::TraceParser::new(EVIDENCE_FILE, Some(initial_cwd)).finish(),
            Some(error.to_string()),
        ),
    }
}

fn empty_trace_parse(initial_cwd: &str) -> parse::Parsed {
    parse::TraceParser::new("raw/trace.log", Some(initial_cwd)).finish()
}

/// Startup permission after backend preflight and subsequent signal boundaries.
/// Cancellation may coexist with a cleanup/observation error, so that error
/// belongs inside the cancelled variant rather than replacing its control fact.
#[derive(Debug, PartialEq, Eq)]
enum StartupGate {
    Ready,
    Cancelled { supervision_error: Option<String> },
    SignalObservationFailed(String),
}

impl StartupGate {
    fn from_probe(probe: VersionProbeOutcome) -> (Vec<u8>, Self) {
        match probe {
            VersionProbeOutcome::Completed(stdout) => (stdout, Self::Ready),
            VersionProbeOutcome::Cancelled { supervision_error } => {
                (Vec::new(), Self::Cancelled { supervision_error })
            }
        }
    }

    fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }

    /// Keep the existing observation boundary: a prior error prevents another
    /// read, while known cancellation without an error still drains this stage.
    /// The closure makes that no-read policy directly testable without signals.
    fn observe_before_spawn(&mut self, observe: impl FnOnce() -> Result<Option<i32>>) {
        if self.supervision_error().is_some() {
            return;
        }
        match observe() {
            Ok(Some(_)) => {
                *self = Self::Cancelled {
                    supervision_error: None,
                }
            }
            Ok(None) => {}
            Err(error) => {
                let error =
                    format!("recorder signal supervision failed before collector spawn: {error:#}");
                match self {
                    Self::Cancelled { supervision_error } => *supervision_error = Some(error),
                    Self::Ready => *self = Self::SignalObservationFailed(error),
                    Self::SignalObservationFailed(_) => {
                        unreachable!("prior error skips observation")
                    }
                }
            }
        }
    }

    fn supervision_error(&self) -> Option<&str> {
        match self {
            Self::Ready => None,
            Self::Cancelled { supervision_error } => supervision_error.as_deref(),
            Self::SignalObservationFailed(error) => Some(error),
        }
    }
}

/// Admission to normalization, distinct from independent resource-limit facts.
/// An unverified length is latched: a later successful size read cannot undo an
/// earlier supervision failure. Duration alone does not suppress trace parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TraceReadPlan {
    Parse,
    SuppressedLengthUnverified,
    SuppressedByteLimit,
}

impl TraceReadPlan {
    fn after_limits(self, limits: &CaptureLimitFacts) -> Self {
        if self == Self::SuppressedLengthUnverified {
            self
        } else if limits.trace_bytes_exceeded() {
            Self::SuppressedByteLimit
        } else {
            self
        }
    }
}

#[cfg(test)]
mod workflow_phase_tests {
    use super::*;

    #[test]
    fn startup_gate_keeps_ready_cancelled_and_failed_boundaries_distinct() {
        let (stdout, mut ready) = StartupGate::from_probe(VersionProbeOutcome::Completed(
            b"verified backend version\n".to_vec(),
        ));
        assert_eq!(stdout, b"verified backend version\n");
        ready.observe_before_spawn(|| Ok(None));
        assert_eq!(ready, StartupGate::Ready);
        ready.observe_before_spawn(|| Ok(Some(libc::SIGINT)));
        assert_eq!(
            ready,
            StartupGate::Cancelled {
                supervision_error: None
            }
        );
        // A later failed observation must not erase cancellation. Its exact
        // diagnostic still reaches the reducer in the existing warning slot.
        ready.observe_before_spawn(|| bail!("late fixture failure"));
        assert_eq!(
            ready,
            StartupGate::Cancelled {
                supervision_error: Some(
                    "recorder signal supervision failed before collector spawn: late fixture failure".into()
                )
            }
        );
        ready.observe_before_spawn(|| panic!("a prior failure forbids another signal read"));

        let mut failed = StartupGate::Ready;
        failed.observe_before_spawn(|| bail!("early fixture failure"));
        assert_eq!(
            failed,
            StartupGate::SignalObservationFailed(
                "recorder signal supervision failed before collector spawn: early fixture failure"
                    .into()
            )
        );
        failed.observe_before_spawn(|| panic!("failed supervision cannot admit startup"));
        assert!(!failed.is_ready());
    }

    #[test]
    fn preflight_cancellation_retains_cleanup_error_and_skips_later_reads() {
        for error in [None, Some("cancel cleanup failed".to_string())] {
            let (stdout, mut gate) = StartupGate::from_probe(VersionProbeOutcome::Cancelled {
                supervision_error: error.clone(),
            });
            assert!(stdout.is_empty());
            assert!(!gate.is_ready());
            assert_eq!(gate.supervision_error(), error.as_deref());
            if error.is_some() {
                gate.observe_before_spawn(|| {
                    panic!("cleanup failure prevents further observation")
                });
            } else {
                gate.observe_before_spawn(|| Ok(None));
            }
            assert_eq!(
                gate,
                StartupGate::Cancelled {
                    supervision_error: error
                }
            );
        }
    }

    #[test]
    fn trace_admission_latches_unknown_length_and_keeps_limits_independent() {
        for duration in [None, Some(Duration::from_millis(17))] {
            for trace_bytes in [None, Some((33, 32))] {
                let mut facts = CaptureLimitFacts::default();
                if let Some(duration) = duration {
                    facts.observe_duration(duration);
                }
                if let Some((observed, limit)) = trace_bytes {
                    facts.observe_trace_bytes(observed, limit);
                }
                let original = facts.clone();
                let expected = if trace_bytes.is_some() {
                    TraceReadPlan::SuppressedByteLimit
                } else {
                    TraceReadPlan::Parse
                };
                assert_eq!(TraceReadPlan::Parse.after_limits(&facts), expected);
                assert_eq!(
                    TraceReadPlan::SuppressedLengthUnverified.after_limits(&facts),
                    TraceReadPlan::SuppressedLengthUnverified
                );
                assert_eq!(
                    facts, original,
                    "admission cannot invent or discard policy facts"
                );
            }
        }
        let mut limits = CaptureLimitFacts::default();
        let plan = TraceReadPlan::SuppressedLengthUnverified;
        assert!(!limits.observe_trace_bytes(32, 32));
        assert_eq!(
            plan.after_limits(&limits),
            plan,
            "a later known length cannot repair a failed observation"
        );
        assert!(limits.observe_trace_bytes(33, 32));
        assert_eq!(plan.after_limits(&limits), plan);
        assert_eq!(limits.trace_bytes_observation(), Some((33, 32)));
    }
}

/// Record one command under the supplied resource policy.
///
/// The backend is never bypassed. A finalized result contains separately
/// reduced target, collector, recorder, limit, and evidence-integrity facts.
///
/// Valid input enters a fresh image at the same PID before storage or child
/// ownership. Only the binary CLI calls this facade; it does not return to an
/// existing host on success. Native environment mutation during snapshot and
/// hostile runtime interference remain outside the supported owner contract.
/// Storage and signal-restoration errors can occur after partial writes or even
/// after finalization; an error is not permission to rerun the target automatically.
pub(crate) fn record(
    root: &Path,
    name: &str,
    command: &[std::ffi::OsString],
    selected: &[String],
    limits: CaptureLimits,
    evidence_contract: RecordingEvidenceContract,
) -> Result<RecordResult> {
    validate_record_input(name, selected, limits, evidence_contract)?;
    match owner::enter(root, name, command, selected, limits, evidence_contract)? {}
}

/// Dispatch the sealed fresh-image request without placing capture policy in main.
pub(crate) fn resume_owner() -> Result<Option<RecordResult>> {
    let Some(mut request) = owner::resume()? else {
        return Ok(None);
    };
    validate_record_input(
        request.name(),
        request.selected(),
        request.limits(),
        request.contract(),
    )?;
    let _environment = request.install_environment()?;
    let mut children = ChildLifecycleScope::new()?;
    let operation = record_owned(
        request.root(),
        request.name(),
        request.command(),
        request.selected(),
        request.limits(),
        request.contract(),
        &children,
    );
    restore_record_result(operation, children.restore()).map(Some)
}

fn validate_record_input(
    name: &str,
    selected: &[String],
    limits: CaptureLimits,
    evidence_contract: RecordingEvidenceContract,
) -> Result<()> {
    // The new saved signal vocabulary is frozen to the supported Linux x86_64
    // ABI. Reject another capture host before creating storage or starting a
    // target; historical recording and platform-independent loading stay intact.
    if evidence_contract == RecordingEvidenceContract::TraceV1
        && !cfg!(all(target_os = "linux", target_arch = "x86_64"))
    {
        bail!("trace-v1 recording requires the supported Linux x86_64 signal contract");
    }
    for key in selected {
        if key.is_empty() || key.contains('=') || key.chars().any(char::is_control) {
            bail!("environment variable name must be nonempty without '=' or control characters");
        }
    }
    if name.is_empty() || name.chars().any(char::is_control) {
        bail!("name must be nonempty without control characters");
    }
    // Validate user policy before opening storage or probing the backend so an
    // impossible monotonic deadline has no filesystem or process side effect.
    limits.validate()?;
    Ok(())
}

fn record_owned(
    root: &Path,
    name: &str,
    command: &[std::ffi::OsString],
    selected: &[String],
    limits: CaptureLimits,
    evidence_contract: RecordingEvidenceContract,
    children: &ChildLifecycleScope,
) -> Result<RecordResult> {
    let environment = selected
        .iter()
        .map(|key| {
            (
                key.clone(),
                std::env::var_os(key).map(|value| bytes(value.as_bytes())),
            )
        })
        .collect();
    require_single_threaded_recorder()?;
    let storage = store::open(root)?;
    storage.check_name_available(name)?;
    let mut signals = SignalScope::for_owner(children)?;
    // Keep the whole workflow inside one result boundary so ordinary errors
    // still reach explicit parent-state restoration. Child guards and the
    // consuming writer independently govern their own resource lifetimes.
    let operation = (|| -> Result<RecordResult> {
        let tracer = std::env::var_os("RUNDELTA_STRACE").unwrap_or_else(|| "strace".into());
        let trace_selector = parse::trace_selector();
        let version = probe_backend(&tracer, &trace_selector, &mut signals)?;
        let (version_stdout, mut startup) = StartupGate::from_probe(version);
        if startup.is_ready() {
            // Close the gap after preflight. Cancellation and failed observation
            // both prevent formal startup, but remain different recorded reasons.
            startup.observe_before_spawn(|| signals.observe());
        }

        // Recheck label reservation under the writer's retained storage lock.
        // Even preflight cancellation gets a record with no invented target or
        // collector status; backend preflight errors above create no record.
        let mut writer = storage.begin_run(name)?;
        let mut m = Metadata {
            schema_version: match evidence_contract {
                RecordingEvidenceContract::StructuredV3 => 3,
                RecordingEvidenceContract::TraceV1 => 4,
            },
            evidence_contract: match evidence_contract {
                RecordingEvidenceContract::StructuredV3 => None,
                RecordingEvidenceContract::TraceV1 => Some("trace-v1".into()),
            },
            target_provenance: None,
            tool_version: env!("CARGO_PKG_VERSION").into(),
            id: writer.id().into(),
            name: name.into(),
            command: command.iter().map(|s| bytes(s.as_bytes())).collect(),
            // Parser context only: this cwd never proves physical path identity
            // for a relative syscall operand lacking per-syscall evidence.
            cwd: bytes(std::env::current_dir()?.as_os_str().as_bytes()),
            started: now(),
            ended: None,
            backend: String::from_utf8_lossy(&version_stdout)
                .lines()
                .next()
                .unwrap_or("strace")
                .into(),
            state: "running".into(),
            exit_code: None,
            signal: None,
            warnings: vec![],
            parsed_events: 0,
            raw_lines: 0,
            environment: Some(environment),
            completeness: Some("pending".into()),
            collector_exit_code: None,
            collector_signal: None,
            target_exit_raw: None,
        };
        writer.write_metadata(&m)?;
        let trace = writer.trace_target()?;

        // Observe again after metadata/procfs I/O so a known cancellation cannot
        // start the target merely because it arrived while storage was initialized.
        startup.observe_before_spawn(|| signals.observe());
        let mut recorder_supervision_error = startup.supervision_error().map(str::to_owned);
        let mut limit_facts = CaptureLimitFacts::default();
        // Failure to establish safe length and an observed excess both forbid
        // parsing below, but only the latter may create a byte-limit fact.
        let mut trace_read_plan = TraceReadPlan::Parse;
        let collector = match startup {
            StartupGate::Cancelled { .. } => CollectorFacts::NotStarted(
                "collector was not started because recorder cancellation was observed".into(),
            ),
            StartupGate::SignalObservationFailed(_) => CollectorFacts::NotStarted(
                "collector was not started because recorder signal supervision failed".into(),
            ),
            StartupGate::Ready => {
                let launch =
                    CollectorLaunch::new(&tracer, &trace_selector, &trace, command, limits);
                let formal = match capture_collector(&mut signals, launch, || writer.trace_len()) {
                    Ok(formal) => formal,
                    Err(error) if is_unquiesced(&error) => {
                        // Initial running metadata/raw bytes remain diagnosable.
                        // Never parse/freeze/publish a possibly live producer.
                        return Err(error).with_context(|| format!("record {} remains unfinalized at {}; raw producer quiescence is not established; do not automatically retry the target", writer.id(), writer.evidence_dir().display()));
                    }
                    Err(error) => return Err(error),
                };
                let (collector, observed_limits, trace_len_error) = formal.into_facts();
                limit_facts = observed_limits;
                if let Some(error) = trace_len_error {
                    trace_read_plan = TraceReadPlan::SuppressedLengthUnverified;
                    append_recorder_supervision_error(&mut recorder_supervision_error, error);
                }
                collector
            }
        };

        if let Some(limit) = limits.max_trace_bytes() {
            // A collector can finish between periodic observations. Check the
            // final anchored length before reading so a fast excess is retained
            // and oversized raw evidence is not normalized after supervision.
            match writer
                .trace_len()
                .context("inspect final anchored trace length before reading raw evidence")
            {
                Ok(observed) => {
                    limit_facts.observe_trace_bytes(observed, limit);
                }
                Err(error) => {
                    trace_read_plan = TraceReadPlan::SuppressedLengthUnverified;
                    append_recorder_supervision_error(
                        &mut recorder_supervision_error,
                        format!(
                            "recorder trace length supervision failed after collector completion: {error:#}"
                        ),
                    );
                }
            }
        }
        let trace_read_plan = trace_read_plan.after_limits(&limit_facts);
        // No raw evidence is rewritten. Skipped/error reads produce an empty
        // normalized trace under the existing all-or-nothing reading contract;
        // the reducer records the independent limitation and process facts.
        let (mut parsed, trace_read_error) = match trace_read_plan {
            TraceReadPlan::SuppressedLengthUnverified | TraceReadPlan::SuppressedByteLimit => {
                (empty_trace_parse(&m.cwd), None)
            }
            TraceReadPlan::Parse => parse_trace_stream(&m.cwd, |parser| {
                writer.for_each_trace_line(|line| {
                    parser.push_line(line);
                    Ok(())
                })
            }),
        };
        if evidence_contract == RecordingEvidenceContract::TraceV1 {
            parsed.bind_task_generations();
        }
        let completion = Completion::from_timeline(&parsed);
        let target_provenance = (evidence_contract == RecordingEvidenceContract::TraceV1)
            .then(|| completion.provenance(&parsed))
            .flatten();
        m.raw_lines = parsed.lines;
        m.parsed_events = parsed.events.len();
        m.ended = Some(now());
        let root_outcome = completion
            .root_exit_index()
            .map(|index| parsed.events[index].outcome.as_str());
        // Freeze once before reducing/publishing facts. Signals arriving during
        // finalization stay pending for parent restoration and do not rewrite
        // this decision or get forwarded to an already reaped collector.
        let recorder = match signals.freeze() {
            Ok(recorder) => recorder,
            Err(error) => {
                let error = format!("recorder signal supervision failed: {error:#}");
                recorder_supervision_error = Some(match recorder_supervision_error {
                    Some(previous) => format!("{previous}; additionally {error}"),
                    None => error,
                });
                signals.recorder_stop()
            }
        };
        let facts = CaptureFactsInput::new(
            root_outcome.map(|raw| decode_target(raw, evidence_contract)),
            completion.missing_exits(),
            collector,
            recorder,
            limit_facts,
            std::mem::take(&mut parsed.warnings),
        )
        .with_observation_errors(recorder_supervision_error, trace_read_error);
        let decision = reduce_capture_facts(facts);
        let state = decision.state();
        let completeness = decision.completeness();
        let code = decision.cli_exit();
        decision.apply_to_metadata(&mut m);
        m.target_provenance = target_provenance;
        let evidence_dir = writer.evidence_dir().display().to_string();
        // finish consumes write authority on success or failure. Its last marker
        // binds the selected evidence contract without upgrading the independent
        // capture completeness. Trace-v1 raw hashes still do not authenticate
        // origin, prove causation, or guarantee power-loss durability.
        writer.finish(&m, &parsed.events)?;
        Ok(RecordResult {
            id: m.id,
            name: m.name,
            state,
            completeness,
            event_count: m.parsed_events,
            warnings: m.warnings,
            evidence_dir,
            cli_exit: code,
        })
    })();
    // Restoration belongs to the recorder boundary, outside persisted capture
    // facts. A committed record can coexist with a tool error here; preserve that
    // distinction instead of presenting the command as safe to retry.
    let restoration = signals.restore_parent();
    restore_record_result(operation, restoration)
}

fn restore_record_result(
    operation: Result<RecordResult>,
    restoration: Result<()>,
) -> Result<RecordResult> {
    match (operation, restoration) {
        (Ok(result), Ok(())) => Ok(result),
        (Ok(result), Err(error)) => bail!(
            "record {} committed but failed to restore recorder signal state: {error:#}",
            result.id
        ),
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(restoration)) => Err(anyhow::anyhow!(
            "{error:#}; additionally failed to restore recorder signal state: {restoration:#}"
        )),
    }
}

#[cfg(test)]
mod streaming_trace_tests {
    use super::*;

    #[test]
    fn streamed_lines_preserve_adapter_lf_crlf_eof_and_warning_semantics() {
        let raw = concat!(
            "7 execve(\"/tool\", [\"tool\"], 0x0) = 0\r\n",
            "7 open(\"relative\", O_RDONLY) = -1 ENOENT (No file)\n",
            "7 +++ exited with 0 +++\n",
            "9 open(\"/unfinished\", O_RDONLY <unfinished ...>"
        );
        let expected = parse::parse_with_cwd(raw, "raw/trace.log", Some("/context"));
        let (actual, error) = parse_trace_stream("/context", |parser| {
            for line in raw.split_inclusive('\n') {
                parser.push_line(line);
            }
            Ok(())
        });

        assert_eq!(error, None);
        assert_eq!(actual.lines, expected.lines);
        assert_eq!(actual.warnings, expected.warnings);
        assert_eq!(
            serde_json::to_value(&actual.events).unwrap(),
            serde_json::to_value(&expected.events).unwrap()
        );
        let actual_completion = Completion::from_timeline(&actual);
        let expected_completion = Completion::from_timeline(&expected);
        assert_eq!(
            actual_completion.root_exit_index(),
            expected_completion.root_exit_index()
        );
        assert_eq!(
            actual_completion.missing_exits(),
            expected_completion.missing_exits()
        );
    }

    #[test]
    fn late_stream_error_discards_every_accepted_prefix_fact() {
        let (parsed, error) = parse_trace_stream("/context", |parser| {
            parser.push_line("7 execve(\"/tool\", [\"tool\"], 0x0) = 0\n");
            parser.push_line("7 +++ exited with 0 +++\n");
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "late injected trace read failure",
            ))
        });

        assert_eq!(parsed.lines, 0);
        assert!(parsed.events.is_empty());
        assert!(parsed.warnings.is_empty());
        assert!(parsed.timeline.generations().next().is_none());
        assert!(
            Completion::from_timeline(&parsed)
                .provenance(&parsed)
                .is_none()
        );
        assert_eq!(error.as_deref(), Some("late injected trace read failure"));

        let decision = reduce_capture(
            None,
            &[],
            CollectorFacts::Reaped(ProcessExit::Exited(0)),
            RecorderStop::None,
            None,
            vec![],
            error,
        );
        assert_eq!(decision.target(), None);
        assert_eq!(decision.state(), CaptureState::CaptureFailed);
        assert_eq!(decision.completeness(), CaptureCompleteness::Incomplete);
        assert_eq!(decision.cli_exit(), 125);
        assert_eq!(
            decision.warnings(),
            [
                "cannot read trace: late injected trace read failure",
                "root process completion not observed; capture incomplete",
            ]
        );
    }
}

#[cfg(test)]
mod reduction_boundary_tests {
    use super::*;

    fn legacy_successful_reduction(
        root_outcome: Option<&str>,
        missing_generations: &[String],
        collector_exit: ProcessExit,
        recorder: RecorderStop,
        mut warnings: Vec<String>,
        trace_read_error: Option<String>,
    ) -> DecisionSnapshot {
        if let Some(error) = trace_read_error {
            warnings.push(format!("cannot read trace: {error}"));
        }

        let mut exit_code = None;
        let mut signal = None;
        let target_exit_raw = root_outcome.map(str::to_owned);
        if let Some(outcome) = root_outcome {
            if let Some(code) = outcome
                .strip_prefix("+++ exited with ")
                .and_then(|value| value.strip_suffix(" +++"))
                .and_then(|value| value.parse::<i32>().ok())
            {
                exit_code = Some(code);
            } else if outcome.starts_with("+++ killed by ") {
                signal = signal_number(outcome);
                if signal.is_none() {
                    warnings.push("unrecognized target termination signal".into());
                }
            }
        }
        let target = root_outcome.map(|_| match (exit_code, signal) {
            (Some(code), None) => ProcessExit::Exited(code),
            (None, Some(signal)) => ProcessExit::Signaled(signal),
            _ => ProcessExit::Unknown,
        });
        if !missing_generations.is_empty() {
            warnings.push(format!(
                "process completion not observed for PIDs: {}",
                missing_generations.join(", ")
            ));
        }

        let confirmation = if let Some(code) = exit_code {
            Some(collector_exit == ProcessExit::Exited(code))
        } else {
            signal.map(|signal| collector_exit == ProcessExit::Signaled(signal))
        };
        let state = if matches!(recorder, RecorderStop::Signal(_)) {
            CaptureState::Interrupted
        } else if root_outcome.is_none() || confirmation == Some(false) {
            CaptureState::CaptureFailed
        } else {
            CaptureState::Completed
        };
        if root_outcome.is_none() {
            warnings.push("root process completion not observed; capture incomplete".into());
        }
        if confirmation == Some(false) {
            warnings.push("collector status does not confirm target completion".into());
        }
        let completeness = if state != CaptureState::Completed {
            CaptureCompleteness::Incomplete
        } else if warnings.is_empty() {
            CaptureCompleteness::CompleteForSupportedEvents
        } else {
            CaptureCompleteness::Partial
        };
        let target_code = signal.map(|signal| 128 + signal).or(exit_code);
        let collector_code = collector_exit.cli_exit();
        let cli_exit = match recorder {
            RecorderStop::Signal(signal) => 128 + signal,
            RecorderStop::None if state == CaptureState::CaptureFailed => 125,
            RecorderStop::None => target_code.or(collector_code).unwrap_or(125),
        };

        DecisionSnapshot {
            target,
            target_exit_raw,
            collector: CollectorFacts::Reaped(collector_exit),
            recorder,
            limits: CaptureLimitFacts::default(),
            recorder_supervision_error: None,
            state,
            completeness,
            warnings,
            cli_exit,
        }
    }

    #[test]
    fn known_collector_fact_matrix_matches_the_legacy_reduction() {
        // A compatibility oracle: matching the old reducer freezes behavior,
        // not its independent correctness. Some pure-input combinations (such
        // as a root fact plus a whole-trace read error) are unreachable in record().
        let roots = [
            None,
            Some("+++ exited with 0 +++"),
            Some("+++ exited with 7 +++"),
            Some("+++ killed by SIGTERM +++"),
            Some("+++ killed by SIGFUTURE +++"),
        ];
        let collectors = [
            ProcessExit::Exited(0),
            ProcessExit::Exited(7),
            ProcessExit::Signaled(libc::SIGTERM),
        ];
        let recorders = [RecorderStop::None, RecorderStop::Signal(libc::SIGINT)];
        let warning_sets = [Vec::new(), vec!["parser warning".into()]];
        let missing_sets = [Vec::new(), vec!["2".into(), "9".into()]];
        let trace_errors = [None, Some("trace read failed")];

        for root in roots {
            for collector in collectors {
                for recorder in recorders {
                    for warnings in &warning_sets {
                        for missing in &missing_sets {
                            for trace_error in trace_errors {
                                let expected = legacy_successful_reduction(
                                    root,
                                    missing,
                                    collector,
                                    recorder,
                                    warnings.clone(),
                                    trace_error.map(str::to_owned),
                                );
                                let actual = reduce_capture(
                                    root,
                                    missing,
                                    CollectorFacts::Reaped(collector),
                                    recorder,
                                    None,
                                    warnings.clone(),
                                    trace_error.map(str::to_owned),
                                );
                                assert_eq!(
                                    actual.snapshot(),
                                    expected,
                                    "root={root:?}, collector={collector:?}, recorder={recorder:?}, warnings={warnings:?}, missing={missing:?}, trace_error={trace_error:?}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn unknown_collector_fact_matrix_fails_closed() {
        let roots = [
            None,
            Some("+++ exited with 0 +++"),
            Some("+++ killed by SIGFUTURE +++"),
        ];
        let recorders = [RecorderStop::None, RecorderStop::Signal(libc::SIGINT)];
        let warning_sets = [Vec::new(), vec!["parser warning".into()]];
        let missing_sets = [Vec::new(), vec!["2".into()]];
        let trace_errors = [None, Some("trace read failed")];

        for root in roots {
            for recorder in recorders {
                for warnings in &warning_sets {
                    for missing in &missing_sets {
                        for trace_error in trace_errors {
                            let decision = reduce_capture(
                                root,
                                missing,
                                CollectorFacts::Reaped(ProcessExit::Unknown),
                                recorder,
                                None,
                                warnings.clone(),
                                trace_error.map(str::to_owned),
                            );
                            assert_eq!(decision.target_exit_raw(), root);
                            assert_eq!(
                                decision.snapshot().collector,
                                CollectorFacts::Reaped(ProcessExit::Unknown)
                            );
                            assert!(decision.warnings().iter().any(|warning| {
                                warning == "collector status does not confirm target completion"
                            }));
                            match recorder {
                                RecorderStop::None => {
                                    assert_eq!(decision.state(), CaptureState::CaptureFailed);
                                    assert_eq!(decision.cli_exit(), 125);
                                }
                                RecorderStop::Signal(signal) => {
                                    assert_eq!(decision.state(), CaptureState::Interrupted);
                                    assert_eq!(decision.cli_exit(), 128 + signal);
                                }
                            }
                            assert_eq!(decision.completeness(), CaptureCompleteness::Incomplete);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn unrepresentable_monotonic_deadlines_fail_before_spawn() {
        let limits =
            CaptureLimits::from_durations(Some(Duration::MAX), None, Duration::from_secs(2));
        let error = supervision::validate_formal_schedule(limits)
            .expect_err("Duration::MAX must not fit in Instant");
        assert!(
            format!("{error:#}").contains("cannot be represented by the monotonic clock"),
            "{error:#}"
        );

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let base = std::env::current_dir()
            .expect("current directory")
            .join("target")
            .join(format!(
                "invalid-capture-limit-{}-{unique}",
                std::process::id()
            ));
        let storage = base.join("storage");
        let sentinel = base.join("target-ran");
        let command = [
            std::ffi::OsString::from("sh"),
            std::ffi::OsString::from("-c"),
            std::ffi::OsString::from("touch \"$1\""),
            std::ffi::OsString::from("sh"),
            sentinel.as_os_str().to_owned(),
        ];
        let error = record(
            &storage,
            "invalid-limit",
            &command,
            &[],
            limits,
            RecordingEvidenceContract::StructuredV3,
        )
        .expect_err("invalid policy must fail before storage or target side effects");
        assert!(
            format!("{error:#}").contains("cannot be represented by the monotonic clock"),
            "{error:#}"
        );
        assert!(!sentinel.exists(), "target ran with an invalid limit");
        assert!(!storage.exists(), "storage was opened for an invalid limit");
    }
}

#[cfg(test)]
mod completion_reduction_tests {
    use super::*;

    fn completion(trace: &str) -> (parse::Parsed, Completion) {
        let parsed = parse::parse_with_cwd(trace, "fixture", Some("/"));
        let completion = Completion::from_timeline(&parsed);
        (parsed, completion)
    }

    #[test]
    fn provenance_does_not_borrow_missing_root_completion_from_other_generations() {
        for raw in [
            concat!(
                "20 execve(\"/root\", [], 0) = 0\n",
                "20 fork() = 3\n",
                "3 +++ exited with 0 +++\n",
            ),
            concat!(
                "20 execve(\"/root\", [], 0) = 0\n",
                "20 fork() = 20\n",
                "20 +++ exited with 0 +++\n",
            ),
            concat!(
                "20 execve(\"/missing\", [], 0) = -1 ENOENT (No file)\n",
                "20 +++ exited with 0 +++\n",
            ),
        ] {
            let (mut parsed, actual) = completion(raw);
            parsed.bind_task_generations();
            assert!(actual.provenance(&parsed).is_none(), "{raw}");
            let decision = reduce_capture(
                None,
                actual.missing_exits(),
                CollectorFacts::Reaped(ProcessExit::Exited(0)),
                RecorderStop::None,
                None,
                parsed.warnings.clone(),
                None,
            );
            assert_eq!(decision.target_exit_raw(), None);
            assert_eq!(decision.target(), None);
            assert_eq!(decision.state(), CaptureState::CaptureFailed);
        }
    }

    #[test]
    fn provenance_keeps_unknown_signal_and_noninitial_root_generation_observable() {
        let (mut parsed, actual) = completion(concat!(
            "20 +++ exited with 0 +++\n",
            "20 execve(\"/root\", [], 0) = 0\n",
            "20 +++ killed by SIG_FUTURE +++\n",
        ));
        parsed.bind_task_generations();
        let provenance = actual.provenance(&parsed).expect("raw unknown signal exit");
        assert_eq!(provenance.root.generation, 2);
        assert_eq!(provenance.root_exec.index, 1);
        assert_eq!(provenance.target_exit.index, 2);
        assert!(parsed.events[1].context_uncertain);
        assert!(parsed.events[2].context_uncertain);
        let outcome = &parsed.events[provenance.target_exit.index].outcome;
        let decision = reduce_capture(
            Some(outcome),
            actual.missing_exits(),
            CollectorFacts::Reaped(ProcessExit::Exited(0)),
            RecorderStop::None,
            None,
            parsed.warnings.clone(),
            None,
        );
        assert_eq!(decision.target(), Some(ProcessExit::Unknown));
        assert_eq!(
            decision.target_exit_raw(),
            Some("+++ killed by SIG_FUTURE +++")
        );
        assert_eq!(decision.completeness(), CaptureCompleteness::Partial);
        assert!(
            decision
                .warnings()
                .iter()
                .any(|warning| warning == "unrecognized target termination signal")
        );
    }
}
