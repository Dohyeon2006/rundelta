//! One-record capture orchestration at the Linux process boundary.
//!
//! This module owns backend probing, signal and pidfd supervision, collector
//! lifetime, raw-trace limits, incremental parsing, and reduction of observed
//! target/collector/recorder facts. It may depend on the pure parser, storage
//! façade, and domain model; none of those modules depend on capture. Storage
//! and output failures remain outside the capture-fact reducer, and persisted
//! metadata stays on schema v3.

use crate::{
    model::{CaptureCompleteness, CaptureState, Metadata, RecordResult, bytes},
    parse, store,
};
use anyhow::{Context, Result, bail};
use std::{
    io::{self, Read},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    os::unix::{
        ffi::OsStrExt,
        process::{CommandExt, ExitStatusExt},
    },
    path::Path,
    process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

// RF-630 owns removal of the whole-input compatibility adapters. Keep their
// signatures type-checked during this streaming stability window without
// calling them on the production path.
const _: fn(&str, &str, Option<&str>) -> parse::Parsed = parse::parse_with_cwd;
const _: fn(&mut store::RunWriter) -> io::Result<String> = store::RunWriter::read_trace;

fn now() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

/// Resource policy for one recorded command.
///
/// Limits are recorder policy only and are deliberately not persisted as new
/// metadata fields. A zero duration or byte limit is a valid immediate
/// boundary; trace bytes trigger only after the anchored file grows beyond the
/// configured value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CaptureLimits {
    /// Maximum time spent supervising the formal collector, excluding the
    /// backend version probe.
    max_duration: Option<Duration>,
    /// Soft upper bound for the anchored raw trace inode.
    max_trace_bytes: Option<u64>,
    /// Grace between TERM and KILL during supervised termination.
    terminate_grace: Duration,
}

impl CaptureLimits {
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

    fn validate(self) -> Result<()> {
        self.validate_at(Instant::now())
    }

    fn validate_at(self, anchor: Instant) -> Result<()> {
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

/// A process completion observed at one capture boundary.
///
/// `Unknown` keeps an observed raw target completion distinct from a missing
/// completion and from a collector/supervision failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessExit {
    Exited(i32),
    Signaled(i32),
    Unknown,
}

impl ProcessExit {
    fn cli_exit(self) -> Option<i32> {
        match self {
            Self::Exited(code) => Some(code),
            Self::Signaled(signal) => Some(128 + signal),
            Self::Unknown => None,
        }
    }

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
enum CollectorFacts {
    Reaped(ProcessExit),
    NotStarted(String),
    Failed(String),
}

/// Recorder control-signal observation, kept separate from process outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecorderStop {
    None,
    Signal(i32),
}

/// Independently observed policy-limit facts.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct CaptureLimitFacts {
    duration: Option<Duration>,
    trace_bytes: Option<TraceBytesLimitFact>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TraceBytesLimitFact {
    observed: u64,
    limit: u64,
}

impl CaptureLimitFacts {
    fn exceeded(&self) -> bool {
        self.duration.is_some() || self.trace_bytes.is_some()
    }

    fn observe_duration(&mut self, limit: Duration) -> bool {
        if self.duration.is_some() {
            false
        } else {
            self.duration = Some(limit);
            true
        }
    }

    fn observe_trace_bytes(&mut self, observed: u64, limit: u64) -> bool {
        if observed <= limit {
            return false;
        }
        let first = self.trace_bytes.is_none();
        self.trace_bytes = Some(TraceBytesLimitFact { observed, limit });
        first
    }

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

/// Pure capture-policy result before persistence and CLI presentation.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CaptureDecision {
    target: Option<ProcessExit>,
    target_exit_raw: Option<String>,
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
    /// Project the already-reduced facts to the frozen metadata v3 fields.
    fn apply_to_metadata(self, metadata: &mut Metadata) {
        debug_assert_eq!(
            matches!(self.recorder, RecorderStop::Signal(_)),
            self.state == CaptureState::Interrupted
        );
        debug_assert!(!self.limits.exceeded() || self.state != CaptureState::Completed);
        (metadata.exit_code, metadata.signal) = self
            .target
            .map(ProcessExit::metadata_fields)
            .unwrap_or((None, None));
        (metadata.collector_exit_code, metadata.collector_signal) = match self.collector {
            CollectorFacts::Reaped(exit) => exit.metadata_fields(),
            CollectorFacts::NotStarted(_) | CollectorFacts::Failed(_) => (None, None),
        };
        metadata.target_exit_raw = self.target_exit_raw;
        metadata.state = self.state.to_string();
        metadata.completeness = Some(self.completeness.to_string());
        metadata.warnings = self.warnings;
    }
}

fn parse_process_exit(outcome: &str) -> ProcessExit {
    if let Some(code) = outcome
        .strip_prefix("+++ exited with ")
        .and_then(|value| value.strip_suffix(" +++"))
        .and_then(|value| value.parse::<i32>().ok())
    {
        ProcessExit::Exited(code)
    } else if outcome.starts_with("+++ killed by ") {
        signal_number(outcome)
            .map(ProcessExit::Signaled)
            .unwrap_or(ProcessExit::Unknown)
    } else {
        ProcessExit::Unknown
    }
}

fn process_exit_from_status(status: &std::process::ExitStatus) -> ProcessExit {
    if let Some(code) = status.code() {
        ProcessExit::Exited(code)
    } else if let Some(signal) = status.signal() {
        ProcessExit::Signaled(signal)
    } else {
        ProcessExit::Unknown
    }
}

struct CaptureFactsInput<'a> {
    root_outcome: Option<&'a str>,
    missing_generations: &'a [String],
    collector: CollectorFacts,
    recorder: RecorderStop,
    limits: CaptureLimitFacts,
    recorder_supervision_error: Option<String>,
    warnings: Vec<String>,
    trace_read_error: Option<String>,
}

