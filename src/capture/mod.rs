use crate::{
    model::{Metadata, bytes},
    parse, store,
};
use anyhow::{Context, Result, bail};
use std::{
    fs,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    os::unix::{
        ffi::OsStrExt,
        fs::PermissionsExt,
        process::{CommandExt, ExitStatusExt},
    },
    path::Path,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};
pub fn now() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
pub fn record(
    root: &Path,
    name: &str,
    command: &[std::ffi::OsString],
    selected: &[String],
) -> Result<i32> {
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
    let _lock = store::lock(root)?;
    if store::all(root)?
        .iter()
        .any(|(_, m)| m.name == name || m.id == name)
    {
        bail!("record name already exists: {name}");
    }
    let tracer = std::env::var_os("RUNDELTA_STRACE").unwrap_or_else(|| "strace".into());
    let version = Command::new(&tracer)
        .arg("--version")
        .output()
        .context("strace unavailable; install strace or set RUNDELTA_STRACE to its executable")?;
    if !version.status.success() {
        bail!("strace --version failed");
    }
    let id = format!(
        "{:x}-{}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
        std::process::id()
    );
    let dir = root.join("runs").join(&id);
    fs::create_dir(&dir)?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    fs::create_dir(dir.join("raw"))?;
    let mut m = Metadata {
        schema_version: 3,
        tool_version: env!("CARGO_PKG_VERSION").into(),
        id,
        name: name.into(),
        command: command.iter().map(|s| bytes(s.as_bytes())).collect(),
        cwd: bytes(std::env::current_dir()?.as_os_str().as_bytes()),
        started: now(),
        ended: None,
        backend: String::from_utf8_lossy(&version.stdout)
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
    store::metadata(&dir, &m)?;
    let trace = dir.join("raw/trace.log");
    let result = (|| -> Result<_> {
        let mut signals =
            signal_hook::iterator::Signals::new([libc::SIGINT, libc::SIGTERM, libc::SIGHUP])?;
        let handle = signals.handle();
        let parent = std::process::id() as libc::pid_t;
        // Require pidfd support before starting a target; no PID-based relay fallback.
        let self_fd = pidfd(parent).context("pidfd_open required (Linux 5.3+)")?;
        send_signal(&self_fd, 0).context("pidfd_send_signal required")?;
        drop(self_fd);
        let mut cmd = Command::new(&tracer);
        cmd.args(["-f","-q","-I","2","--kill-on-exit","-xx","-yy","-s","65535","-e","trace=execve,execveat,open,openat,openat2,access,faccessat,faccessat2,chdir,fchdir,clone,clone3,fork,vfork,unshare,setns,chroot,pivot_root","-o"]).arg(&trace).arg("--").args(command);
        // Only async-signal-safe syscalls in the post-fork/pre-exec child. The
        // parent check closes the race where the recorder dies before prctl.
        unsafe {
            cmd.pre_exec(move || {
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
        let mut child = cmd
            .spawn()
            .context("failed to start supervised strace; command was not run without tracing")?;
        let child_fd = match pidfd(child.id() as i32) {
            Ok(fd) => fd,
            Err(e) => {
                // The child is still ours and unreaped, so its PID cannot be reused.
                let _ = child.kill();
                let _ = child.wait();
                return Err(e.into());
            }
        };
        let relay = std::thread::spawn(move || {
            let mut last = None;
            for sig in signals.forever() {
                last = Some(sig);
                if let Err(e) = send_signal(&child_fd, sig)
                    && e.raw_os_error() != Some(libc::ESRCH)
                {
                    // Escalate on relay failure without using a potentially reused PID.
                    let _ = send_signal(&child_fd, libc::SIGKILL);
                }
            }
            last
        });
        let status = child.wait();
        handle.close();
        let interrupted = relay.join().unwrap_or(None);
        Ok((status?, interrupted))
    })();
    let raw = fs::read_to_string(&trace);
    let parsed = parse::parse_with_cwd(raw.as_deref().unwrap_or(""), "raw/trace.log", Some(&m.cwd));
    m.warnings = parsed.warnings;
    m.raw_lines = parsed.lines;
    m.parsed_events = parsed.events.len();
    m.ended = Some(now());
    if let Err(e) = &raw {
        m.warnings.push(format!("cannot read trace: {e}"));
    }
    let completion = Completion::from_events(&parsed.events);
    let root_exit = completion.root_exit;
    let code = match result {
        Ok((status, interrupted)) => {
            m.collector_exit_code = status.code();
            m.collector_signal = status.signal();
            if let Some(exit) = root_exit {
                m.target_exit_raw = Some(exit.outcome.clone());
                if let Some(code) = exit
                    .outcome
                    .strip_prefix("+++ exited with ")
                    .and_then(|s| s.strip_suffix(" +++"))
                    .and_then(|s| s.parse::<i32>().ok())
                {
                    m.exit_code = Some(code);
                } else if exit.outcome.starts_with("+++ killed by ") {
                    m.signal = signal_number(&exit.outcome);
                    if m.signal.is_none() {
                        m.warnings
                            .push("unrecognized target termination signal".into());
                    }
                }
            }
            let missing_exits = completion.missing_exits;
            if !missing_exits.is_empty() {
                m.warnings.push(format!(
                    "process completion not observed for PIDs: {}",
                    missing_exits.join(", ")
                ));
            }
            let target_code = m.signal.map(|s| 128 + s).or(m.exit_code);
            let collector_code = status.signal().map(|s| 128 + s).or(status.code());
            let consistent = if let Some(code) = m.exit_code {
                Some(status.code() == Some(code) && status.signal().is_none())
            } else {
                m.signal.map(|sig| status.signal() == Some(sig))
            };
            m.state = capture_state(root_exit.is_some(), consistent, interrupted.is_some()).into();
            if root_exit.is_none() {
                m.warnings
                    .push("root process completion not observed; capture incomplete".into());
            }
            if consistent == Some(false) {
                m.warnings
                    .push("collector status does not confirm target completion".into());
            }
            m.completeness = Some(
                if m.state != "completed" {
                    "incomplete"
                } else if m.warnings.is_empty() {
                    "complete_for_supported_events"
                } else {
                    "partial"
                }
                .into(),
            );
            if let Some(sig) = interrupted {
                128 + sig
            } else if m.state == "capture_failed" {
                125
            } else {
                target_code.or(collector_code).unwrap_or(125)
            }
        }

        Err(e) => {
            m.state = "capture_failed".into();
            m.completeness = Some("incomplete".into());
            m.warnings.push(format!("{e:#}"));
            125
        }
    };
    store::finish(&dir, &m, &parsed.events)?;
    eprintln!(
        "Recorded {:?}: {} ({}, {} events, {} warnings)\nEvidence: {}",
        name,
        m.id,
        m.state,
        m.parsed_events,
        m.warnings.len(),
        dir.display()
    );
    for w in &m.warnings {
        eprintln!("warning: {w}");
    }
    Ok(code)
}

/// Aggregate in event order; PID keys and exit events borrow the parsed trace.
/// The first successful exec selects the root; its last exit wins, even if
/// that exit preceded the exec. Merely appearing as child_pid requires an exit.
struct Completion<'a> {
    root_exit: Option<&'a crate::model::Event>,
    missing_exits: Vec<&'a str>,
}

impl<'a> Completion<'a> {
    fn from_events(events: &'a [crate::model::Event]) -> Self {
        let mut processes = std::collections::BTreeMap::new();
        let mut root_pid = None;
        for event in events {
            let last_exit = processes.entry(event.pid.as_str()).or_insert(None);
            if event.kind == "exit" {
                *last_exit = Some(event);
            }
            if root_pid.is_none() && event.kind == "exec" && event.outcome == "success" {
                root_pid = Some(event.pid.as_str());
            }
            if let Some(child) = &event.child_pid {
                processes.entry(child.as_str()).or_insert(None);
            }
        }
        let root_exit = root_pid.and_then(|pid| processes.get(pid).copied().flatten());
        // BTreeMap preserves the former BTreeSet's lexical warning order.
        let missing_exits = processes
            .into_iter()
            .filter_map(|(pid, exit)| exit.is_none().then_some(pid))
            .collect();
        Self {
            root_exit,
            missing_exits,
        }
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
    fn strace_realtime_names_use_kernel_base() {
        assert_eq!(signal_number("+++ killed by SIGRT_2 +++"), Some(34));
        assert_eq!(signal_number("+++ killed by SIGRT_32 +++"), Some(64));
        assert_eq!(signal_number("+++ killed by SIGRT_999 +++"), None);
    }
}

fn capture_state(
    root_observed: bool,
    confirmation: Option<bool>,
    interrupted: bool,
) -> &'static str {
    if interrupted {
        "interrupted"
    } else if !root_observed || confirmation == Some(false) {
        "capture_failed"
    } else {
        "completed"
    }
}
#[cfg(test)]
mod unknown_exit_tests {
    use super::*;
    #[test]
    fn unknown_target_status_is_not_collector_failure() {
        assert_eq!(capture_state(true, None, false), "completed");
        assert_eq!(capture_state(false, None, false), "capture_failed");
        assert_eq!(capture_state(true, Some(false), false), "capture_failed");
        assert_eq!(capture_state(true, Some(true), true), "interrupted");
    }
}

fn pidfd(pid: libc::pid_t) -> std::io::Result<OwnedFd> {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0u32) };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) })
    }
}
fn send_signal(fd: &OwnedFd, sig: i32) -> std::io::Result<()> {
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
mod completion_tests {
    use super::*;
    use crate::model::Event;
    use std::collections::BTreeSet;

    // Frozen pre-A3 selection predicates: compare event identity as well as PIDs.
    fn assert_legacy_equivalent(events: &[Event]) {
        let root = events
            .iter()
            .find(|e| e.kind == "exec" && e.outcome == "success")
            .map(|e| &e.pid);
        let exit = root.and_then(|pid| {
            events
                .iter()
                .rev()
                .find(|e| e.kind == "exit" && &e.pid == pid)
        });
        let missing: BTreeSet<_> = events
            .iter()
            .flat_map(|e| std::iter::once(e.pid.as_str()).chain(e.child_pid.as_deref()))
            .filter(|pid| !events.iter().any(|e| e.kind == "exit" && e.pid == *pid))
            .collect();
        let actual = Completion::from_events(events);
        assert_eq!(
            actual.root_exit.map(std::ptr::from_ref),
            exit.map(std::ptr::from_ref)
        );
        assert_eq!(
            actual.missing_exits,
            missing.into_iter().collect::<Vec<_>>()
        );
    }

    #[test]
    fn completion_matches_legacy_for_event_permutations_and_prefixes() {
        let mut events = parse::parse_with_cwd(
            "20 execve(\"/a\", [], 0) = 0\n3 execve(\"/b\", [], 0) = 0\n20 fork() = 100\n20 +++ exited with 7 +++\n3 +++ killed by SIGFUTURE +++\n20 +++ exited with 0 +++\n",
            "fixture", Some("/"),
        ).events;
        fn permute(events: &mut [Event], offset: usize) {
            if offset == events.len() {
                for len in 0..=events.len() {
                    assert_legacy_equivalent(&events[..len]);
                }
            } else {
                for index in offset..events.len() {
                    events.swap(offset, index);
                    permute(events, offset + 1);
                    events.swap(offset, index);
                }
            }
        }
        assert_eq!(events.len(), 6);
        permute(&mut events, 0);
    }
}
