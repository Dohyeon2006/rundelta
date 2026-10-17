//! Platform and saved-contract adapters for already-selected exit evidence.
//!
//! Legacy capture retains the host's signal ABI and accepted raw spellings.
//! Trace-v1 uses the model's fixed vocabulary. Both bind the original spelling to
//! an observed fact before entering pure reduction; a missing target never gains
//! an outcome from collector status. Wait-status conversion stays at this edge.

use super::reduction::{ObservedTargetExit, ProcessExit};
use crate::model::RecordingEvidenceContract;
use std::os::unix::process::ExitStatusExt;

/// Decode one present target observation at capture's frozen publication boundary.
/// Contract selection does not change root selection, evidence indices or raw text.
pub(super) fn decode_target(raw: &str, contract: RecordingEvidenceContract) -> ObservedTargetExit {
    let outcome = match contract {
        RecordingEvidenceContract::StructuredV3 => parse_process_exit(raw),
        RecordingEvidenceContract::TraceV1 => trace_v1_process_exit(raw),
    };
    ObservedTargetExit::new(raw, outcome)
}

/// Interpret the parser-selected root exit spelling, preserving unknown facts.
/// A collector result is deliberately not used to fill a missing target fact.
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

/// Decode the fixed saved contract rather than the current host's signal ABI.
fn trace_v1_process_exit(outcome: &str) -> ProcessExit {
    match crate::model::trace_v1_target_fields(outcome) {
        Ok((Some(code), None)) => ProcessExit::Exited(code),
        Ok((None, Some(signal))) => ProcessExit::Signaled(signal),
        _ => ProcessExit::Unknown,
    }
}

/// Preserve the wait result's signal/code distinction before any CLI projection.
pub(super) fn process_exit_from_status(status: &std::process::ExitStatus) -> ProcessExit {
    if let Some(code) = status.code() {
        ProcessExit::Exited(code)
    } else if let Some(signal) = status.signal() {
        ProcessExit::Signaled(signal)
    } else {
        ProcessExit::Unknown
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

/// Expose the unchanged host decoder only to the independent legacy oracle.
#[cfg(test)]
pub(super) fn legacy_signal_number(outcome: &str) -> Option<i32> {
    signal_number(outcome)
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