fn reduce_capture_facts(input: CaptureFactsInput<'_>) -> CaptureDecision {
    let CaptureFactsInput {
        root_outcome,
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

    let target_exit_raw = root_outcome.map(str::to_owned);
    let target = root_outcome.map(parse_process_exit);
    if root_outcome.is_some_and(|outcome| outcome.starts_with("+++ killed by "))
        && target == Some(ProcessExit::Unknown)
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
            let confirmation = match target {
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
        RecorderStop::None => target
            .and_then(ProcessExit::cli_exit)
            .or_else(|| collector_exit.and_then(ProcessExit::cli_exit))
            .unwrap_or(125),
    };

    CaptureDecision {
        target,
        target_exit_raw,
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
fn reduce_capture(
    root_outcome: Option<&str>,
    missing_generations: &[String],
    collector: CollectorFacts,
    recorder: RecorderStop,
    recorder_supervision_error: Option<String>,
    warnings: Vec<String>,
    trace_read_error: Option<String>,
) -> CaptureDecision {
    reduce_capture_facts(CaptureFactsInput {
        root_outcome,
        missing_generations,
        collector,
        recorder,
        limits: CaptureLimitFacts::default(),
        recorder_supervision_error,
        warnings,
        trace_read_error,
    })
}

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

#[derive(Clone, Copy, Default)]
struct CollectorFaults {
    #[cfg(test)]
    pidfd: bool,
    #[cfg(test)]
    pidfd_errno: Option<i32>,
    #[cfg(test)]
    wait: bool,
    #[cfg(test)]
    wait_errno: Option<i32>,
    #[cfg(test)]
    kill: bool,
    #[cfg(test)]
    reap: bool,
}

/// Proof checked immediately before spawning a direct child in this
/// single-threaded supervisor. A default SIGCHLD disposition without
/// SA_NOCLDWAIT retains exited children until this owner reaps them; an ignored
/// signal, automatic reaping, or an inherited handler cannot provide that
/// guarantee. The check does not change what the target inherits.
struct ChildWaitability;

impl ChildWaitability {
    fn before_spawn() -> Result<Self> {
        let mask = current_signal_mask().context("inspect child reaping signal mask")?;
        let saved = saved_signal(libc::SIGCHLD, &mask)?;
        Self::from_action(&saved.action)
    }

    fn from_action(action: &libc::sigaction) -> Result<Self> {
        if action.sa_sigaction != libc::SIG_DFL || action.sa_flags & libc::SA_NOCLDWAIT != 0 {
            bail!(
                "SIGCHLD disposition cannot safely supervise child lifetimes: requires SIG_DFL without SA_NOCLDWAIT"
            );
        }
        Ok(Self)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChildOwnership {
    /// No other reaper or auto-reaping signal disposition can release the PID.
    Waitable,
    /// A successful wait consumed the owned child.
    Reaped,
    /// ESRCH/ECHILD disproved ownership; neither cleanup nor Drop may signal it.
    Lost,
}

/// Owns one spawned collector until it has been explicitly reaped.
///
/// `pidfd` is `None` only while `attach` is acquiring the safe handle. A
/// successfully returned guard always owns both the child and its pidfd.
struct CollectorGuard {
    child: Child,
    pidfd: Option<OwnedFd>,
    ownership: ChildOwnership,
    #[cfg(test)]
    faults: CollectorFaults,
}

impl CollectorGuard {
    fn attach(
        child: Child,
        _waitability: ChildWaitability,
        faults: CollectorFaults,
    ) -> Result<Self> {
        let mut guard = Self {
            child,
            pidfd: None,
            ownership: ChildOwnership::Waitable,
            #[cfg(test)]
            faults,
        };
        #[cfg(not(test))]
        let _ = faults;

        let pidfd = match guard.open_pidfd() {
            Ok(pidfd) => pidfd,
            Err(error) => {
                if error.raw_os_error() == Some(libc::ESRCH) {
                    // The kernel no longer identifies this PID as our child.
                    // In particular, do not turn failed pidfd acquisition into
                    // a numeric-PID signal after ownership has been disproved.
                    guard.ownership = ChildOwnership::Lost;
                }
                let error = anyhow::Error::new(error).context("pidfd_open for collector required");
                return Err(guard.abort(error));
            }
        };
        guard.pidfd = Some(pidfd);
        Ok(guard)
    }

    fn open_pidfd(&self) -> std::io::Result<OwnedFd> {
        #[cfg(test)]
        if let Some(errno) = self.faults.pidfd_errno {
            return Err(std::io::Error::from_raw_os_error(errno));
        }
        #[cfg(test)]
        if self.faults.pidfd {
            return Err(injected_collector_error("pidfd"));
        }
        pidfd(self.child.id() as i32)
    }

    fn finish(mut self) -> Result<ExitStatus> {
        match self.wait_normally() {
            Ok(status) => Ok(status),
            Err(error) => Err(with_cleanup_error(error, self.terminate_and_reap())),
        }
    }

    fn abort(mut self, error: anyhow::Error) -> anyhow::Error {
        with_cleanup_error(error, self.terminate_and_reap())
    }

    fn wait_normally(&mut self) -> Result<ExitStatus> {
        #[cfg(test)]
        if let Some(errno) = self.faults.wait_errno {
            return self
                .observe_wait(Err(std::io::Error::from_raw_os_error(errno)))
                .context("failed waiting for collector");
        }
        #[cfg(test)]
        if self.faults.wait {
            bail!(injected_collector_error("wait"));
        }
        let waited = self.child.wait();
        self.observe_wait(waited)
            .context("failed waiting for collector")
    }

    fn observe_wait(&mut self, result: io::Result<ExitStatus>) -> io::Result<ExitStatus> {
        match &result {
            Ok(_) => self.ownership = ChildOwnership::Reaped,
            Err(error) if error.raw_os_error() == Some(libc::ECHILD) => {
                self.ownership = ChildOwnership::Lost;
            }
            Err(_) => {}
        }
        result
    }

    fn terminate_and_reap(&mut self) -> Result<()> {
        if self.ownership != ChildOwnership::Waitable {
            return Ok(());
        }
        self.terminate_and_reap_status().map(|_| ())
    }

    fn terminate_and_reap_status(&mut self) -> Result<ExitStatus> {
        if self.ownership != ChildOwnership::Waitable {
            bail!("collector child ownership is no longer available for termination/reaping");
        }
        let terminate_error = self.terminate().err();
        let reap_result = self.reap_after_termination();
        match (terminate_error, reap_result) {
            (None, Ok(status)) => Ok(status),
            (Some(error), Ok(_)) | (None, Err(error)) => Err(error),
            (Some(terminate), Err(reap)) => Err(anyhow::anyhow!(
                "{terminate:#}; additionally failed to reap collector: {reap:#}"
            )),
        }
    }

    fn terminate(&mut self) -> Result<()> {
        if self.ownership != ChildOwnership::Waitable {
            return Ok(());
        }
        let result = if let Some(pidfd) = &self.pidfd {
            send_signal(pidfd, libc::SIGKILL)
        } else {
            // ChildWaitability was checked before spawn, no competing reaper
            // exists in this single-threaded supervisor, and neither ESRCH nor
            // ECHILD has invalidated ownership. The unreaped child therefore
            // retains this PID even when pidfd_open failed for another reason.
            self.child.kill()
        };
        if let Err(error) = result
            && error.raw_os_error() != Some(libc::ESRCH)
        {
            return Err(error).context("failed to terminate collector");
        }
        // Test faults are reported only after the real destructive operation,
        // so exercising an error path never intentionally leaves a process.
        #[cfg(test)]
        if self.faults.kill {
            bail!(injected_collector_error("kill"));
        }
        Ok(())
    }

    fn reap_after_termination(&mut self) -> Result<ExitStatus> {
        // There is no second userspace deadline after SIGKILL. Reaping the
        // direct child relies on Linux eventually making that task waitable;
        // an uninterruptible kernel wait can therefore delay cleanup.
        let waited = self.child.wait();
        let status = self
            .observe_wait(waited)
            .context("failed to reap collector after termination")?;
        // As above, inject after the real reap to keep the seam leak-free.
        #[cfg(test)]
        if self.faults.reap {
            bail!(injected_collector_error("reap"));
        }
        Ok(status)
    }

    fn pidfd_raw(&self) -> Result<i32> {
        Ok(self
            .pidfd
            .as_ref()
            .context("collector pidfd missing after attachment")?
            .as_raw_fd())
    }

    fn take_stdout(&mut self) -> Result<ChildStdout> {
        self.child
            .stdout
            .take()
            .context("supervised child stdout pipe missing")
    }

    fn take_stderr(&mut self) -> Result<ChildStderr> {
        self.child
            .stderr
            .take()
            .context("supervised child stderr pipe missing")
    }

    fn forward(&self, signal: i32) -> Result<()> {
        let pidfd = self
            .pidfd
            .as_ref()
            .context("collector pidfd missing after attachment")?;
        match send_signal(pidfd, signal) {
            Ok(()) => Ok(()),
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => Ok(()),
            Err(error) => Err(error).context("failed to signal collector"),
        }
    }

    fn cancel(mut self) -> Result<ExitStatus> {
        self.terminate_and_reap_status()
    }
}

impl Drop for CollectorGuard {
    fn drop(&mut self) {
        if self.ownership == ChildOwnership::Waitable {
            let _ = self.terminate_and_reap();
        }
    }
}

fn with_cleanup_error(primary: anyhow::Error, cleanup: Result<()>) -> anyhow::Error {
    match cleanup {
        Ok(()) => primary,
        Err(cleanup) => {
            anyhow::anyhow!("{primary:#}; collector termination/reap also failed: {cleanup:#}")
        }
    }
}

#[cfg(test)]
fn injected_collector_error(operation: &str) -> std::io::Error {
    std::io::Error::other(format!("injected collector {operation} failure"))
}

const CONTROL_SIGNALS: [i32; 3] = [libc::SIGHUP, libc::SIGINT, libc::SIGTERM];
const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const VERSION_STREAM_LIMIT: usize = 64 * 1024;
const VERSION_ERROR_DISPLAY_LIMIT: usize = 4 * 1024;
const TRACE_LIMIT_POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Clone, Copy)]
struct SavedSignal {
    number: i32,
    action: libc::sigaction,
    blocked: bool,
}

#[derive(Clone, Copy)]
struct ChildSignalState {
    signals: [SavedSignal; CONTROL_SIGNALS.len()],
    mask: libc::sigset_t,
}

impl ChildSignalState {
    fn restore(self) -> std::io::Result<()> {
        for saved in self.signals {
            set_signal_action(saved.number, &saved.action)?;
        }
        set_signal_mask(&self.mask)
    }
}

fn configure_supervised_child(
    command: &mut Command,
    child_signals: ChildSignalState,
    parent: libc::pid_t,
) {
    // Command::spawn synchronously waits for Rust's exec-error handshake. The
    // poll deadline begins only after spawn returns, so a kernel/runtime defect
    // that wedges that handshake remains a deliberately narrow unsupervised
    // window. Every ordinary post-spawn path is pidfd-supervised. Linux clears
    // PDEATHSIG across a later privileged exec, which is outside the trusted,
    // non-daemonizing strace backend contract.
    // SAFETY: the post-fork closure captures only copied signal/PID values and
    // invokes signal-state syscalls, prctl and getppid; it does not allocate,
    // lock, or access parent-thread state. Its error paths construct only OS
    // errors. Restored sigaction values and masks came from successful queries,
    // and prctl receives the documented scalar PR_SET_PDEATHSIG arguments.
    unsafe {
        command.pre_exec(move || {
            child_signals.restore()?;
            if libc::prctl(
                libc::PR_SET_PDEATHSIG,
                libc::SIGKILL as libc::c_ulong,
                0,
                0,
                0,
            ) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                return Err(std::io::Error::from_raw_os_error(libc::ECHILD));
            }
            Ok(())
        });
    }
}

#[derive(Clone, Copy, Default)]
struct SignalFaults {
    #[cfg(test)]
    action_index: Option<usize>,
    #[cfg(test)]
    signalfd: bool,
    #[cfg(test)]
    poll: bool,
    #[cfg(test)]
    restore_action_once: bool,
    #[cfg(test)]
    restore_mask_once: bool,
}

/// Owns the recorder's temporary signal mask, dispositions, and signalfd.
///
/// Watched signals are blocked before inherited `SIG_IGN` or handlers are
/// replaced with `SIG_DFL`. That makes all three signals observable through
/// signalfd without running an asynchronous handler. Parent and child restore
/// the exact saved state at their respective boundaries. After capture facts
/// freeze, late signals stay pending for the recorder's original disposition;
/// they are not target outcome facts and must not be forwarded to a former child.
struct SignalScope {
    fd: OwnedFd,
    saved: [SavedSignal; CONTROL_SIGNALS.len()],
    original_mask: libc::sigset_t,
    first_observed: Option<i32>,
    frozen: bool,
    restored: bool,
    #[cfg(test)]
    faults: SignalFaults,
    #[cfg(test)]
    restore_attempts: usize,
}

impl SignalScope {
    fn new() -> Result<Self> {
        Self::with_faults(SignalFaults::default())
    }

    fn with_faults(faults: SignalFaults) -> Result<Self> {
        #[cfg(not(test))]
        let _ = faults;
        let original_mask = current_signal_mask().context("read recorder signal mask")?;
        let saved = [
            saved_signal(libc::SIGHUP, &original_mask)?,
            saved_signal(libc::SIGINT, &original_mask)?,
            saved_signal(libc::SIGTERM, &original_mask)?,
        ];
        let watched = control_signal_set()?;
        let observable = default_signal_action()?;
        block_signal_set(&watched).context("block recorder control signals")?;

        let mut changed = 0usize;
        for index in 0..saved.len() {
            #[cfg(test)]
            if faults.action_index == Some(index) {
                let error = anyhow::Error::new(injected_signal_error("sigaction"));
                return Err(with_signal_setup_rollback(
                    error,
                    &saved,
                    changed,
                    &original_mask,
                ));
            }
            if let Err(error) = set_signal_action(saved[index].number, &observable) {
                return Err(with_signal_setup_rollback(
                    anyhow::Error::new(error).context("make control signal observable"),
                    &saved,
                    changed,
                    &original_mask,
                ));
            }
            changed += 1;
        }

        #[cfg(test)]
        if faults.signalfd {
            let error = anyhow::Error::new(injected_signal_error("signalfd"));
            return Err(with_signal_setup_rollback(
                error,
                &saved,
                changed,
                &original_mask,
            ));
        }
        // SAFETY: `watched` is an initialized sigset_t that remains live for
        // the call. A negative fd requests a new descriptor with valid flags;
        // watched signals have already been blocked on this recorder thread.
        let descriptor =
            unsafe { libc::signalfd(-1, &watched, libc::SFD_NONBLOCK | libc::SFD_CLOEXEC) };
        if descriptor < 0 {
            return Err(with_signal_setup_rollback(
                anyhow::Error::new(std::io::Error::last_os_error())
                    .context("create recorder signalfd"),
                &saved,
                changed,
                &original_mask,
            ));
        }

        Ok(Self {
            // SAFETY: successful signalfd returned a new, valid descriptor;
            // no other owner exists, and ownership transfers exactly once.
            fd: unsafe { OwnedFd::from_raw_fd(descriptor) },
            saved,
            original_mask,
            first_observed: None,
            frozen: false,
            restored: false,
            #[cfg(test)]
            faults,
            #[cfg(test)]
            restore_attempts: 0,
        })
    }

    fn child_state(&self) -> ChildSignalState {
        ChildSignalState {
            signals: self.saved,
            mask: self.original_mask,
        }
    }

    fn observe(&mut self) -> Result<Option<i32>> {
        // Reading signalfd consumes pending signals. Once capture facts are
        // frozen (also during restoration retries), leave new signals pending
        // so restoring the saved action and mask preserves caller semantics.
        if self.frozen {
            return Ok(self.first_observed);
        }
        loop {
            let mut information = std::mem::MaybeUninit::<libc::signalfd_siginfo>::uninit();
            // SAFETY: the owned signalfd remains open, and the destination has
            // space/alignment for exactly the requested structure size. read
            // may initialize only a prefix, checked before any field access.
            let read = unsafe {
                libc::read(
                    self.fd.as_raw_fd(),
                    information.as_mut_ptr().cast(),
                    std::mem::size_of::<libc::signalfd_siginfo>(),
                )
            };
            if read == std::mem::size_of::<libc::signalfd_siginfo>() as isize {
                // SAFETY: the preceding successful read initialized every
                // byte of signalfd_siginfo, whose fields are integer types.
                let signal = unsafe { information.assume_init() }.ssi_signo as i32;
                if self.first_observed.is_none() {
                    self.first_observed = Some(signal);
                }
                continue;
            }
            if read < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                if error.kind() == std::io::ErrorKind::WouldBlock {
                    break;
                }
                return Err(error).context("read recorder signalfd");
            }
            bail!("short read from recorder signalfd: {read} bytes");
        }
        Ok(self.first_observed)
    }

    fn recorder_stop(&self) -> RecorderStop {
        self.first_observed
            .map(RecorderStop::Signal)
            .unwrap_or(RecorderStop::None)
    }

    fn requires_direct_termination(&self, signal: i32) -> bool {
        self.saved
            .iter()
            .find(|saved| saved.number == signal)
            .is_some_and(|saved| saved.blocked || saved.action.sa_sigaction == libc::SIG_IGN)
    }

    fn freeze(&mut self) -> Result<RecorderStop> {
        let observation = self.observe();
        // Even a failed last observation ends capture's ownership of signals.
        // Cleanup must not silently consume a signal on a later retry.
        self.frozen = true;
        observation?;
        Ok(self.recorder_stop())
    }

    fn poll(&self, descriptors: &mut [libc::pollfd], deadline: Option<Instant>) -> Result<i32> {
        #[cfg(test)]
        if self.faults.poll {
            bail!(injected_signal_error("poll"));
        }
        loop {
            let timeout = deadline.map_or(-1, |deadline| {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    0
                } else {
                    remaining
                        .as_millis()
                        .saturating_add(1)
                        .min(i32::MAX as u128) as i32
                }
            });
            // SAFETY: the slice supplies `len` initialized, writable pollfd
            // entries for the entire call. poll does not retain its pointer.
            // Every active descriptor stays owned by the supervising scope.
            let ready = unsafe {
                libc::poll(
                    descriptors.as_mut_ptr(),
                    descriptors.len() as libc::nfds_t,
                    timeout,
                )
            };
            if ready >= 0 {
                return Ok(ready);
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error)
                    .context("poll recorder signal and supervised process descriptors");
            }
        }
    }

    fn restore_parent(&mut self) -> Result<()> {
        if self.restored {
            return Ok(());
        }
        let drain_error = self.freeze().err();
        #[cfg(test)]
        let first_attempt = self.restore_attempts == 0;
        #[cfg(test)]
        {
            self.restore_attempts += 1;
        }
        #[cfg(test)]
        let action_error = if first_attempt && self.faults.restore_action_once {
            Some(anyhow::Error::new(injected_signal_error(
                "restore sigaction",
            )))
        } else {
            restore_signal_actions(&self.saved, self.saved.len()).err()
        };
        #[cfg(not(test))]
        let action_error = restore_signal_actions(&self.saved, self.saved.len()).err();
        let mask_error = if action_error.is_some() {
            None
        } else {
            #[cfg(test)]
            if first_attempt && self.faults.restore_mask_once {
                Some(injected_signal_error("restore mask"))
            } else {
                set_signal_mask(&self.original_mask).err()
            }
            #[cfg(not(test))]
            {
                set_signal_mask(&self.original_mask).err()
            }
        };
        let result = combine_signal_restore_errors(drain_error, action_error, mask_error);
        if result.is_ok() {
            self.restored = true;
        }
        result
    }
}

impl Drop for SignalScope {
    fn drop(&mut self) {
        let _ = self.restore_parent();
    }
}

fn saved_signal(number: i32, mask: &libc::sigset_t) -> Result<SavedSignal> {
    // SAFETY: Linux sigaction's integer/pointer fields permit zero bits. The
    // value is only exposed after sigaction has populated it successfully.
    let mut action = unsafe { std::mem::zeroed() };
    // SAFETY: null input requests a query; `action` is valid writable storage
    // for one sigaction. The kernel validates the signal number.
    if unsafe { libc::sigaction(number, std::ptr::null(), &mut action) } != 0 {
        return Err(std::io::Error::last_os_error()).context("read control signal action");
    }
    // SAFETY: `mask` is an initialized signal set borrowed for the call; libc
    // validates the signal number before accessing its membership bit.
    let blocked = unsafe { libc::sigismember(mask, number) };
    if blocked < 0 {
        return Err(std::io::Error::last_os_error()).context("read control signal mask member");
    }
    Ok(SavedSignal {
        number,
        action,
        blocked: blocked == 1,
    })
}

fn current_signal_mask() -> std::io::Result<libc::sigset_t> {
    // SAFETY: Linux sigset_t is an integer bitset for which zero is valid.
    let mut mask = unsafe { std::mem::zeroed() };
    // SAFETY: a null input set queries without changing the thread mask, and
    // `mask` provides valid output storage for one initialized sigset_t.
    if unsafe { libc::sigprocmask(libc::SIG_SETMASK, std::ptr::null(), &mut mask) } != 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(mask)
    }
}

fn control_signal_set() -> std::io::Result<libc::sigset_t> {
    // SAFETY: Linux sigset_t permits an all-zero integer representation.
    let mut set = unsafe { std::mem::zeroed() };
    // SAFETY: `set` is writable storage for one sigset_t and remains live
    // throughout libc's initialization of the empty signal set.
    if unsafe { libc::sigemptyset(&mut set) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    for signal in CONTROL_SIGNALS {
        // SAFETY: `set` was initialized by sigemptyset, and each control signal
        // is a valid Linux signal number. The pointer is not retained.
        if unsafe { libc::sigaddset(&mut set, signal) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(set)
}

fn default_signal_action() -> std::io::Result<libc::sigaction> {
    // SAFETY: all Linux sigaction fields admit zero bits; explicit SIG_DFL
    // and a libc-initialized empty mask complete the action before use.
    let mut action = unsafe { std::mem::zeroed::<libc::sigaction>() };
    action.sa_sigaction = libc::SIG_DFL;
    // SAFETY: sa_mask is valid writable sigset_t storage within `action`.
    if unsafe { libc::sigemptyset(&mut action.sa_mask) } != 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(action)
    }
}

fn set_signal_action(number: i32, action: &libc::sigaction) -> std::io::Result<()> {
    // SAFETY: `action` is either returned by a successful sigaction query or
    // fully initialized with SIG_DFL/SIG_IGN and an empty mask. Its borrow
    // lasts through the call; null oldact requests no output. Invalid numbers
    // are reported by libc rather than used for a Rust memory access.
    if unsafe { libc::sigaction(number, action, std::ptr::null_mut()) } != 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn block_signal_set(set: &libc::sigset_t) -> std::io::Result<()> {
    // SAFETY: `set` is an initialized sigset_t borrowed through the syscall;
    // SIG_BLOCK is a valid operation and no old-mask output is requested.
    if unsafe { libc::sigprocmask(libc::SIG_BLOCK, set, std::ptr::null_mut()) } != 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn set_signal_mask(mask: &libc::sigset_t) -> std::io::Result<()> {
    // SAFETY: `mask` is an initialized, saved or libc-edited sigset_t. The
    // borrowed input outlives the syscall and no output pointer is supplied.
    if unsafe { libc::sigprocmask(libc::SIG_SETMASK, mask, std::ptr::null_mut()) } != 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn restore_signal_actions(saved: &[SavedSignal], count: usize) -> Result<()> {
    let mut errors = Vec::new();
    for signal in saved[..count].iter().rev() {
        if let Err(error) = set_signal_action(signal.number, &signal.action) {
            errors.push(format!("signal {}: {error}", signal.number));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        bail!("restore control signal actions: {}", errors.join("; "))
    }
}

fn with_signal_setup_rollback(
    primary: anyhow::Error,
    saved: &[SavedSignal],
    changed: usize,
    original_mask: &libc::sigset_t,
) -> anyhow::Error {
    let actions = restore_signal_actions(saved, changed).err();
    let mask = set_signal_mask(original_mask).err().map(anyhow::Error::new);
    match (actions, mask) {
        (None, None) => primary,
        (actions, mask) => anyhow::anyhow!(
            "{primary:#}; signal setup rollback failed: actions={}; mask={}",
            actions
                .map(|error| format!("{error:#}"))
                .unwrap_or_else(|| "ok".into()),
            mask.map(|error| format!("{error:#}"))
                .unwrap_or_else(|| "ok".into())
        ),
    }
}

fn combine_signal_restore_errors(
    drain: Option<anyhow::Error>,
    actions: Option<anyhow::Error>,
    mask: Option<std::io::Error>,
) -> Result<()> {
    let mut errors = Vec::new();
    if let Some(error) = drain {
        errors.push(format!("drain signalfd: {error:#}"));
    }
    if let Some(error) = actions {
        errors.push(format!("restore actions: {error:#}"));
    }
    if let Some(error) = mask {
        errors.push(format!("restore mask: {error}"));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        bail!("{}", errors.join("; "))
    }
}

#[cfg(test)]
fn injected_signal_error(operation: &str) -> std::io::Error {
    std::io::Error::other(format!("injected signal {operation} failure"))
}

enum VersionProbeOutcome {
    Completed(Vec<u8>),
    Cancelled { supervision_error: Option<String> },
}

struct CapturedPipe<R> {
    reader: R,
    bytes: Vec<u8>,
    eof: bool,
    name: &'static str,
}

impl<R: Read + AsRawFd> CapturedPipe<R> {
    fn new(reader: R, name: &'static str) -> Result<Self> {
        set_nonblocking(reader.as_raw_fd())
            .with_context(|| format!("set version probe {name} nonblocking"))?;
        Ok(Self {
            reader,
            bytes: Vec::new(),
            eof: false,
            name,
        })
    }

    fn pollfd(&self) -> libc::pollfd {
        libc::pollfd {
            fd: if self.eof {
                -1
            } else {
                self.reader.as_raw_fd()
            },
            events: libc::POLLIN,
            revents: 0,
        }
    }

    fn drain(&mut self) -> Result<()> {
        if self.eof {
            return Ok(());
        }
        loop {
            let mut chunk = [0u8; 8192];
            match self.reader.read(&mut chunk) {
                Ok(0) => {
                    self.eof = true;
                    return Ok(());
                }
                Ok(count) => {
                    if self.bytes.len().saturating_add(count) > VERSION_STREAM_LIMIT {
                        bail!(
                            "strace version probe {} exceeded {}-byte limit",
                            self.name,
                            VERSION_STREAM_LIMIT
                        );
                    }
                    self.bytes.extend_from_slice(&chunk[..count]);
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("read strace version probe {}", self.name));
                }
            }
        }
    }
}

fn set_nonblocking(descriptor: i32) -> std::io::Result<()> {
    // SAFETY: F_GETFL takes only an fd and returns status flags without using
    // a variadic pointer. The caller retains ownership of the pipe descriptor.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: F_SETFL expects a promoted integer flag argument, not a pointer;
    // existing status flags are preserved and only O_NONBLOCK is added.
    if unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn require_pidfd_capability() -> Result<()> {
    let self_fd =
        pidfd(std::process::id() as libc::pid_t).context("pidfd_open required (Linux 5.3+)")?;
    send_signal(&self_fd, 0).context("pidfd_send_signal required")
}

fn version_error_display(stderr: &[u8]) -> String {
    let rendered = String::from_utf8_lossy(stderr);
    let rendered = rendered.trim();
    if rendered.len() <= VERSION_ERROR_DISPLAY_LIMIT {
        return if rendered.is_empty() {
            "(no stderr)".into()
        } else {
            rendered.into()
        };
    }

    let suffix = format!(" [truncated; stderr was {} bytes]", stderr.len());
    let budget = VERSION_ERROR_DISPLAY_LIMIT.saturating_sub(suffix.len());
    let mut end = budget.min(rendered.len());
    while !rendered.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &rendered[..end], suffix)
}

fn cancelled_version_probe(guard: CollectorGuard) -> VersionProbeOutcome {
    let supervision_error = guard
        .cancel()
        .err()
        .map(|error| format!("failed to terminate/reap cancelled version probe: {error:#}"));
    VersionProbeOutcome::Cancelled { supervision_error }
}

fn fail_version_probe(
    scope: &mut SignalScope,
    guard: CollectorGuard,
    primary: anyhow::Error,
) -> Result<VersionProbeOutcome> {
    match scope.observe() {
        Ok(Some(_)) => Ok(cancelled_version_probe(guard)),
        Ok(None) => Err(guard.abort(primary)),
        Err(observation) => Err(guard.abort(anyhow::anyhow!(
            "{primary:#}; additionally failed to observe recorder signal: {observation:#}"
        ))),
    }
}

fn complete_version_probe(
    scope: &mut SignalScope,
    guard: CollectorGuard,
    stdout: &mut CapturedPipe<ChildStdout>,
    stderr: &mut CapturedPipe<ChildStderr>,
) -> Result<VersionProbeOutcome> {
    if let Err(error) = stdout.drain().and_then(|()| stderr.drain()) {
        return fail_version_probe(scope, guard, error);
    }
    if let Err(error) = scope.observe() {
        return Err(guard.abort(error));
    }
    if scope.first_observed.is_some() {
        return Ok(cancelled_version_probe(guard));
    }
    let status = guard.finish()?;
    if scope.observe()?.is_some() {
        return Ok(VersionProbeOutcome::Cancelled {
            supervision_error: None,
        });
    }
    match process_exit_from_status(&status) {
        ProcessExit::Exited(0) => Ok(VersionProbeOutcome::Completed(std::mem::take(
            &mut stdout.bytes,
        ))),
        ProcessExit::Exited(code) => bail!(
            "strace version probe exited nonzero ({code}): {}",
            version_error_display(&stderr.bytes)
        ),
        ProcessExit::Signaled(signal) => bail!(
            "strace version probe terminated by signal {signal}: {}",
            version_error_display(&stderr.bytes)
        ),
        ProcessExit::Unknown => bail!(
            "strace version probe completion was not observable: {}",
            version_error_display(&stderr.bytes)
        ),
    }
}

fn supervise_version_probe(
    scope: &mut SignalScope,
    guard: CollectorGuard,
    mut stdout: CapturedPipe<ChildStdout>,
    mut stderr: CapturedPipe<ChildStderr>,
) -> Result<VersionProbeOutcome> {
    let probe_fd = match guard.pidfd_raw() {
        Ok(descriptor) => descriptor,
        Err(error) => return Err(guard.abort(error)),
    };
    let guard = guard;
    let deadline = Instant::now() + VERSION_PROBE_TIMEOUT;

    loop {
        if let Err(error) = scope.observe() {
            return Err(guard.abort(error));
        }
        if scope.first_observed.is_some() {
            return Ok(cancelled_version_probe(guard));
        }

        let mut descriptors = [
            libc::pollfd {
                fd: scope.fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: probe_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            stdout.pollfd(),
            stderr.pollfd(),
        ];
        let ready = match scope.poll(&mut descriptors, Some(deadline)) {
            Ok(ready) => ready,
            Err(error) => return fail_version_probe(scope, guard, error),
        };
        if ready == 0 {
            if let Err(error) = scope.observe() {
                return Err(guard.abort(error));
            }
            if scope.first_observed.is_some() {
                return Ok(cancelled_version_probe(guard));
            }
            return Err(guard.abort(anyhow::anyhow!(
                "strace version probe timed out after {} seconds",
                VERSION_PROBE_TIMEOUT.as_secs()
            )));
        }
        if descriptors
            .iter()
            .any(|descriptor| descriptor.revents & libc::POLLNVAL != 0)
        {
            return fail_version_probe(
                scope,
                guard,
                anyhow::anyhow!(
                    "invalid fd while polling recorder signals, version probe, and output pipes"
                ),
            );
        }

        if descriptors[0].revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) != 0
            && let Err(error) = scope.observe()
        {
            return Err(guard.abort(error));
        }
        if scope.first_observed.is_some() {
            return Ok(cancelled_version_probe(guard));
        }

        if descriptors[2].revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) != 0
            && let Err(error) = stdout.drain()
        {
            return fail_version_probe(scope, guard, error);
        }
        if descriptors[3].revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) != 0
            && let Err(error) = stderr.drain()
        {
            return fail_version_probe(scope, guard, error);
        }
        if descriptors[1].revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) != 0 {
            // Drain only currently buffered bytes. Descendants may retain a pipe
            // fd, so waiting for EOF after the direct probe is pidfd-ready could
            // hang forever and would confuse descendant lifetime with the probe.
            return complete_version_probe(scope, guard, &mut stdout, &mut stderr);
        }
    }
}

fn probe_backend(
    tracer: &std::ffi::OsStr,
    trace_selector: &str,
    scope: &mut SignalScope,
) -> Result<VersionProbeOutcome> {
    if scope.observe()?.is_some() {
        return Ok(VersionProbeOutcome::Cancelled {
            supervision_error: None,
        });
    }
    require_pidfd_capability()?;

    let parent = std::process::id() as libc::pid_t;
    let mut command = Command::new(tracer);
    command
        .arg("-e")
        .arg(trace_selector)
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_supervised_child(&mut command, scope.child_state(), parent);
    let waitability = ChildWaitability::before_spawn()?;
    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            if scope.observe()?.is_some() {
                return Ok(VersionProbeOutcome::Cancelled {
                    supervision_error: None,
                });
            }
            return Err(anyhow::Error::new(error).context(
                "failed to start supervised strace version probe; verify strace installation, RUNDELTA_STRACE, and process supervision support",
            ));
        }
    };
    let mut guard = CollectorGuard::attach(child, waitability, CollectorFaults::default())?;
    let stdout = match guard.take_stdout() {
        Ok(stdout) => stdout,
        Err(error) => return Err(guard.abort(error)),
    };
    let stderr = match guard.take_stderr() {
        Ok(stderr) => stderr,
        Err(error) => return Err(guard.abort(error)),
    };
    let stdout = match CapturedPipe::new(stdout, "stdout") {
        Ok(stdout) => stdout,
        Err(error) => return Err(guard.abort(error)),
    };
    let stderr = match CapturedPipe::new(stderr, "stderr") {
        Ok(stderr) => stderr,
        Err(error) => return Err(guard.abort(error)),
    };
    supervise_version_probe(scope, guard, stdout, stderr)
}

#[derive(Debug)]
/// Monotonic deadlines anchored immediately before the formal collector spawn.
///
/// The version probe is intentionally outside this clock. Policy is validated
/// once before any side effect and checked again at the actual anchor. A later
/// representability failure still prevents target startup, but the writer may
/// already contain an unfinalized running record for failure inspection.
struct CaptureSchedule {
    duration_limit: Option<Duration>,
    duration_deadline: Option<Instant>,
    terminate_grace: Duration,
}

impl CaptureSchedule {
    fn before_spawn(limits: CaptureLimits) -> Result<Self> {
        let started = Instant::now();
        limits.validate_at(started)?;
        let duration_deadline = limits
            .max_duration
            .map(|duration| {
                started
                    .checked_add(duration)
                    .context("validated capture duration")
            })
            .transpose()?;
        Ok(Self {
            duration_limit: limits.max_duration,
            duration_deadline,
            terminate_grace: limits.terminate_grace,
        })
    }

    fn termination_deadline(&self) -> Result<Instant> {
        Instant::now()
            .checked_add(self.terminate_grace)
            .context("capture termination grace cannot be represented by the monotonic clock")
    }
}

/// Facts returned by collector supervision without collapsing process status,
/// capture limits, or trace-length observation failures into one outcome.
struct CollectorSupervision {
    status: Result<ExitStatus>,
    limits: CaptureLimitFacts,
    trace_len_error: Option<String>,
}

/// Anchored trace-size observation plus deterministic test-only fault seams.
///
/// Production always uses the open [`store::RunWriter`]; tests can exercise
/// classification without weakening the storage façade or adding a trait.
enum TraceLengthSource<'a> {
    Anchored(&'a store::RunWriter),
    #[cfg(test)]
    Fixed(u64),
    #[cfg(test)]
    Failed(&'static str),
}

impl TraceLengthSource<'_> {
    fn len(&self) -> Result<u64> {
        match self {
            Self::Anchored(writer) => writer.trace_len(),
            #[cfg(test)]
            Self::Fixed(length) => Ok(*length),
            #[cfg(test)]
            Self::Failed(message) => bail!("{message}"),
        }
    }
}

impl CollectorSupervision {
    fn finished(status: Result<ExitStatus>, limits: CaptureLimitFacts) -> Self {
        Self {
            status,
            limits,
            trace_len_error: None,
        }
    }

    fn failed(guard: CollectorGuard, error: anyhow::Error, limits: CaptureLimitFacts) -> Self {
        Self::finished(Err(guard.abort(error)), limits)
    }

    fn trace_len_failed(
        guard: CollectorGuard,
        error: anyhow::Error,
        limits: CaptureLimitFacts,
    ) -> Self {
        Self {
            status: guard.cancel(),
            limits,
            trace_len_error: Some(format!(
                "recorder trace length supervision failed: {error:#}"
            )),
        }
    }
}

fn observe_capture_limits(
    trace_length: &TraceLengthSource<'_>,
    policy: CaptureLimits,
    schedule: &CaptureSchedule,
    facts: &mut CaptureLimitFacts,
) -> Result<bool> {
    let mut newly_exceeded = false;
    if let (Some(limit), Some(deadline)) = (schedule.duration_limit, schedule.duration_deadline)
        && Instant::now() >= deadline
    {
        newly_exceeded |= facts.observe_duration(limit);
    }
    if let Some(limit) = policy.max_trace_bytes
        && facts.trace_bytes.is_none()
    {
        let observed = trace_length
            .len()
            .context("inspect anchored trace length during collector supervision")?;
        newly_exceeded |= facts.observe_trace_bytes(observed, limit);
    }
    Ok(newly_exceeded)
}

fn earlier_deadline(current: &mut Option<Instant>, candidate: Option<Instant>) {
    if let Some(candidate) = candidate
        && current.is_none_or(|current| candidate < current)
    {
        *current = Some(candidate);
    }
}

fn supervision_poll_deadline(
    policy: CaptureLimits,
    schedule: &CaptureSchedule,
    facts: &CaptureLimitFacts,
    termination_deadline: Option<Instant>,
) -> Option<Instant> {
    let mut deadline = termination_deadline;
    if facts.duration.is_none() {
        earlier_deadline(&mut deadline, schedule.duration_deadline);
    }
    if policy.max_trace_bytes.is_some() && facts.trace_bytes.is_none() {
        earlier_deadline(
            &mut deadline,
            Instant::now().checked_add(TRACE_LIMIT_POLL_INTERVAL),
        );
    }
    deadline
}

/// Supervise one attached collector until it is reaped.
///
/// Recorder signals, capture limits, and collector readiness are observed in
/// that priority order. While a byte limit is pending, poll wakes at least once
/// every 50 ms to inspect the anchored inode. Limit cleanup sends TERM, waits
/// the configured grace, then sends KILL; all returned process status remains
/// the actual wait result rather than a synthetic policy outcome.
fn supervise_collector(
    scope: &mut SignalScope,
    guard: CollectorGuard,
    trace_length: &TraceLengthSource<'_>,
    policy: CaptureLimits,
    schedule: &CaptureSchedule,
) -> CollectorSupervision {
    let collector_fd = match guard.pidfd_raw() {
        Ok(descriptor) => descriptor,
        Err(error) => {
            return CollectorSupervision::failed(guard, error, CaptureLimitFacts::default());
        }
    };
    let guard = guard;
    let mut facts = CaptureLimitFacts::default();
    let mut termination_deadline = None;
    let mut recorder_signal_handled = false;
    let mut collector_ready = false;

    loop {
        if let Err(error) = scope.observe() {
            return CollectorSupervision::failed(guard, error, facts);
        }
        let newly_exceeded =
            match observe_capture_limits(trace_length, policy, schedule, &mut facts) {
                Ok(exceeded) => exceeded,
                Err(error) => return CollectorSupervision::trace_len_failed(guard, error, facts),
            };

        if !recorder_signal_handled && let Some(signal) = scope.first_observed {
            recorder_signal_handled = true;
            if scope.requires_direct_termination(signal) {
                return CollectorSupervision::finished(guard.cancel(), facts);
            }
            if let Err(error) = guard.forward(signal) {
                return CollectorSupervision::failed(guard, error, facts);
            }
            if termination_deadline.is_none() {
                termination_deadline = match schedule.termination_deadline() {
                    Ok(deadline) => Some(deadline),
                    Err(error) => return CollectorSupervision::failed(guard, error, facts),
                };
            }
        }

        if newly_exceeded && termination_deadline.is_none() {
            if let Err(error) = guard.forward(libc::SIGTERM) {
                return CollectorSupervision::failed(guard, error, facts);
            }
            termination_deadline = match schedule.termination_deadline() {
                Ok(deadline) => Some(deadline),
                Err(error) => return CollectorSupervision::failed(guard, error, facts),
            };
        }

        // A simultaneous signal/limit observation is reduced before the
        // collector status. Once those facts are retained, a ready direct child
        // can be reaped without manufacturing a termination outcome.
        if collector_ready {
            return CollectorSupervision::finished(guard.finish(), facts);
        }
        if termination_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return CollectorSupervision::finished(guard.cancel(), facts);
        }

        let mut descriptors = [
            libc::pollfd {
                fd: scope.fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: collector_fd,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let deadline = supervision_poll_deadline(policy, schedule, &facts, termination_deadline);
        if let Err(error) = scope.poll(&mut descriptors, deadline) {
            return CollectorSupervision::failed(guard, error, facts);
        }
        if descriptors
            .iter()
            .any(|descriptor| descriptor.revents & libc::POLLNVAL != 0)
        {
            return CollectorSupervision::failed(
                guard,
                anyhow::anyhow!("invalid fd while polling recorder signals and collector"),
                facts,
            );
        }
        collector_ready =
            descriptors[1].revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) != 0;
    }
}

/// Record one command under the supplied resource policy.
///
/// The backend is never bypassed. A finalized result contains separately
/// reduced target, collector, recorder, limit, and evidence-integrity facts.
pub(crate) fn record(
    root: &Path,
    name: &str,
    command: &[std::ffi::OsString],
    selected: &[String],
    limits: CaptureLimits,
) -> Result<RecordResult> {
    let mut environment = std::collections::BTreeMap::new();
    for key in selected {
        if key.is_empty() || key.contains('=') || key.chars().any(char::is_control) {
            bail!("environment variable name must be nonempty without '=' or control characters");
        }
        environment.insert(
            key.clone(),
            std::env::var_os(key).map(|v| bytes(v.as_bytes())),
        );
    }
    if name.is_empty() || name.chars().any(char::is_control) {
        bail!("name must be nonempty without control characters");
    }
    // Validate user policy before opening storage or probing the backend so an
    // impossible monotonic deadline has no filesystem or process side effect.
    limits.validate()?;
    let storage = store::open(root)?;
    storage.check_name_available(name)?;
    let mut signals = SignalScope::new()?;
    let operation = (|| -> Result<RecordResult> {
        let tracer = std::env::var_os("RUNDELTA_STRACE").unwrap_or_else(|| "strace".into());
        let trace_selector = parse::trace_selector();
        let version = probe_backend(&tracer, &trace_selector, &mut signals)?;
        let (version_stdout, mut cancelled_before_writer, mut recorder_supervision_error) =
            match version {
                VersionProbeOutcome::Completed(stdout) => (stdout, false, None),
                VersionProbeOutcome::Cancelled { supervision_error } => {
                    (Vec::new(), true, supervision_error)
                }
            };
        if !cancelled_before_writer && recorder_supervision_error.is_none() {
            match signals.observe() {
                Ok(signal) => cancelled_before_writer = signal.is_some(),
                Err(error) => {
                    recorder_supervision_error = Some(format!(
                        "recorder signal supervision failed before collector spawn: {error:#}"
                    ));
                }
            }
        }

        let mut writer = storage.begin_run(name)?;
        let mut m = Metadata {
            schema_version: 3,
            tool_version: env!("CARGO_PKG_VERSION").into(),
            id: writer.id().into(),
            name: name.into(),
            command: command.iter().map(|s| bytes(s.as_bytes())).collect(),
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

        let observed_before_spawn = if recorder_supervision_error.is_none() {
            match signals.observe() {
                Ok(signal) => signal,
                Err(error) => {
                    recorder_supervision_error = Some(format!(
                        "recorder signal supervision failed before collector spawn: {error:#}"
                    ));
                    None
                }
            }
        } else {
            None
        };
        let mut limit_facts = CaptureLimitFacts::default();
        let mut skip_trace_read_for_unavailable_length = false;
        let collector = if cancelled_before_writer || observed_before_spawn.is_some() {
            CollectorFacts::NotStarted(
                "collector was not started because recorder cancellation was observed".into(),
            )
        } else if recorder_supervision_error.is_some() {
            CollectorFacts::NotStarted(
                "collector was not started because recorder signal supervision failed".into(),
            )
        } else {
            let schedule = CaptureSchedule::before_spawn(limits)?;
            let result = (|| -> Result<CollectorGuard> {
                let parent = std::process::id() as libc::pid_t;
                let mut cmd = Command::new(&tracer);
                cmd.args([
                    "-f",
                    "-q",
                    "-I",
                    "2",
                    "--kill-on-exit",
                    "-xx",
                    "-yy",
                    "-s",
                    "65535",
                    "-e",
                ])
                .arg(&trace_selector)
                .arg("-o")
                .arg(&trace)
                .arg("--")
                .args(command);
                configure_supervised_child(&mut cmd, signals.child_state(), parent);
                let waitability = ChildWaitability::before_spawn()?;
                let child = cmd.spawn().context(
                    "failed to start supervised strace; command was not run without tracing",
                )?;
                CollectorGuard::attach(child, waitability, CollectorFaults::default())
            })();
            match result {
                Ok(guard) => {
                    let trace_length = TraceLengthSource::Anchored(&writer);
                    let supervision =
                        supervise_collector(&mut signals, guard, &trace_length, limits, &schedule);
                    limit_facts = supervision.limits;
                    if let Some(error) = supervision.trace_len_error {
                        skip_trace_read_for_unavailable_length = true;
                        append_recorder_supervision_error(&mut recorder_supervision_error, error);
                    }
                    match supervision.status {
                        Ok(status) => CollectorFacts::Reaped(process_exit_from_status(&status)),
                        Err(error) => CollectorFacts::Failed(format!("{error:#}")),
                    }
                }
                Err(error) => CollectorFacts::Failed(format!("{error:#}")),
            }
        };

        if let Some(limit) = limits.max_trace_bytes {
            match writer
                .trace_len()
                .context("inspect final anchored trace length before reading raw evidence")
            {
                Ok(observed) => {
                    limit_facts.observe_trace_bytes(observed, limit);
                }
                Err(error) => {
                    skip_trace_read_for_unavailable_length = true;
                    append_recorder_supervision_error(
                        &mut recorder_supervision_error,
                        format!(
                            "recorder trace length supervision failed after collector completion: {error:#}"
                        ),
                    );
                }
            }
        }
        let skip_trace_read =
            skip_trace_read_for_unavailable_length || limit_facts.trace_bytes.is_some();
        let (mut parsed, trace_read_error) = if skip_trace_read {
            (empty_trace_parse(&m.cwd), None)
        } else {
            parse_trace_stream(&m.cwd, |parser| {
                writer.for_each_trace_line(|line| {
                    parser.push_line(line);
                    Ok(())
                })
            })
        };
        let completion = Completion::from_timeline(&parsed);
        m.raw_lines = parsed.lines;
        m.parsed_events = parsed.events.len();
        m.ended = Some(now());
        let root_outcome = completion
            .root_exit
            .map(|index| parsed.events[index].outcome.as_str());
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
        let decision = reduce_capture_facts(CaptureFactsInput {
            root_outcome,
            missing_generations: &completion.missing_exits,
            collector,
            recorder,
            limits: limit_facts,
            recorder_supervision_error,
            warnings: std::mem::take(&mut parsed.warnings),
            trace_read_error,
        });
        let state = decision.state;
        let completeness = decision.completeness;
        let code = decision.cli_exit;
        decision.apply_to_metadata(&mut m);
        let evidence_dir = writer.evidence_dir().display().to_string();
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
    let restoration = signals.restore_parent();
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

/// Derive process completion from the parser's evidence-ordered task generations.
///
/// The root is the generation containing the first successful exec. An exit
/// satisfies only its own generation, so an earlier incarnation of the same
/// numeric PID cannot prove either root or child completion.
struct Completion {
    root_exit: Option<usize>,
    missing_exits: Vec<String>,
}

impl Completion {
    fn from_timeline(parsed: &parse::Parsed) -> Self {
        let timeline = &parsed.timeline;
        let root_generation = timeline.events_in_evidence_order().find_map(|index| {
            let event = &parsed.events[index];
            (event.kind == "exec" && event.outcome == "success")
                .then(|| timeline.event_generation(index))
        });
        let root_exit = root_generation.and_then(|generation| timeline.exit_event(generation));

        let mut pid_counts = std::collections::BTreeMap::new();
        for generation in timeline.generations() {
            *pid_counts.entry(timeline.pid(generation)).or_insert(0usize) += 1;
        }
        let mut missing_exits = timeline
            .generations()
            .filter(|&generation| timeline.exit_event(generation).is_none())
            .map(|generation| {
                let pid = timeline.pid(generation);
                if pid_counts[pid] == 1 {
                    pid.to_owned()
                } else {
                    timeline.label(generation)
                }
            })
            .collect::<Vec<_>>();
        // Preserve the old lexical PID ordering for ordinary single-generation
        // traces while making repeated-generation labels deterministic.
        missing_exits.sort();
        Self {
            root_exit,
            missing_exits,
        }
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
        assert_eq!(actual_completion.root_exit, expected_completion.root_exit);
        assert_eq!(
            actual_completion.missing_exits,
            expected_completion.missing_exits
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
        assert_eq!(decision.target, None);
        assert_eq!(decision.state, CaptureState::CaptureFailed);
        assert_eq!(decision.completeness, CaptureCompleteness::Incomplete);
        assert_eq!(decision.cli_exit, 125);
        assert_eq!(
            decision.warnings,
            [
                "cannot read trace: late injected trace read failure",
                "root process completion not observed; capture incomplete",
            ]
        );
    }
}

fn signal_number(outcome: &str) -> Option<i32> {
    let name = outcome
        .strip_prefix("+++ killed by ")?
        .split_whitespace()
        .next()?;
    // strace signal.c uses ASM_SIGRTMIN=32 (kernel ABI), not glibc's
    // dynamic SIGRTMIN(), which skips NPTL-reserved signals on this platform.
    if let Some(offset) = name
        .strip_prefix("SIGRT_")
        .and_then(|s| s.parse::<i32>().ok())
    {
        return offset
            .checked_add(32)
            .filter(|v| *v >= 32 && *v <= libc::SIGRTMAX());
    }
    if let Some(offset) = name
        .strip_prefix("SIGRTMIN+")
        .and_then(|s| s.parse::<i32>().ok())
    {
        return libc::SIGRTMIN()
            .checked_add(offset)
            .filter(|v| *v >= libc::SIGRTMIN() && *v <= libc::SIGRTMAX());
    }
    if name == "SIGRTMIN" {
        return Some(libc::SIGRTMIN());
    }
    if name == "SIGRTMAX" {
        return Some(libc::SIGRTMAX());
    }
    // strace uses Linux signal abbreviations; libc values follow this target's ABI.
    let signals = [
        ("SIGHUP", libc::SIGHUP),
        ("SIGINT", libc::SIGINT),
        ("SIGQUIT", libc::SIGQUIT),
        ("SIGILL", libc::SIGILL),
        ("SIGTRAP", libc::SIGTRAP),
        ("SIGABRT", libc::SIGABRT),
        ("SIGBUS", libc::SIGBUS),
        ("SIGFPE", libc::SIGFPE),
        ("SIGKILL", libc::SIGKILL),
        ("SIGUSR1", libc::SIGUSR1),
        ("SIGSEGV", libc::SIGSEGV),
        ("SIGUSR2", libc::SIGUSR2),
        ("SIGPIPE", libc::SIGPIPE),
        ("SIGALRM", libc::SIGALRM),
        ("SIGTERM", libc::SIGTERM),
        ("SIGSTKFLT", libc::SIGSTKFLT),
        ("SIGCHLD", libc::SIGCHLD),
        ("SIGCONT", libc::SIGCONT),
        ("SIGSTOP", libc::SIGSTOP),
        ("SIGTSTP", libc::SIGTSTP),
        ("SIGTTIN", libc::SIGTTIN),
        ("SIGTTOU", libc::SIGTTOU),
        ("SIGURG", libc::SIGURG),
        ("SIGXCPU", libc::SIGXCPU),
        ("SIGXFSZ", libc::SIGXFSZ),
        ("SIGVTALRM", libc::SIGVTALRM),
        ("SIGPROF", libc::SIGPROF),
        ("SIGWINCH", libc::SIGWINCH),
        ("SIGIO", libc::SIGIO),
        ("SIGPWR", libc::SIGPWR),
        ("SIGSYS", libc::SIGSYS),
    ];
    signals
        .into_iter()
        .find_map(|(n, v)| (name == n).then_some(v))
}

#[cfg(test)]
mod signal_tests {
    use super::*;

    #[test]
    fn linux_standard_signal_names_are_complete() {
        let signals = [
            ("SIGHUP", libc::SIGHUP),
            ("SIGINT", libc::SIGINT),
            ("SIGQUIT", libc::SIGQUIT),
            ("SIGILL", libc::SIGILL),
            ("SIGTRAP", libc::SIGTRAP),
            ("SIGABRT", libc::SIGABRT),
            ("SIGBUS", libc::SIGBUS),
            ("SIGFPE", libc::SIGFPE),
            ("SIGKILL", libc::SIGKILL),
            ("SIGUSR1", libc::SIGUSR1),
            ("SIGSEGV", libc::SIGSEGV),
            ("SIGUSR2", libc::SIGUSR2),
            ("SIGPIPE", libc::SIGPIPE),
            ("SIGALRM", libc::SIGALRM),
            ("SIGTERM", libc::SIGTERM),
            ("SIGSTKFLT", libc::SIGSTKFLT),
            ("SIGCHLD", libc::SIGCHLD),
            ("SIGCONT", libc::SIGCONT),
            ("SIGSTOP", libc::SIGSTOP),
            ("SIGTSTP", libc::SIGTSTP),
            ("SIGTTIN", libc::SIGTTIN),
            ("SIGTTOU", libc::SIGTTOU),
            ("SIGURG", libc::SIGURG),
            ("SIGXCPU", libc::SIGXCPU),
            ("SIGXFSZ", libc::SIGXFSZ),
            ("SIGVTALRM", libc::SIGVTALRM),
            ("SIGPROF", libc::SIGPROF),
            ("SIGWINCH", libc::SIGWINCH),
            ("SIGIO", libc::SIGIO),
            ("SIGPWR", libc::SIGPWR),
            ("SIGSYS", libc::SIGSYS),
        ];
        for (name, number) in signals {
            assert_eq!(
                signal_number(&format!("+++ killed by {name} +++")),
                Some(number),
                "{name}"
            );
        }
        assert_eq!(signal_number("+++ killed by SIGFUTURE +++"), None);
    }

    #[test]
    fn strace_realtime_names_use_kernel_base() {
        assert_eq!(signal_number("+++ killed by SIGRT_2 +++"), Some(34));
        assert_eq!(signal_number("+++ killed by SIGRT_32 +++"), Some(64));
        assert_eq!(signal_number("+++ killed by SIGRT_999 +++"), None);
    }
}

#[cfg(test)]
mod signal_scope_tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    static SIGNAL_STATE: Mutex<()> = Mutex::new(());

    struct OriginalSignalState {
        saved: [SavedSignal; CONTROL_SIGNALS.len()],
        mask: libc::sigset_t,
    }

    impl OriginalSignalState {
        fn capture() -> Self {
            let mask = current_signal_mask().expect("read original test signal mask");
            let saved = CONTROL_SIGNALS.map(|number| {
                saved_signal(number, &mask).expect("read original test signal action")
            });
            Self { saved, mask }
        }
    }

    impl Drop for OriginalSignalState {
        fn drop(&mut self) {
            let watched = control_signal_set().expect("build test signal set");
            block_signal_set(&watched).expect("block signals while restoring test state");
            restore_signal_actions(&self.saved, self.saved.len())
                .expect("restore original test actions");
            set_signal_mask(&self.mask).expect("restore original test mask");
        }
    }

    fn signal_test_lock() -> MutexGuard<'static, ()> {
        SIGNAL_STATE
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    fn set_test_signal(number: i32, disposition: usize, blocked: bool) {
        let watched = control_signal_set().expect("build test signal set");
        block_signal_set(&watched).expect("block signals while changing test state");
        let mut action = default_signal_action().expect("build test signal action");
        action.sa_sigaction = disposition;
        set_signal_action(number, &action).expect("set test signal action");

        let mut mask = current_signal_mask().expect("read blocked test mask");
        // SAFETY: mask was initialized by a successful query and is writable;
        // test callers pass one of the valid Linux control signal numbers.
        let result = unsafe {
            if blocked {
                libc::sigaddset(&mut mask, number)
            } else {
                libc::sigdelset(&mut mask, number)
            }
        };
        assert_eq!(result, 0, "change test signal mask");
        set_signal_mask(&mask).expect("install test signal mask");
    }

    fn send_to_current_thread(number: i32) {
        // SAFETY: pthread_self returns the current live thread, so its handle
        // cannot expire during this call. Tests send only blocked control
        // signals to that thread and consume them through its signalfd.
        let result = unsafe { libc::pthread_kill(libc::pthread_self(), number) };
        assert_eq!(result, 0, "send signal to test thread");
    }

    fn assert_saved_state(expected: &[SavedSignal]) {
        let mask = current_signal_mask().expect("read restored mask");
        for saved in expected {
            let actual = saved_signal(saved.number, &mask).expect("read restored action");
            assert_eq!(actual.action.sa_sigaction, saved.action.sa_sigaction);
            assert_eq!(actual.blocked, saved.blocked);
        }
    }

    fn assert_control_signals_blocked() {
        let mask = current_signal_mask().expect("read active signal mask");
        for number in CONTROL_SIGNALS {
            // SAFETY: the queried mask is initialized and borrowed for the
            // call; `number` is a valid member of CONTROL_SIGNALS.
            assert_eq!(unsafe { libc::sigismember(&mask, number) }, 1, "{number}");
        }
    }

    #[test]
    fn setup_failures_roll_back_changed_actions_and_mask() {
        let _lock = signal_test_lock();
        let original = OriginalSignalState::capture();
        set_test_signal(libc::SIGHUP, libc::SIG_IGN, false);
        let expected_mask = current_signal_mask().expect("read fixture mask");
        let expected = CONTROL_SIGNALS
            .map(|number| saved_signal(number, &expected_mask).expect("read fixture action"));

        for faults in [
            SignalFaults {
                action_index: Some(1),
                ..SignalFaults::default()
            },
            SignalFaults {
                signalfd: true,
                ..SignalFaults::default()
            },
        ] {
            let error = SignalScope::with_faults(faults)
                .err()
                .expect("injected setup failure must fail");
            assert!(format!("{error:#}").contains("injected signal"));
            assert_saved_state(&expected);
        }
        drop(original);
    }

    #[test]
    fn inherited_ignore_is_observable_and_restored() {
        let _lock = signal_test_lock();
        let original = OriginalSignalState::capture();
        set_test_signal(libc::SIGHUP, libc::SIG_IGN, false);
        let expected_mask = current_signal_mask().expect("read fixture mask");
        let expected = CONTROL_SIGNALS
            .map(|number| saved_signal(number, &expected_mask).expect("read fixture action"));

        let mut scope = SignalScope::new().expect("create signal scope");
        assert!(scope.requires_direct_termination(libc::SIGHUP));
        send_to_current_thread(libc::SIGHUP);
        assert_eq!(
            scope.observe().expect("observe ignored HUP"),
            Some(libc::SIGHUP)
        );
        scope.restore_parent().expect("restore caller signal state");
        assert_saved_state(&expected);
        drop(scope);
        drop(original);
    }

    #[test]
    fn inherited_blocked_mask_is_preserved_and_uses_direct_cancellation() {
        let _lock = signal_test_lock();
        let original = OriginalSignalState::capture();
        set_test_signal(libc::SIGHUP, libc::SIG_DFL, true);
        let expected_mask = current_signal_mask().expect("read fixture mask");
        let expected = CONTROL_SIGNALS
            .map(|number| saved_signal(number, &expected_mask).expect("read fixture action"));

        let mut scope = SignalScope::new().expect("create signal scope");
        assert!(scope.requires_direct_termination(libc::SIGHUP));
        assert_eq!(
            // SAFETY: child_state contains the initialized saved signal mask,
            // whose temporary lives through this query of valid SIGHUP.
            unsafe { libc::sigismember(&scope.child_state().mask, libc::SIGHUP) },
            1
        );
        send_to_current_thread(libc::SIGHUP);
        assert_eq!(
            scope.observe().expect("observe blocked HUP"),
            Some(libc::SIGHUP)
        );
        scope.restore_parent().expect("restore caller signal state");
        assert_saved_state(&expected);
        drop(scope);
        drop(original);
    }

    #[test]
    fn first_observed_signal_is_stable_across_later_observations() {
        let _lock = signal_test_lock();
        let _original = OriginalSignalState::capture();
        let mut scope = SignalScope::new().expect("create signal scope");

        send_to_current_thread(libc::SIGHUP);
        assert_eq!(
            scope.observe().expect("observe first signal"),
            Some(libc::SIGHUP)
        );
        send_to_current_thread(libc::SIGTERM);
        assert_eq!(
            scope.observe().expect("observe additional signal"),
            Some(libc::SIGHUP)
        );
        scope.restore_parent().expect("restore caller signal state");
    }

    #[test]
    fn failed_restore_remains_retryable_for_drop_cleanup() {
        let _lock = signal_test_lock();
        let original = OriginalSignalState::capture();

        for faults in [
            SignalFaults {
                restore_action_once: true,
                ..SignalFaults::default()
            },
            SignalFaults {
                restore_mask_once: true,
                ..SignalFaults::default()
            },
        ] {
            let mut scope = SignalScope::with_faults(faults).expect("create signal scope");
            let error = scope
                .restore_parent()
                .expect_err("first restore attempt must fail");
            assert!(format!("{error:#}").contains("injected signal restore"));
            assert!(!scope.restored);
            assert_control_signals_blocked();
            scope
                .restore_parent()
                .expect("second restore attempt must succeed");
            assert!(scope.restored);
            assert_saved_state(&original.saved);
        }
        drop(original);
    }

    #[test]
    fn frozen_signals_remain_pending_through_restore_and_retry() {
        let _lock = signal_test_lock();
        let _original = OriginalSignalState::capture();
        for number in CONTROL_SIGNALS {
            set_test_signal(number, libc::SIG_DFL, true);
            for faults in [
                SignalFaults::default(),
                SignalFaults {
                    restore_action_once: true,
                    ..SignalFaults::default()
                },
                SignalFaults {
                    restore_mask_once: true,
                    ..SignalFaults::default()
                },
            ] {
                let mut scope = SignalScope::with_faults(faults).unwrap();
                assert_eq!(scope.freeze().unwrap(), RecorderStop::None);
                send_to_current_thread(number);
                let observed = scope.observe().unwrap();
                if scope.restore_parent().is_err() {
                    assert_control_signals_blocked();
                    scope.restore_parent().unwrap();
                }
                // A separate reader consumes any preserved pending signal before
                // assertions/fixture teardown can unblock its default action.
                let mut reader = SignalScope::new().unwrap();
                let pending = reader.observe().unwrap();
                reader.restore_parent().unwrap();
                assert_eq!(observed, None, "frozen capture facts changed");
                assert_eq!(pending, Some(number), "late signal was consumed");
                assert_eq!(scope.recorder_stop(), RecorderStop::None);
            }
        }
    }

    #[test]
    fn restoration_retry_does_not_acquire_new_signals() {
        let _lock = signal_test_lock();
        let _original = OriginalSignalState::capture();
        set_test_signal(libc::SIGHUP, libc::SIG_DFL, true);
        let mut scope = SignalScope::with_faults(SignalFaults {
            restore_action_once: true,
            ..SignalFaults::default()
        })
        .unwrap();
        assert!(scope.restore_parent().is_err());
        send_to_current_thread(libc::SIGHUP);
        scope.restore_parent().unwrap();
        let mut reader = SignalScope::new().unwrap();
        let pending = reader.observe().unwrap();
        reader.restore_parent().unwrap();
        assert_eq!(pending, Some(libc::SIGHUP));
        assert_eq!(scope.recorder_stop(), RecorderStop::None);
    }

    #[test]
    fn late_signal_does_not_rewrite_an_already_observed_cancellation() {
        let _lock = signal_test_lock();
        let _original = OriginalSignalState::capture();
        set_test_signal(libc::SIGTERM, libc::SIG_DFL, true);
        let mut scope = SignalScope::new().unwrap();
        send_to_current_thread(libc::SIGHUP);
        assert_eq!(scope.freeze().unwrap(), RecorderStop::Signal(libc::SIGHUP));
        send_to_current_thread(libc::SIGTERM);
        let frozen = scope.freeze().unwrap();
        scope.restore_parent().unwrap();
        let mut reader = SignalScope::new().unwrap();
        let pending = reader.observe().unwrap();
        reader.restore_parent().unwrap();
        assert_eq!(frozen, RecorderStop::Signal(libc::SIGHUP));
        assert_eq!(pending, Some(libc::SIGTERM));
    }

    #[test]
    fn poll_failure_terminates_and_reaps_the_collector() {
        let _lock = signal_test_lock();
        let _original = OriginalSignalState::capture();
        let mut scope = SignalScope::with_faults(SignalFaults {
            poll: true,
            ..SignalFaults::default()
        })
        .expect("create signal scope");
        let waitability = ChildWaitability::before_spawn().expect("waitable test children");
        let child = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleeping collector fixture");
        let process_id = child.id();
        let observer = pidfd(process_id as i32).expect("open observer pidfd");
        let guard = CollectorGuard::attach(child, waitability, CollectorFaults::default())
            .expect("attach collector guard");

        let policy = CaptureLimits::default();
        let schedule = CaptureSchedule::before_spawn(policy).expect("build capture schedule");
        let trace_length = TraceLengthSource::Fixed(0);
        let error = supervise_collector(&mut scope, guard, &trace_length, policy, &schedule)
            .status
            .expect_err("injected poll failure must escape supervision");
        assert!(format!("{error:#}").contains("injected signal poll failure"));
        let gone = send_signal(&observer, 0).expect_err("collector must be gone");
        assert_eq!(gone.raw_os_error(), Some(libc::ESRCH));
        assert!(!std::path::PathBuf::from(format!("/proc/{process_id}")).exists());
        scope.restore_parent().expect("restore caller signal state");
    }

    #[test]
    fn forwarded_signal_escalates_after_grace_when_collector_ignores_it() {
        let _lock = signal_test_lock();
        let _original = OriginalSignalState::capture();
        set_test_signal(libc::SIGHUP, libc::SIG_DFL, false);
        let mut scope = SignalScope::new().expect("create signal scope");
        let child_signals = scope.child_state();
        let mut command = Command::new("sleep");
        command.arg("30");
        // SAFETY: this post-fork closure uses only copied signal state and
        // async-signal-safe signal operations. Linux sigaction permits zero
        // initialization; its mask is initialized before installing SIG_IGN.
        // The closure neither allocates nor touches shared Rust state.
        unsafe {
            command.pre_exec(move || {
                child_signals.restore()?;
                let mut ignored = std::mem::zeroed::<libc::sigaction>();
                ignored.sa_sigaction = libc::SIG_IGN;
                if libc::sigemptyset(&mut ignored.sa_mask) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                set_signal_action(libc::SIGHUP, &ignored)
            });
        }
        let waitability = ChildWaitability::before_spawn().expect("waitable test children");
        let child = command.spawn().expect("spawn signal-resistant collector");
        let process_id = child.id();
        let observer = pidfd(process_id as i32).expect("open observer pidfd");
        let guard = CollectorGuard::attach(child, waitability, CollectorFaults::default())
            .expect("attach collector guard");
        send_to_current_thread(libc::SIGHUP);

        let started = Instant::now();
        let policy = CaptureLimits::default();
        let schedule = CaptureSchedule::before_spawn(policy).expect("build capture schedule");
        let trace_length = TraceLengthSource::Fixed(0);
        let status = supervise_collector(&mut scope, guard, &trace_length, policy, &schedule)
            .status
            .expect("supervise escalation");
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        assert!(started.elapsed() >= Duration::from_millis(1_800));
        assert_eq!(scope.recorder_stop(), RecorderStop::Signal(libc::SIGHUP));
        let gone = send_signal(&observer, 0).expect_err("collector must be gone");
        assert_eq!(gone.raw_os_error(), Some(libc::ESRCH));
        assert!(!std::path::PathBuf::from(format!("/proc/{process_id}")).exists());
        scope.restore_parent().expect("restore caller signal state");
    }

    #[test]
    fn duration_limit_uses_term_then_configured_grace_then_kill() {
        let _lock = signal_test_lock();
        let _original = OriginalSignalState::capture();
        let mut scope = SignalScope::new().expect("create signal scope");
        let child_signals = scope.child_state();
        let policy = CaptureLimits {
            max_duration: Some(Duration::ZERO),
            max_trace_bytes: None,
            terminate_grace: Duration::from_millis(20),
        };
        let schedule =
            CaptureSchedule::before_spawn(policy).expect("build immediate capture schedule");
        let mut command = Command::new("sleep");
        command.arg("30");
        // SAFETY: this post-fork closure uses copied state and signal syscalls
        // only, with no locks/allocations. The zero-valid Linux sigaction is
        // completed with SIG_IGN and a libc-initialized empty signal mask.
        unsafe {
            command.pre_exec(move || {
                child_signals.restore()?;
                let mut ignored = std::mem::zeroed::<libc::sigaction>();
                ignored.sa_sigaction = libc::SIG_IGN;
                if libc::sigemptyset(&mut ignored.sa_mask) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                set_signal_action(libc::SIGTERM, &ignored)
            });
        }
        let waitability = ChildWaitability::before_spawn().expect("waitable test children");
        let child = command.spawn().expect("spawn TERM-resistant collector");
        let process_id = child.id();
        let observer = pidfd(process_id as i32).expect("open observer pidfd");
        let guard = CollectorGuard::attach(child, waitability, CollectorFaults::default())
            .expect("attach collector guard");

        let started = Instant::now();
        let trace_length = TraceLengthSource::Fixed(0);
        let supervised = supervise_collector(&mut scope, guard, &trace_length, policy, &schedule);
        let status = supervised.status.expect("terminate limited collector");
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        assert_eq!(supervised.limits.duration, Some(Duration::ZERO));
        assert_eq!(supervised.limits.trace_bytes, None);
        assert_eq!(supervised.trace_len_error, None);
        assert!(started.elapsed() >= Duration::from_millis(10));
        assert_eq!(scope.recorder_stop(), RecorderStop::None);
        let gone = send_signal(&observer, 0).expect_err("collector must be gone");
        assert_eq!(gone.raw_os_error(), Some(libc::ESRCH));
        assert!(!std::path::PathBuf::from(format!("/proc/{process_id}")).exists());
        scope.restore_parent().expect("restore caller signal state");
    }

    #[test]
    fn trace_length_failure_is_supervision_error_not_a_limit_fact() {
        let _lock = signal_test_lock();
        let _original = OriginalSignalState::capture();
        let mut scope = SignalScope::new().expect("create signal scope");
        let policy = CaptureLimits {
            max_trace_bytes: Some(0),
            ..CaptureLimits::default()
        };
        let schedule = CaptureSchedule::before_spawn(policy).expect("build capture schedule");
        let waitability = ChildWaitability::before_spawn().expect("waitable test children");
        let child = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleeping collector fixture");
        let process_id = child.id();
        let observer = pidfd(process_id as i32).expect("open observer pidfd");
        let guard = CollectorGuard::attach(child, waitability, CollectorFaults::default())
            .expect("attach collector guard");
        let trace_length = TraceLengthSource::Failed("injected anchored fstat failure");

        let supervised = supervise_collector(&mut scope, guard, &trace_length, policy, &schedule);
        let status = supervised
            .status
            .expect("reap collector after fstat failure");
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        assert_eq!(supervised.limits, CaptureLimitFacts::default());
        assert!(
            supervised
                .trace_len_error
                .as_deref()
                .is_some_and(|error| error.contains("injected anchored fstat failure"))
        );
        assert_eq!(scope.recorder_stop(), RecorderStop::None);
        let gone = send_signal(&observer, 0).expect_err("collector must be gone");
        assert_eq!(gone.raw_os_error(), Some(libc::ESRCH));
        assert!(!std::path::PathBuf::from(format!("/proc/{process_id}")).exists());
        scope.restore_parent().expect("restore caller signal state");
    }

    #[test]
    fn child_signal_restore_failure_prevents_exec() {
        let _lock = signal_test_lock();
        let _original = OriginalSignalState::capture();
        let mut scope = SignalScope::new().expect("create signal scope");
        let mut child_signals = scope.child_state();
        child_signals.signals[0].number = -1;
        let marker = std::env::current_dir()
            .expect("current directory")
            .join("target")
            .join(format!("pre-exec-marker-{}", std::process::id()));
        let mut command = Command::new("sh");
        command.arg("-c").arg("touch \"$1\"").arg("sh").arg(&marker);
        // SAFETY: restoring copied signal state uses only signal syscalls and
        // OS-error construction after fork. The deliberately invalid signal
        // number is rejected by libc and never used as a Rust memory index.
        unsafe {
            command.pre_exec(move || child_signals.restore());
        }

        let error = command
            .spawn()
            .expect_err("invalid signal restore must fail before exec");
        assert_eq!(error.raw_os_error(), Some(libc::EINVAL));
        assert!(!marker.exists(), "target executed after pre_exec failure");
        scope.restore_parent().expect("restore caller signal state");
    }

    #[test]
    fn version_error_display_is_bounded_and_marks_truncation() {
        let mut stderr = b"BEGIN\n".to_vec();
        stderr.extend(std::iter::repeat_n(b'e', 10_000));
        stderr.extend_from_slice(b"\nSECRET-TAIL\n");
        let display = version_error_display(&stderr);
        assert!(display.len() <= VERSION_ERROR_DISPLAY_LIMIT);
        assert!(display.starts_with("BEGIN\n"));
        assert!(display.contains("[truncated; stderr was 10019 bytes]"));
        assert!(!display.contains("SECRET-TAIL"));
    }

    #[test]
    fn pending_recorder_signal_wins_over_ready_version_pidfd() {
        let _lock = signal_test_lock();
        let _original = OriginalSignalState::capture();
        set_test_signal(libc::SIGHUP, libc::SIG_DFL, false);
        let mut scope = SignalScope::new().expect("create signal scope");
        let parent = std::process::id() as libc::pid_t;
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("printf 'fixture-version\\n'")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_supervised_child(&mut command, scope.child_state(), parent);
        let waitability = ChildWaitability::before_spawn().expect("waitable test children");
        let child = command.spawn().expect("spawn completed version fixture");
        let mut guard = CollectorGuard::attach(child, waitability, CollectorFaults::default())
            .expect("attach version fixture");
        let stdout = guard.take_stdout().expect("take version stdout");
        let stderr = guard.take_stderr().expect("take version stderr");
        let mut ready = libc::pollfd {
            fd: guard.pidfd_raw().expect("version pidfd"),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `ready` is one initialized writable pollfd, and its pidfd
        // stays owned by guard for this bounded poll call.
        assert_eq!(unsafe { libc::poll(&mut ready, 1, 2_000) }, 1);
        send_to_current_thread(libc::SIGHUP);

        let outcome = supervise_version_probe(
            &mut scope,
            guard,
            CapturedPipe::new(stdout, "stdout").expect("capture stdout"),
            CapturedPipe::new(stderr, "stderr").expect("capture stderr"),
        )
        .expect("signal-priority supervision");
        assert!(matches!(
            outcome,
            VersionProbeOutcome::Cancelled {
                supervision_error: None
            }
        ));
        assert_eq!(scope.recorder_stop(), RecorderStop::Signal(libc::SIGHUP));
        scope.restore_parent().expect("restore caller signal state");
    }

    #[test]
    fn ready_probe_does_not_wait_for_descendant_pipe_eof() {
        let _lock = signal_test_lock();
        let _original = OriginalSignalState::capture();
        let mut scope = SignalScope::new().expect("create signal scope");
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let marker = std::env::current_dir()
            .expect("current directory")
            .join("target")
            .join(format!(
                "version-descendant-{}-{unique}",
                std::process::id()
            ));
        let script = concat!(
            "import os,sys,time\n",
            "pid=os.fork()\n",
            "if pid == 0:\n",
            " open(sys.argv[1],'w').write(str(os.getpid()))\n",
            " time.sleep(5)\n",
            " os._exit(0)\n",
            "while not os.path.exists(sys.argv[1]): time.sleep(0.001)\n",
            "os.write(1,b'fixture-version\\n')\n",
        );
        let parent = std::process::id() as libc::pid_t;
        let mut command = Command::new("python3");
        command
            .arg("-c")
            .arg(script)
            .arg(&marker)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_supervised_child(&mut command, scope.child_state(), parent);
        let waitability = ChildWaitability::before_spawn().expect("waitable test children");
        let child = command.spawn().expect("spawn descendant-pipe fixture");
        let mut guard = CollectorGuard::attach(child, waitability, CollectorFaults::default())
            .expect("attach descendant-pipe fixture");
        let stdout = guard.take_stdout().expect("take version stdout");
        let stderr = guard.take_stderr().expect("take version stderr");

        let started = Instant::now();
        let outcome = supervise_version_probe(
            &mut scope,
            guard,
            CapturedPipe::new(stdout, "stdout").expect("capture stdout"),
            CapturedPipe::new(stderr, "stderr").expect("capture stderr"),
        )
        .expect("supervise descendant-pipe fixture");
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(
            matches!(outcome, VersionProbeOutcome::Completed(ref bytes) if bytes == b"fixture-version\n")
        );

        let descendant = std::fs::read_to_string(&marker)
            .expect("read descendant pid")
            .parse::<i32>()
            .expect("parse descendant pid");
        let observer = pidfd(descendant).expect("open descendant pidfd");
        send_signal(&observer, libc::SIGKILL).expect("kill pipe-holding descendant");
        let deadline = Instant::now() + Duration::from_secs(2);
        while send_signal(&observer, 0).is_ok() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            send_signal(&observer, 0)
                .expect_err("descendant must be gone")
                .raw_os_error(),
            Some(libc::ESRCH)
        );
        std::fs::remove_file(&marker).expect("remove descendant marker");
        scope.restore_parent().expect("restore caller signal state");
    }
}

#[cfg(test)]
mod reducer_tests {
    use super::*;

    fn legacy_successful_reduction(
        root_outcome: Option<&str>,
        missing_generations: &[String],
        collector_exit: ProcessExit,
        recorder: RecorderStop,
        mut warnings: Vec<String>,
        trace_read_error: Option<String>,
    ) -> CaptureDecision {
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

        CaptureDecision {
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
                                    actual, expected,
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
                            assert_eq!(decision.target_exit_raw.as_deref(), root);
                            assert_eq!(
                                decision.collector,
                                CollectorFacts::Reaped(ProcessExit::Unknown)
                            );
                            assert!(decision.warnings.iter().any(|warning| {
                                warning == "collector status does not confirm target completion"
                            }));
                            match recorder {
                                RecorderStop::None => {
                                    assert_eq!(decision.state, CaptureState::CaptureFailed);
                                    assert_eq!(decision.cli_exit, 125);
                                }
                                RecorderStop::Signal(signal) => {
                                    assert_eq!(decision.state, CaptureState::Interrupted);
                                    assert_eq!(decision.cli_exit, 128 + signal);
                                }
                            }
                            assert_eq!(decision.completeness, CaptureCompleteness::Incomplete);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn normal_facts_have_the_frozen_decision() {
        let decision = reduce_capture(
            Some("+++ exited with 0 +++"),
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
                target: Some(ProcessExit::Exited(0)),
                target_exit_raw: Some("+++ exited with 0 +++".into()),
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
            Some("+++ killed by SIGTERM +++"),
            &[],
            CollectorFacts::Failed("collector wait failed".into()),
            RecorderStop::None,
            None,
            vec![],
            None,
        );
        let mut metadata = Metadata {
            schema_version: 3,
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
            (None, Some(libc::SIGTERM))
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
            Some("+++ killed by SIGFUTURE +++"),
            &[],
            CollectorFacts::Reaped(ProcessExit::Exited(9)),
            RecorderStop::None,
            None,
            vec![],
            None,
        );
        assert_eq!(unknown.target, Some(ProcessExit::Unknown));
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
        assert_eq!(missing.target, None);
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
            Some("+++ exited with 0 +++"),
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
            Some("+++ exited with 143 +++"),
            &[],
            CollectorFacts::Reaped(ProcessExit::Signaled(libc::SIGTERM)),
            RecorderStop::None,
            None,
            vec![],
            None,
        );
        assert_eq!(decision.target, Some(ProcessExit::Exited(143)));
        assert_eq!(
            decision.collector,
            CollectorFacts::Reaped(ProcessExit::Signaled(libc::SIGTERM))
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
            Some("+++ exited with 7 +++"),
            &[],
            CollectorFacts::Reaped(ProcessExit::Exited(7)),
            RecorderStop::Signal(libc::SIGINT),
            None,
            vec![],
            None,
        );
        assert_eq!(decision.target, Some(ProcessExit::Exited(7)));
        assert_eq!(
            decision.collector,
            CollectorFacts::Reaped(ProcessExit::Exited(7))
        );
        assert_eq!(decision.recorder, RecorderStop::Signal(libc::SIGINT));
        assert_eq!(decision.state, CaptureState::Interrupted);
        assert_eq!(decision.completeness, CaptureCompleteness::Incomplete);
        assert_eq!(decision.cli_exit, 128 + libc::SIGINT);
    }

    #[test]
    fn collector_failure_preserves_independent_target_facts() {
        let decision = reduce_capture(
            Some("+++ killed by SIGTERM +++"),
            &["2".into()],
            CollectorFacts::Failed("collector wait failed".into()),
            RecorderStop::None,
            None,
            vec!["parser warning".into()],
            Some("trace read failed".into()),
        );
        assert_eq!(decision.target, Some(ProcessExit::Signaled(libc::SIGTERM)));
        assert_eq!(
            decision.target_exit_raw.as_deref(),
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
        assert_eq!(decision.target, None);
        assert_eq!(decision.target_exit_raw, None);
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
            Some("+++ exited with 4 +++"),
            &[],
            CollectorFacts::Failed("collector wait failed".into()),
            RecorderStop::Signal(libc::SIGTERM),
            None,
            vec![],
            None,
        );
        assert_eq!(decision.target, Some(ProcessExit::Exited(4)));
        assert_eq!(decision.state, CaptureState::Interrupted);
        assert_eq!(decision.completeness, CaptureCompleteness::Incomplete);
        assert_eq!(decision.cli_exit, 128 + libc::SIGTERM);
    }

    #[test]
    fn recorder_supervision_failure_preserves_reaped_collector_fact() {
        let decision = reduce_capture(
            Some("+++ exited with 0 +++"),
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
            Some("+++ exited with 0 +++"),
            &[],
            CollectorFacts::Reaped(ProcessExit::Exited(0)),
            RecorderStop::Signal(libc::SIGTERM),
            Some("recorder signal supervision failed: injected read error".into()),
            vec![],
            None,
        );
        assert_eq!(interrupted.state, CaptureState::Interrupted);
        assert_eq!(interrupted.cli_exit, 128 + libc::SIGTERM);
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
            RecorderStop::Signal(libc::SIGHUP),
            None,
            vec![],
            None,
        );
        assert_eq!(decision.target, None);
        assert_eq!(decision.target_exit_raw, None);
        assert!(matches!(decision.collector, CollectorFacts::NotStarted(_)));
        assert_eq!(decision.state, CaptureState::Interrupted);
        assert_eq!(decision.completeness, CaptureCompleteness::Incomplete);
        assert_eq!(decision.cli_exit, 128 + libc::SIGHUP);
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
            root_outcome: Some("+++ exited with 0 +++"),
            missing_generations: &[],
            collector: CollectorFacts::Reaped(ProcessExit::Exited(0)),
            recorder: RecorderStop::None,
            limits: limits.clone(),
            recorder_supervision_error: None,
            warnings: vec![],
            trace_read_error: None,
        });
        assert_eq!(limited.target, Some(ProcessExit::Exited(0)));
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
            root_outcome: Some("+++ exited with 0 +++"),
            missing_generations: &[],
            collector: CollectorFacts::Failed("collector wait failed".into()),
            recorder: RecorderStop::Signal(libc::SIGTERM),
            limits,
            recorder_supervision_error: None,
            warnings: vec![],
            trace_read_error: None,
        });
        assert_eq!(interrupted.state, CaptureState::Interrupted);
        assert_eq!(interrupted.completeness, CaptureCompleteness::Incomplete);
        assert_eq!(interrupted.cli_exit, 128 + libc::SIGTERM);
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
    fn unrepresentable_monotonic_deadlines_fail_before_spawn() {
        let limits = CaptureLimits {
            max_duration: Some(Duration::MAX),
            ..CaptureLimits::default()
        };
        let error = CaptureSchedule::before_spawn(limits)
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
        let error = record(&storage, "invalid-limit", &command, &[], limits)
            .expect_err("invalid policy must fail before storage or target side effects");
        assert!(
            format!("{error:#}").contains("cannot be represented by the monotonic clock"),
            "{error:#}"
        );
        assert!(!sentinel.exists(), "target ran with an invalid limit");
        assert!(!storage.exists(), "storage was opened for an invalid limit");
    }
}

fn pidfd(pid: libc::pid_t) -> std::io::Result<OwnedFd> {
    // SAFETY: pidfd_open takes the promoted PID and zero flags, no pointers.
    // Linux validates the PID; successful nonnegative results are fresh fds.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0u32) };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        // SAFETY: the successful syscall returned a fresh descriptor with no
        // existing Rust owner; the transfer occurs exactly once.
        Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) })
    }
}
fn send_signal(fd: &OwnedFd, sig: i32) -> std::io::Result<()> {
    // SAFETY: `fd` keeps a pidfd alive through the syscall; signal is a scalar
    // validated by Linux, null siginfo requests standard kernel-generated
    // information, and zero flags are required. No Rust pointer is dereferenced.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            fd.as_raw_fd(),
            sig,
            std::ptr::null::<libc::siginfo_t>(),
            0u32,
        )
    };
    if rc < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod collector_guard_tests {
    use super::*;

    #[test]
    fn child_waitability_rejects_auto_reaping_and_external_handlers() {
        let mut action = default_signal_action().expect("initialize SIGCHLD fixture");
        assert!(ChildWaitability::from_action(&action).is_ok());
        action.sa_flags = libc::SA_NOCLDSTOP;
        assert!(ChildWaitability::from_action(&action).is_ok());
        action.sa_flags |= libc::SA_NOCLDWAIT;
        assert!(ChildWaitability::from_action(&action).is_err());
        action.sa_flags = 0;
        action.sa_sigaction = libc::SIG_IGN;
        assert!(ChildWaitability::from_action(&action).is_err());
        extern "C" fn external_handler(_signal: i32) {}
        action.sa_sigaction = external_handler as *const () as usize;
        assert!(ChildWaitability::from_action(&action).is_err());
    }

    fn sleeping_collector() -> (Child, ChildWaitability, u32, OwnedFd) {
        let waitability = ChildWaitability::before_spawn().expect("waitable test children");
        let child = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleeping collector fixture");
        let process_id = child.id();
        let observer = pidfd(process_id as i32).expect("pidfd support required by capture tests");
        (child, waitability, process_id, observer)
    }

    fn assert_collector_gone(process_id: u32, observer: &OwnedFd) {
        let error = send_signal(observer, 0).expect_err("collector must no longer accept signals");
        assert_eq!(error.raw_os_error(), Some(libc::ESRCH));
        assert!(
            !std::path::PathBuf::from(format!("/proc/{process_id}")).exists(),
            "collector {process_id} was not reaped"
        );
    }

    // The live child stands in for a process that inherited the old numeric
    // PID after ownership was lost. Observe through its independent pidfd and
    // clean it up before asserting, even when the regression kills it.
    fn assert_reused_pid_substitute_survived(process_id: u32, observer: &OwnedFd) {
        let remained_alive = send_signal(observer, 0).is_ok();
        if remained_alive {
            send_signal(observer, libc::SIGKILL).expect("clean up test-owned substitute");
        }
        let mut status = 0;
        // SAFETY: this PID belongs to the direct test child; `status` is valid
        // writable storage. No signaling uses its numeric PID, and ECHILD is
        // accepted when the old implementation has already reaped the child.
        let waited = unsafe { libc::waitpid(process_id as i32, &mut status, 0) };
        assert!(
            waited == process_id as i32
                || (waited == -1
                    && io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD)),
            "failed to reap test-owned substitute"
        );
        assert!(
            remained_alive,
            "lost collector identity signaled the reused-PID substitute"
        );
    }

    #[test]
    fn vanished_pidfd_target_never_signals_a_reused_pid_substitute() {
        let (child, waitability, process_id, observer) = sleeping_collector();
        let error = match CollectorGuard::attach(
            child,
            waitability,
            CollectorFaults {
                pidfd_errno: Some(libc::ESRCH),
                ..CollectorFaults::default()
            },
        ) {
            Ok(_) => panic!("injected vanished pidfd target unexpectedly attached"),
            Err(error) => error,
        };
        assert!(format!("{error:#}").contains("pidfd_open"));
        assert_reused_pid_substitute_survived(process_id, &observer);
    }

    #[test]
    fn echild_wait_never_signals_a_reused_pid_substitute() {
        let (child, waitability, process_id, observer) = sleeping_collector();
        let guard = CollectorGuard::attach(
            child,
            waitability,
            CollectorFaults {
                wait_errno: Some(libc::ECHILD),
                ..CollectorFaults::default()
            },
        )
        .expect("attach collector guard");
        let error = guard
            .finish()
            .expect_err("injected ECHILD must fail waiting");
        assert!(format!("{error:#}").contains("failed waiting"));
        assert_reused_pid_substitute_survived(process_id, &observer);
    }

    #[test]
    fn pidfd_failure_explicitly_terminates_and_reaps_spawned_collector() {
        let (child, waitability, process_id, observer) = sleeping_collector();
        let error = match CollectorGuard::attach(
            child,
            waitability,
            CollectorFaults {
                pidfd: true,
                ..CollectorFaults::default()
            },
        ) {
            Ok(_) => panic!("injected pidfd failure unexpectedly succeeded"),
            Err(error) => error,
        };
        assert!(
            format!("{error:#}").contains("injected collector pidfd failure"),
            "{error:#}"
        );
        assert_collector_gone(process_id, &observer);
    }

    #[test]
    fn descriptor_exhaustion_preserves_proven_child_cleanup() {
        let (child, waitability, process_id, observer) = sleeping_collector();
        let error = match CollectorGuard::attach(
            child,
            waitability,
            CollectorFaults {
                pidfd_errno: Some(libc::EMFILE),
                ..CollectorFaults::default()
            },
        ) {
            Ok(_) => panic!("injected descriptor exhaustion unexpectedly attached"),
            Err(error) => error,
        };
        assert!(format!("{error:#}").contains("pidfd_open"));
        assert_collector_gone(process_id, &observer);
    }

    #[test]
    fn wait_kill_and_reap_faults_are_reported_without_residual_collectors() {
        let cases = [
            (
                "wait",
                CollectorFaults {
                    wait: true,
                    ..CollectorFaults::default()
                },
                &["injected collector wait failure"][..],
            ),
            (
                "kill",
                CollectorFaults {
                    wait: true,
                    kill: true,
                    ..CollectorFaults::default()
                },
                &[
                    "injected collector wait failure",
                    "injected collector kill failure",
                ][..],
            ),
            (
                "reap",
                CollectorFaults {
                    wait: true,
                    reap: true,
                    ..CollectorFaults::default()
                },
                &[
                    "injected collector wait failure",
                    "injected collector reap failure",
                ][..],
            ),
        ];

        for (name, faults, expected) in cases {
            let (child, waitability, process_id, observer) = sleeping_collector();
            let guard =
                CollectorGuard::attach(child, waitability, faults).expect("attach collector guard");
            let error = guard
                .finish()
                .expect_err("fault must fail collector finish");
            let detail = format!("{error:#}");
            for needle in expected {
                assert!(detail.contains(needle), "{name}: {detail}");
            }
            assert_collector_gone(process_id, &observer);
        }
    }

    #[test]
    fn guard_retains_cleanup_ownership_on_early_return() {
        let (child, waitability, process_id, observer) = sleeping_collector();
        let guard = CollectorGuard::attach(child, waitability, CollectorFaults::default())
            .expect("attach collector guard");

        let error = guard.abort(anyhow::anyhow!("forced post-spawn early return"));

        assert!(format!("{error:#}").contains("forced post-spawn early return"));
        assert_collector_gone(process_id, &observer);
    }

    fn fail_while_storage_lock_is_held(
        root: &Path,
        observed: &mut Option<(u32, OwnedFd)>,
    ) -> Result<()> {
        let storage = store::open(root)?;
        let _lock = storage.lock()?;
        let (child, waitability, process_id, observer) = sleeping_collector();
        *observed = Some((process_id, observer));
        let guard = CollectorGuard::attach(
            child,
            waitability,
            CollectorFaults {
                wait: true,
                ..CollectorFaults::default()
            },
        )?;
        guard.finish()?;
        Ok(())
    }

    #[test]
    fn collector_failure_releases_storage_lock_for_reacquisition() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let root = std::env::current_dir()
            .expect("current directory")
            .join("target")
            .join(format!(
                "collector-guard-lock-{}-{unique}",
                std::process::id()
            ));
        let mut observed = None;

        let error = fail_while_storage_lock_is_held(&root, &mut observed)
            .expect_err("injected wait failure must escape the locked scope");
        assert!(
            format!("{error:#}").contains("injected collector wait failure"),
            "{error:#}"
        );
        let (process_id, observer) = observed.expect("collector observation");
        assert_collector_gone(process_id, &observer);

        let storage = store::open(&root).expect("reopen storage after failure");
        let lock = storage
            .lock()
            .expect("reacquire storage lock after failure");
        drop(lock);
        drop(storage);
        std::fs::remove_dir_all(&root).expect("remove collector guard test storage");
    }
}

#[cfg(test)]
mod completion_tests {
    use super::*;

    fn completion(trace: &str) -> (parse::Parsed, Completion) {
        let parsed = parse::parse_with_cwd(trace, "fixture", Some("/"));
        let completion = Completion::from_timeline(&parsed);
        (parsed, completion)
    }

    #[test]
    fn completion_uses_evidence_order_and_accepts_fresh_generations() {
        let (parsed, actual) = completion(concat!(
            "20 execve(\"/root\", [], 0 <unfinished ...>\n",
            "3 execve(\"/later\", [], 0) = 0\n",
            "20 <... execve resumed>) = 0\n",
            "20 +++ exited with 7 +++\n",
            "3 +++ exited with 0 +++\n",
        ));
        let root = &parsed.events[actual.root_exit.expect("root exit")];
        assert_eq!(
            (&root.pid, root.outcome.as_str()),
            (&"20".to_owned(), "+++ exited with 7 +++")
        );
        assert!(actual.missing_exits.is_empty());
        assert!(
            parsed
                .warnings
                .iter()
                .any(|warning| warning.contains("Unintroduced"))
        );

        let (parsed, actual) = completion(concat!(
            "20 execve(\"/root\", [], 0) = 0\n",
            "20 clone(flags=SIGCHLD <unfinished ...>\n",
            "3 execve(\"/child\", [], 0) = 0\n",
            "20 <... clone resumed>) = 3\n",
            "3 +++ exited with 0 +++\n",
            "20 clone(flags=SIGCHLD) = 3\n",
            "3 +++ exited with 0 +++\n",
            "20 +++ exited with 0 +++\n",
        ));
        assert!(actual.root_exit.is_some());
        assert!(actual.missing_exits.is_empty());
        assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
    }

    #[test]
    fn completion_never_borrows_an_exit_across_generations() {
        struct Case {
            name: &'static str,
            trace: &'static str,
            root_outcome: Option<&'static str>,
            missing: &'static [&'static str],
            identity_warning: Option<&'static str>,
        }
        let cases = [
            Case {
                name: "fresh reused child is independently missing",
                trace: concat!(
                    "20 execve(\"/root\", [], 0) = 0\n",
                    "20 fork() = 2\n",
                    "2 +++ exited with 0 +++\n",
                    "20 fork() = 2\n",
                    "20 +++ exited with 0 +++\n",
                ),
                root_outcome: Some("+++ exited with 0 +++"),
                missing: &["2#2"],
                identity_warning: None,
            },
            Case {
                name: "live collision leaves the displaced generation missing",
                trace: concat!(
                    "20 execve(\"/root\", [], 0) = 0\n",
                    "20 fork() = 2\n",
                    "20 fork() = 2\n",
                    "2 +++ exited with 0 +++\n",
                    "20 +++ exited with 0 +++\n",
                ),
                root_outcome: Some("+++ exited with 0 +++"),
                missing: &["2#1"],
                identity_warning: Some("LiveCollision"),
            },
            Case {
                name: "exit before exec belongs to the previous generation",
                trace: concat!(
                    "20 +++ exited with 0 +++\n",
                    "20 execve(\"/root\", [], 0) = 0\n",
                ),
                root_outcome: None,
                missing: &["20#2"],
                identity_warning: Some("Reappeared"),
            },
            Case {
                name: "second exit does not replace the root generation exit",
                trace: concat!(
                    "20 execve(\"/root\", [], 0) = 0\n",
                    "20 +++ exited with 4 +++\n",
                    "20 +++ exited with 7 +++\n",
                ),
                root_outcome: Some("+++ exited with 4 +++"),
                missing: &[],
                identity_warning: Some("Reappeared"),
            },
            Case {
                name: "single generation labels preserve legacy spelling and order",
                trace: concat!(
                    "20 execve(\"/root\", [], 0) = 0\n",
                    "20 fork() = 3\n",
                    "20 fork() = 100\n",
                    "20 +++ exited with 0 +++\n",
                ),
                root_outcome: Some("+++ exited with 0 +++"),
                missing: &["100", "3"],
                identity_warning: None,
            },
        ];

        for case in cases {
            let (parsed, actual) = completion(case.trace);
            assert_eq!(
                actual
                    .root_exit
                    .map(|index| parsed.events[index].outcome.as_str()),
                case.root_outcome,
                "{}",
                case.name
            );
            assert_eq!(actual.missing_exits, case.missing, "{}", case.name);
            assert_eq!(
                case.identity_warning.map(|needle| parsed
                    .warnings
                    .iter()
                    .any(|warning| warning.contains(needle))),
                case.identity_warning.map(|_| true),
                "{}: {:?}",
                case.name,
                parsed.warnings
            );
        }
    }

    #[test]
    fn unintroduced_generation_with_its_own_exit_is_complete_but_untrusted() {
        let (parsed, actual) = completion(concat!(
            "20 execve(\"/root\", [], 0) = 0\n",
            "21 open(\"/observed\", O_RDONLY) = 3\n",
            "21 +++ exited with 0 +++\n",
            "20 +++ exited with 0 +++\n",
        ));
        assert!(actual.root_exit.is_some());
        assert!(actual.missing_exits.is_empty());
        assert!(
            parsed
                .warnings
                .iter()
                .any(|warning| warning.contains("Unintroduced"))
        );
    }
}
