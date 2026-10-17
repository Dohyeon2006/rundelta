//! Linux child and signal supervision with private resource ownership.
//!
//! This adapter owns guard/reaping authority, pidfds, signal dispositions and
//! masks, copied pre-exec state, version pipes and monotonic supervision. The
//! workflow borrows an opaque signal owner and receives independent facts; no
//! Child, descriptor, waitability token or ownership state crosses that API.
//! Storage is observed only through a mutable length callback. Parser/reducer
//! publication and explicit outer signal restoration remain with the workflow.

use super::{
    platform_exit::process_exit_from_status,
    policy::CaptureLimits,
    reduction::{CaptureLimitFacts, CollectorFacts, ProcessExit, RecorderStop},
};
use anyhow::{Context, Result, bail};
use std::{
    ffi::CString,
    fs::File,
    io::{self, Read},
    marker::PhantomData,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    os::unix::ffi::OsStrExt,
    os::unix::process::ExitStatusExt,
    process::{Command, ExitStatus},
    rc::Rc,
    time::{Duration, Instant},
};

// The storage-lock regression combines two real ownership boundaries. Its old
// body stays colocated as a test-only integration seam, never a production edge.
#[cfg(test)]
use crate::store;
#[cfg(test)]
use std::{
    os::unix::process::CommandExt,
    path::Path,
    process::{Child, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

/// Classify a complete task enumeration; even a late directory-read error must
/// fail closed rather than being discarded after counting an apparent one.
fn require_single_recorder_task(tasks: impl IntoIterator<Item = io::Result<()>>) -> Result<()> {
    let mut count = 0usize;
    for task in tasks {
        task.context("enumerate recorder threads")?;
        count += 1;
    }
    if count != 1 {
        bail!("capture requires a single-threaded recorder (observed {count} tasks)");
    }
    Ok(())
}

/// Reject an already multithreaded caller before storage or signal mutation.
/// This is an entry observation, not a lifetime lock: the caller must still
/// preserve exclusive dispositions/reaping and create no threads during capture.
pub(super) fn require_single_threaded_recorder() -> Result<()> {
    let tasks = std::fs::read_dir("/proc/self/task").context("inspect recorder thread count")?;
    require_single_recorder_task(tasks.map(|task| task.map(|_| ())))
}

/// Test-only failures at collector ownership boundaries.
/// Kill/reap flags report failure after the real cleanup operation; they test
/// diagnostic propagation, not kernel refusal to terminate a still-running child.
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

/// SIGCHLD precondition checked immediately before spawning a direct child in this
/// single-threaded supervisor. A default SIGCHLD disposition without
/// SA_NOCLDWAIT retains exited children until this owner reaps them; an ignored
/// signal, automatic reaping, or an inherited handler cannot provide that
/// guarantee. The check does not change what the target inherits.
///
/// This token records a disposition check, not process-wide exclusivity. The
/// fresh binary image must keep its no-thread/no-competing-reaper callgraph
/// valid until reaping. ChildLifecycleScope separately preserves the inherited
/// target action while normalizing only that exclusive owner's waitability.
struct ChildWaitability;

impl ChildWaitability {
    /// Inspect owner SIGCHLD immediately before creating a direct child.
    /// This check never mutates it; fresh-image normalization has a distinct owner.
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

/// Cleanup authority, distinct from whether a pidfd still exists.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ChildOwnership {
    /// No other reaper or auto-reaping signal disposition can release the PID.
    Waitable,
    /// A successful wait consumed the owned child.
    Reaped,
    /// ESRCH/ECHILD disproved ownership; neither cleanup nor Drop may signal it.
    Lost,
    /// Wait was not completed; outer signal restoration must not revive cleanup.
    Abandoned,
}

/// A cleanup failure that cannot establish that raw production has stopped.
/// It is intentionally a private typed error: formatting alone must not erase
/// the workflow's fail-closed prohibition on normalization/finalization.
#[derive(Debug)]
struct UnquiescedCollector {
    pid: libc::pid_t,
}

impl std::fmt::Display for UnquiescedCollector {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            output,
            "collector {} was not observed reaped; raw evidence may still be changing",
            self.pid
        )
    }
}

impl std::error::Error for UnquiescedCollector {}

/// Query the error chain without exposing process identity or cleanup authority.
pub(super) fn is_unquiesced(error: &anyhow::Error) -> bool {
    error.downcast_ref::<UnquiescedCollector>().is_some()
}

/// Reject detectable configuration that the fixed native adapter cannot honor.
/// Command offers no getter for stdio, pre_exec closures or env_clear: those
/// remain constraints of the two audited private call sites, not a general
/// substitution contract. Explicit environment edits and cwd are inspectable
/// and must fail before creating a child instead of being silently ignored.
fn admit_backend_command(command: &Command) -> io::Result<()> {
    if command.get_current_dir().is_some() || command.get_envs().next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "fixed backend launcher does not support Command cwd/environment mutation",
        ));
    }
    Ok(())
}

/// Parent-owned child identity and launch pipes, constructed before handshake.
/// Only the two fixed backend configurations use this adapter. It does not
/// implement arbitrary Command cwd/env/stdio/pre_exec mutation. All allocation
/// is complete before fork, and the child exits through execvp or _exit only.
struct CollectorChild {
    pid: libc::pid_t,
    stdout: Option<File>,
    stderr: Option<File>,
    exec_error: Option<File>,
    handshake: [u8; 8],
    handshake_len: usize,
    started: Instant,
}

impl CollectorChild {
    fn spawn(
        command: &Command,
        capture_pipes: bool,
        signals: ChildSignalState,
    ) -> io::Result<Self> {
        admit_backend_command(command)?;
        let started = Instant::now();
        let program = CString::new(command.get_program().as_bytes())?;
        let mut arguments = vec![program.clone()];
        for argument in command.get_args() {
            arguments.push(CString::new(argument.as_bytes())?);
        }
        let mut pointers: Vec<_> = arguments.iter().map(|argument| argument.as_ptr()).collect();
        pointers.push(std::ptr::null());
        let (exec_read, exec_write) = high_pipe()?;
        let output = if capture_pipes {
            Some(high_pipe()?)
        } else {
            None
        };
        let errors = if capture_pipes {
            Some(high_pipe()?)
        } else {
            None
        };
        set_nonblocking(exec_read.as_raw_fd())?;
        let parent = std::process::id() as libc::pid_t;
        // SAFETY: this is the admitted fresh single-thread owner with exclusive
        // signal/reaping authority. Parent allocations are already complete;
        // the child branch below performs only native operations and _exit.
        let process = unsafe { libc::fork() };
        if process < 0 {
            return Err(io::Error::last_os_error());
        }
        if process == 0 {
            // Never unwind/drop Rust resources in the forked child. The pipe
            // transfers setup errno independently of a target/collector status.
            let code = (|| -> io::Result<()> {
                close_child_fd(exec_read.as_raw_fd());
                if let Some((read, write)) = &output {
                    close_child_fd(read.as_raw_fd());
                    duplicate_child_fd(write.as_raw_fd(), libc::STDOUT_FILENO)?;
                    close_child_fd(write.as_raw_fd());
                }
                if let Some((read, write)) = &errors {
                    close_child_fd(read.as_raw_fd());
                    duplicate_child_fd(write.as_raw_fd(), libc::STDERR_FILENO)?;
                    close_child_fd(write.as_raw_fd());
                }
                // Match this build's Command::do_exec SIGPIPE normalization;
                // owner's Rust-startup ignored action is not target inheritance.
                // SAFETY: zero is the platform's initial sigaction storage;
                // sigemptyset below establishes its mask before publication.
                let mut pipe_action: libc::sigaction = unsafe { std::mem::zeroed() };
                pipe_action.sa_sigaction = libc::SIG_DFL;
                // SAFETY: pipe_action owns writable native sigset storage.
                if unsafe { libc::sigemptyset(&mut pipe_action.sa_mask) } != 0 {
                    return Err(io::Error::last_os_error());
                }
                set_signal_action(libc::SIGPIPE, &pipe_action)?;
                arm_supervised_child(signals, parent)?;
                // SAFETY: all CString/pointer allocations predate fork and
                // remain live; execvp inherits the installed raw environ and
                // preserves libc PATH lookup/ENOEXEC fallback used by Command.
                unsafe { libc::execvp(program.as_ptr(), pointers.as_ptr()) };
                Err(io::Error::last_os_error())
            })();
            let errno = code
                .err()
                .and_then(|error| error.raw_os_error())
                .unwrap_or(libc::EIO);
            let mut frame = *b"RDSP\0\0\0\0";
            frame[4..].copy_from_slice(&errno.to_ne_bytes());
            let mut written = 0;
            while written < frame.len() {
                // SAFETY: frame's remaining bytes are initialized and live;
                // this only writes our CLOEXEC child handshake descriptor.
                let count = unsafe {
                    libc::write(
                        exec_write.as_raw_fd(),
                        frame[written..].as_ptr().cast(),
                        frame.len() - written,
                    )
                };
                if count > 0 {
                    written += count as usize;
                } else if count < 0
                    && io::Error::last_os_error().raw_os_error() == Some(libc::EINTR)
                {
                    continue;
                } else {
                    break;
                }
            }
            // SAFETY: return from this branch would drop parent Rust resources;
            // _exit terminates this child image without allocation/unwinding.
            unsafe { libc::_exit(127) }
        }
        drop(exec_write);
        let stdout = output.map(|(read, write)| {
            drop(write);
            File::from(read)
        });
        let stderr = errors.map(|(read, write)| {
            drop(write);
            File::from(read)
        });
        Ok(Self {
            pid: process,
            stdout,
            stderr,
            exec_error: Some(File::from(exec_read)),
            handshake: [0; 8],
            handshake_len: 0,
            started,
        })
    }

    /// Adapt a real test child without pretending std::spawn has a pollable
    /// handshake. Its default Drop does not signal/wait; ownership moves here.
    #[cfg(test)]
    fn from_test_child(mut child: Child) -> Self {
        Self {
            pid: child.id() as libc::pid_t,
            stdout: child
                .stdout
                .take()
                .map(|pipe| File::from(OwnedFd::from(pipe))),
            stderr: child
                .stderr
                .take()
                .map(|pipe| File::from(OwnedFd::from(pipe))),
            exec_error: None,
            handshake: [0; 8],
            handshake_len: 0,
            started: Instant::now(),
        }
    }

    fn id(&self) -> u32 {
        self.pid as u32
    }

    /// One nonblocking wait call. EINTR remains an error for the deadline owner
    /// to retry after checking the clock; never retry forever inside this leaf.
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let mut status = 0;
        // SAFETY: this PID came from our fork/std test child, remains unreaped
        // under CollectorGuard authority, and status is writable native storage.
        let waited = unsafe { libc::waitpid(self.pid, &mut status, libc::WNOHANG) };
        if waited == 0 {
            Ok(None)
        } else if waited == self.pid {
            Ok(Some(ExitStatus::from_raw(status)))
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn kill(&self) -> io::Result<()> {
        // SAFETY: guard permits numeric fallback only while this unreaped
        // child retains exclusive waitable ownership; identity loss revokes it.
        if unsafe { libc::kill(self.pid, libc::SIGKILL) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn handshake_fd(&self) -> i32 {
        self.exec_error.as_ref().map_or(-1, AsRawFd::as_raw_fd)
    }

    /// Consume only available handshake bytes. EOF is a launch boundary, not a
    /// target success fact. Partial/malformed/extra errno frames fail closed.
    fn observe_handshake(&mut self) -> Result<()> {
        let Some(reader) = self.exec_error.as_mut() else {
            return Ok(());
        };
        loop {
            let mut bytes = [0; 9];
            match reader.read(&mut bytes) {
                Ok(0) => {
                    self.exec_error = None;
                    return decode_exec_handshake(&self.handshake[..self.handshake_len]);
                }
                Ok(count) => {
                    if self.handshake_len + count > self.handshake.len() {
                        bail!("invalid collector exec-error frame length");
                    }
                    self.handshake[self.handshake_len..self.handshake_len + count]
                        .copy_from_slice(&bytes[..count]);
                    self.handshake_len += count;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => return Ok(()),
                Err(error) => return Err(error).context("read collector exec-error handshake"),
            }
        }
    }
}

fn decode_exec_handshake(bytes: &[u8]) -> Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    if bytes.len() != 8 || &bytes[..4] != b"RDSP" {
        bail!("invalid collector exec-error frame");
    }
    let errno = i32::from_ne_bytes(bytes[4..].try_into().expect("exact checked frame"));
    if errno <= 0 {
        bail!("invalid collector exec-error errno");
    }
    Err(io::Error::from_raw_os_error(errno))
        .context("failed to start supervised strace before exec")
}

/// Raise newly created pipe ends above standard descriptors. This preserves
/// actually closed stdio and prevents dup2 from overwriting a different pipe.
fn high_pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut descriptors = [-1; 2];
    // SAFETY: descriptors is writable two-int storage. Successful pipe2 returns
    // exactly two new FDs, taken once below; all are CLOEXEC until dup2.
    if unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful pipe2 returned both uniquely owned descriptors.
    let read = unsafe { OwnedFd::from_raw_fd(descriptors[0]) };
    // SAFETY: this distinct pipe2 descriptor has no existing Rust owner.
    let write = unsafe { OwnedFd::from_raw_fd(descriptors[1]) };
    Ok((high_fd(read)?, high_fd(write)?))
}

fn high_fd(descriptor: OwnedFd) -> io::Result<OwnedFd> {
    if descriptor.as_raw_fd() >= 3 {
        return Ok(descriptor);
    }
    // SAFETY: F_DUPFD_CLOEXEC takes integer lower bound and makes a new owned FD.
    let duplicate = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
    if duplicate < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful duplication has no Rust owner; old low FD drops
    // on return, preserving its pre-pipe closed state without a leak.
    Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
}

fn close_child_fd(descriptor: i32) {
    // SAFETY: this only closes the fork child's copy of a known owned pipe FD;
    // no Rust Drop follows in this branch. Linux close EINTR must not be retried.
    unsafe { libc::close(descriptor) };
}

fn duplicate_child_fd(from: i32, to: i32) -> io::Result<()> {
    // SAFETY: from is an owned high pipe FD; to is a standard descriptor and
    // cannot alias another private pipe. dup2 clears the new stdio CLOEXEC.
    if unsafe { libc::dup2(from, to) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Owns one spawned collector until it has been explicitly reaped.
///
/// `pidfd` is `None` only while `attach` is acquiring the safe handle. A
/// successfully returned guard always owns both the child and its pidfd.
/// The pidfd supplies stable signal identity; successful waiting consumes child
/// ownership. An fd alone does not establish that this caller may reap a child.
struct CollectorGuard {
    child: CollectorChild,
    pidfd: Option<OwnedFd>,
    ownership: ChildOwnership,
    #[cfg(test)]
    faults: CollectorFaults,
}

impl CollectorGuard {
    /// Take cleanup authority before acquiring pidfd, closing post-spawn errors.
    /// Numeric-PID cleanup is permitted only while waitable ownership still holds.
    #[cfg(test)]
    fn attach(
        child: Child,
        waitability: ChildWaitability,
        faults: CollectorFaults,
    ) -> Result<Self> {
        Self::attach_owned(CollectorChild::from_test_child(child), waitability, faults)
    }

    fn attach_owned(
        child: CollectorChild,
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

    /// Consume authority through normal waiting, with explicit error cleanup.
    fn finish(mut self) -> Result<ExitStatus> {
        match self.wait_normally() {
            Ok(status) => Ok(status),
            Err(error) => {
                let cleanup = self.terminate_and_reap();
                let error = with_cleanup_error(error, cleanup);
                Err(self.revoke_unquiesced(error))
            }
        }
    }

    /// Preserve the primary failure together with any termination/reap failure.
    fn abort(mut self, error: anyhow::Error) -> anyhow::Error {
        let cleanup = self.terminate_and_reap();
        let error = with_cleanup_error(error, cleanup);
        self.revoke_unquiesced(error)
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
        let waited = self.wait_until(Instant::now() + CHILD_REAP_TIMEOUT);
        self.observe_wait(waited)
            .context("failed waiting for collector")
    }

    /// Invalidate cleanup authority as soon as wait consumes or disproves it.
    /// Other wait errors retain authority so explicit cleanup can still try.
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

    /// Attempt reaping even if termination reports an error.
    /// A cleanup error remains an error even when the child was then reaped;
    /// this result therefore does not expose every underlying wait observation.
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
        // KILL is a request, not a completion observation. Cleanup uses the
        // same finite WNOHANG retry protocol even if delivery failed; it cannot
        // manufacture quiescence from pidfd readiness or exceed its userspace
        // budget through an endless EINTR retry.
        let waited = self.wait_until(Instant::now() + CHILD_REAP_TIMEOUT);
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

    /// Borrow the descriptor for polling while this guard retains ownership.
    fn pidfd_raw(&self) -> Result<i32> {
        Ok(self
            .pidfd
            .as_ref()
            .context("collector pidfd missing after attachment")?
            .as_raw_fd())
    }

    /// Transfer pipe ownership to the version reader; the child remains guarded.
    fn take_stdout(&mut self) -> Result<File> {
        self.child
            .stdout
            .take()
            .context("supervised child stdout pipe missing")
    }

    /// Transfer the independent stderr pipe without changing child ownership.
    fn take_stderr(&mut self) -> Result<File> {
        self.child
            .stderr
            .take()
            .context("supervised child stderr pipe missing")
    }

    /// Signal the stable collector identity without treating delivery as an exit.
    /// ESRCH needs no further delivery; only a subsequent wait can supply status.
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

    /// Consume authority through KILL/reap; do not synthesize a policy exit code.
    fn cancel(mut self) -> Result<ExitStatus> {
        match self.terminate_and_reap_status() {
            Ok(status) => Ok(status),
            Err(error) => Err(self.revoke_unquiesced(error)),
        }
    }

    /// Remove authority before inherited SIGCHLD restoration can release a PID.
    fn revoke_unquiesced(&mut self, error: anyhow::Error) -> anyhow::Error {
        if self.ownership == ChildOwnership::Reaped {
            return error;
        }
        self.ownership = ChildOwnership::Abandoned;
        error.context(UnquiescedCollector {
            pid: self.child.pid,
        })
    }

    /// Every retry observes a fixed deadline, including endless wait EINTR or
    /// contradictory pidfd readiness. No blocking wait is used as a fallback.
    fn wait_until(&mut self, deadline: Instant) -> io::Result<ExitStatus> {
        loop {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "collector reaping exceeded 5-second userspace cleanup budget",
                ));
            }
            match self.child.try_wait() {
                Ok(Some(status)) => return Ok(status),
                Ok(None) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
            // A fixed small sleep also bounds retry CPU when an inconsistent
            // pidfd is permanently ready but wait does not supply a status.
            let timeout = deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(10))
                .as_millis() as i32;
            // SAFETY: zero descriptors permits null; poll is only this bounded
            // userspace wakeup. Signals stay under the enclosing recorder scope.
            unsafe { libc::poll(std::ptr::null_mut(), 0, timeout) };
        }
    }
}

impl Drop for CollectorGuard {
    fn drop(&mut self) {
        // Fallback cleanup cannot return diagnostics. Normal error paths use
        // abort/cancel explicitly; dropping a guard is not proof cleanup succeeded.
        if self.ownership == ChildOwnership::Waitable {
            let _ = self.terminate();
            if let Ok(Some(_)) = self.child.try_wait() {
                self.ownership = ChildOwnership::Reaped;
            } else {
                self.ownership = ChildOwnership::Abandoned;
            }
        }
    }
}

/// Keep the triggering failure foremost when explicit cleanup also fails.
fn with_cleanup_error(primary: anyhow::Error, cleanup: Result<()>) -> anyhow::Error {
    match cleanup {
        Ok(()) => primary,
        Err(cleanup) => {
            anyhow::anyhow!("{primary:#}; collector termination/reap also failed: {cleanup:#}")
        }
    }
}

#[cfg(test)]
mod child_protocol_tests {
    use super::*;

    #[test]
    fn exec_handshake_accepts_only_empty_success_or_exact_positive_errno() {
        assert!(decode_exec_handshake(b"").is_ok());
        let mut frame = *b"RDSP\0\0\0\0";
        frame[4..].copy_from_slice(&libc::ENOENT.to_ne_bytes());
        let error = decode_exec_handshake(&frame).unwrap_err();
        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().raw_os_error(),
            Some(libc::ENOENT)
        );
        for bytes in [
            b"R".as_slice(),
            b"WRNG\x02\0\0\0",
            b"RDSP\0\0\0\0",
            b"RDSP\xff\xff\xff\xff",
            b"RDSP\x02\0\0\0extra",
        ] {
            assert!(decode_exec_handshake(bytes).is_err(), "{bytes:?}");
        }
    }

    #[test]
    fn cancelled_preflight_preserves_unquiesced_type_and_reaped_diagnostics() {
        let success = cancelled_probe_after_cleanup(Ok(ExitStatus::from_raw(0))).unwrap();
        assert!(matches!(
            success,
            VersionProbeOutcome::Cancelled {
                supervision_error: None
            }
        ));
        let failed =
            cancelled_probe_after_cleanup(Err(anyhow::anyhow!("after real reap failed"))).unwrap();
        assert!(
            matches!(failed, VersionProbeOutcome::Cancelled { supervision_error: Some(ref error) } if error.contains("after real reap failed"))
        );
        let unsafe_to_publish =
            cancelled_probe_after_cleanup(Err(anyhow::Error::new(UnquiescedCollector { pid: 42 })))
                .err()
                .expect("unquiesced cancellation must remain an error");
        assert!(
            is_unquiesced(&unsafe_to_publish),
            "do not demote quiescence to text"
        );
    }

    #[test]
    fn typed_unquiesced_marker_survives_diagnostic_context() {
        let error =
            anyhow::Error::new(UnquiescedCollector { pid: 41 }).context("original wait failed");
        assert!(is_unquiesced(&error));
        assert!(
            !is_unquiesced(&anyhow::anyhow!("collector 41 was not observed reaped")),
            "wording is not a quiescence fact"
        );
    }
}

#[cfg(test)]
fn injected_collector_error(operation: &str) -> std::io::Error {
    std::io::Error::other(format!("injected collector {operation} failure"))
}

const CONTROL_SIGNALS: [i32; 3] = [libc::SIGHUP, libc::SIGINT, libc::SIGTERM];
const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
// Separate cleanup horizon; it is not a new formal capture/output quota.
const CHILD_REAP_TIMEOUT: Duration = VERSION_PROBE_TIMEOUT;
const VERSION_STREAM_LIMIT: usize = 64 * 1024;
const VERSION_ERROR_DISPLAY_LIMIT: usize = 4 * 1024;
const TRACE_LIMIT_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// A complete queried disposition plus one membership bit from the caller mask.
#[derive(Clone, Copy)]
struct SavedSignal {
    number: i32,
    action: libc::sigaction,
    blocked: bool,
}

/// Original process dispositions plus the caller thread's mask for pre-exec use.
#[derive(Clone, Copy)]
struct ChildSignalState {
    signals: [SavedSignal; CONTROL_SIGNALS.len()],
    mask: libc::sigset_t,
    inherited_chld: Option<libc::sigaction>,
}

impl ChildSignalState {
    /// Undo recorder-only signal changes in the forked child before backend exec.
    /// These copied values require no parent lock or allocation after fork.
    fn restore(self) -> std::io::Result<()> {
        for saved in self.signals {
            set_signal_action(saved.number, &saved.action)?;
        }
        if let Some(action) = self.inherited_chld {
            set_signal_action(libc::SIGCHLD, &action)?;
        }
        set_signal_mask(&self.mask)
    }
}

/// Configure the fork/exec boundary without starting a process.
/// The backend receives the caller's original signal state and a parent-death
/// signal; ordinary target-tree cleanup additionally relies on trusted strace.
#[cfg(test)]
fn configure_supervised_child(
    command: &mut Command,
    child_signals: ChildSignalState,
    parent: libc::pid_t,
) {
    // Test-only std::Command adapter exercises the same copied native setup.
    // SAFETY: captures only copied native state, and the callback performs no
    // allocation, locks or access to parent-thread state before exec.
    unsafe {
        command.pre_exec(move || arm_supervised_child(child_signals, parent));
    }
}

fn arm_supervised_child(child_signals: ChildSignalState, parent: libc::pid_t) -> io::Result<()> {
    // SAFETY: documented scalar PR_SET_PDEATHSIG arguments; no pointer retained.
    if unsafe {
        libc::prctl(
            libc::PR_SET_PDEATHSIG,
            libc::SIGKILL as libc::c_ulong,
            0,
            0,
            0,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: getppid has no arguments/pointers and observes the current child.
    if unsafe { libc::getppid() } != parent {
        return Err(io::Error::from_raw_os_error(libc::ECHILD));
    }
    child_signals.restore()
}

/// Deterministic test seams for partial setup and retryable restoration failures.
/// They do not cover every possible failure of the underlying signal syscalls.
#[derive(Clone, Copy, Default)]
struct SignalFaults {
    #[cfg(test)]
    action_index: Option<usize>,
    #[cfg(test)]
    signalfd: bool,
    #[cfg(test)]
    poll: bool,
    #[cfg(test)]
    observe_interrupted: bool,
    #[cfg(test)]
    restore_action_once: bool,
    #[cfg(test)]
    restore_mask_once: bool,
}

/// Own the fresh CLI image's child-waitability normalization.
///
/// Exec has removed prior host threads/reapers and caught handler addresses.
/// Backend children retain the resulting original cross-exec SIGCHLD action;
/// only their exclusive recorder uses default waitable children. This owner
/// remains alive through commit and the explicit restoration error boundary.
pub(super) struct ChildLifecycleScope {
    inherited: libc::sigaction,
    restored: bool,
    _exclusive: PhantomData<Rc<()>>,
}

impl ChildLifecycleScope {
    pub(super) fn new() -> Result<Self> {
        require_single_threaded_recorder()?;
        let inherited = saved_signal(libc::SIGCHLD, &current_signal_mask()?)?.action;
        if inherited.sa_sigaction != libc::SIG_DFL && inherited.sa_sigaction != libc::SIG_IGN {
            bail!("fresh capture owner inherited a live SIGCHLD handler");
        }
        let observable = default_signal_action()?;
        let mut scope = Self {
            inherited,
            restored: true,
            _exclusive: PhantomData,
        };
        set_signal_action(libc::SIGCHLD, &observable)
            .context("establish exclusive capture-owner SIGCHLD waitability")?;
        scope.restored = false;
        // Keep a failed postcondition inside an already constructed owner so
        // its original disposition is still restored on the failure path.
        if let Err(error) = ChildWaitability::before_spawn() {
            return match scope.restore() {
                Ok(()) => Err(error),
                Err(restoration) => Err(anyhow::anyhow!(
                    "{error:#}; additionally failed SIGCHLD setup restoration: {restoration:#}"
                )),
            };
        }
        Ok(scope)
    }

    pub(super) fn restore(&mut self) -> Result<()> {
        if !self.restored {
            set_signal_action(libc::SIGCHLD, &self.inherited)
                .context("restore capture-owner SIGCHLD disposition")?;
            self.restored = true;
        }
        Ok(())
    }
}

impl Drop for ChildLifecycleScope {
    fn drop(&mut self) {
        // An explicit failure remains an error even if this last retry works.
        let _ = self.restore();
    }
}

/// Owns the recorder's temporary signal mask, dispositions, and signalfd.
///
/// Watched signals are blocked before inherited `SIG_IGN` or handlers are
/// replaced with `SIG_DFL`. That makes all three signals observable through
/// signalfd without running an asynchronous handler. Parent and child restore
/// the exact saved state at their respective boundaries. After capture facts
/// freeze, late signals stay pending for the recorder's original disposition;
/// they are not target outcome facts and must not be forwarded to a former child.
///
/// This scope must stay on its creating thread: the saved mask is thread-local,
/// while action changes affect the whole process. The marker forbids Send/Sync;
/// it does not prevent another scope from changing the process-wide actions.
pub(super) struct SignalScope {
    fd: OwnedFd,
    saved: [SavedSignal; CONTROL_SIGNALS.len()],
    original_mask: libc::sigset_t,
    inherited_chld: Option<libc::sigaction>,
    first_observed: Option<i32>,
    frozen: bool,
    restored: bool,
    // Rc is used only as an auto-trait marker: no allocation or shared state.
    _thread_bound: PhantomData<Rc<()>>,
    #[cfg(test)]
    faults: SignalFaults,
    #[cfg(test)]
    restore_attempts: usize,
}

impl SignalScope {
    /// Borrow the exclusive fresh-image child owner without transferring it.
    pub(super) fn for_owner(owner: &ChildLifecycleScope) -> Result<Self> {
        let mut scope = Self::new()?;
        scope.inherited_chld = Some(owner.inherited);
        Ok(scope)
    }
    /// Acquire recorder signal state; the workflow retains this owner through storage finalization.
    pub(super) fn new() -> Result<Self> {
        // Recheck the admitted image after storage preflight. The image boundary
        // removed caller hooks; this inventory additionally rejects a now-visible
        // violation of the owner's no-thread/no-competing-reaper callgraph.
        require_single_threaded_recorder()?;
        ChildWaitability::before_spawn()?;
        Self::with_faults(SignalFaults::default())
    }

    /// Snapshot caller state before blocking controls and replacing actions.
    /// Partial setup errors use constructor rollback because no owning scope has
    /// yet been returned; that rollback's limitations are documented separately.
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
            inherited_chld: None,
            first_observed: None,
            frozen: false,
            restored: false,
            _thread_bound: PhantomData,
            #[cfg(test)]
            faults,
            #[cfg(test)]
            restore_attempts: 0,
        })
    }

    /// Snapshot inherited state, never the recorder's temporary blocked state.
    fn child_state(&self) -> ChildSignalState {
        ChildSignalState {
            signals: self.saved,
            mask: self.original_mask,
            inherited_chld: self.inherited_chld,
        }
    }

    /// Drain control records and latch the first one actually read from signalfd.
    /// Standard signals can coalesce; this is not a chronology of signal arrival.
    /// Additional records do not replace the latched cancellation fact.
    pub(super) fn observe(&mut self) -> Result<Option<i32>> {
        // Reading signalfd consumes pending signals. Once capture facts are
        // frozen (also during restoration retries), leave new signals pending
        // so restoring the saved action and mask preserves caller semantics.
        if self.frozen {
            return Ok(self.first_observed);
        }
        #[cfg(test)]
        if self.faults.observe_interrupted {
            return Err(io::Error::from_raw_os_error(libc::EINTR))
                .context("read recorder signalfd");
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
                if error.kind() == std::io::ErrorKind::WouldBlock {
                    break;
                }
                // EINTR is an incomplete observation, not proof of no pending
                // cancellation. In particular freeze must report it before
                // ending acquisition; retrying here could hide outer clocks.
                return Err(error).context("read recorder signalfd");
            }
            bail!("short read from recorder signalfd: {read} bytes");
        }
        Ok(self.first_observed)
    }

    /// Read the first observed cancellation without consuming pending signals.
    pub(super) fn recorder_stop(&self) -> RecorderStop {
        self.first_observed
            .map(RecorderStop::Signal)
            .unwrap_or(RecorderStop::None)
    }

    /// Avoid forwarding a signal the backend inherits as blocked or ignored.
    /// Cancellation then uses guard cleanup rather than waiting for that signal
    /// to terminate a child that preserves the caller's original semantics.
    fn requires_direct_termination(&self, signal: i32) -> bool {
        self.saved
            .iter()
            .find(|saved| saved.number == signal)
            .is_some_and(|saved| saved.blocked || saved.action.sa_sigaction == libc::SIG_IGN)
    }

    /// End acquisition of cancellation facts before finalization/restoration.
    /// Late signals remain pending for the original caller disposition instead
    /// of changing metadata after its capture decision has been made.
    pub(super) fn freeze(&mut self) -> Result<RecorderStop> {
        let observation = self.observe();
        // Even a failed last observation ends capture's ownership of signals.
        // Cleanup must not silently consume a signal on a later retry.
        self.frozen = true;
        observation?;
        Ok(self.recorder_stop())
    }

    /// Wait for owned descriptor readiness using a monotonic absolute deadline.
    /// Interrupted polls recompute the remainder; they do not restart the budget.
    /// Readiness still requires the caller to inspect each descriptor's events.
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
            if yield_interrupted_poll(deadline, Instant::now()) {
                // An interrupted poll establishes no readiness. Clear any
                // partial native output before the caller checks its clock or
                // observes signals again. None yields control without adding
                // a default capture duration; Some yields only at expiry.
                for descriptor in descriptors.iter_mut() {
                    descriptor.revents = 0;
                }
                return Ok(0);
            }
        }
    }

    /// Restore actions before unblocking the creating thread's original mask.
    /// An action error leaves control signals blocked, allowing a later retry
    /// without activating a temporary disposition. Frozen facts remain frozen.
    pub(super) fn restore_parent(&mut self) -> Result<()> {
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

/// An unlimited wait may yield after interruption without becoming a timeout;
/// a finite wait retries against its original absolute deadline only.
fn yield_interrupted_poll(deadline: Option<Instant>, now: Instant) -> bool {
    deadline.is_none_or(|deadline| now >= deadline)
}

impl Drop for SignalScope {
    fn drop(&mut self) {
        // An explicit restore reports errors to record(); Drop supplies one
        // further best-effort attempt without acquiring late cancellation facts.
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

/// Restore only actions already changed, attempting all of them in reverse order.
/// A reported error means original dispositions have not all been established.
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

/// Keep setup failure and rollback diagnostics distinct when no scope was built.
/// An error is not a guarantee that the caller's signal state was restored.
fn with_signal_setup_rollback(
    primary: anyhow::Error,
    saved: &[SavedSignal],
    changed: usize,
    original_mask: &libc::sigset_t,
) -> anyhow::Error {
    let actions = restore_signal_actions(saved, changed).err();
    // Match restore_parent's ordering: do not unblock a pending signal while
    // any temporary action remains installed. No owning scope exists to retry
    // constructor rollback, so return the error with controls still blocked.
    let mask_retained = actions.is_some();
    let mask = if mask_retained {
        None
    } else {
        set_signal_mask(original_mask).err().map(anyhow::Error::new)
    };
    match (actions, mask) {
        (None, None) => primary,
        (actions, mask) => anyhow::anyhow!(
            "{primary:#}; signal setup rollback failed: actions={}; mask={}",
            actions
                .map(|error| format!("{error:#}"))
                .unwrap_or_else(|| "ok".into()),
            mask.map(|error| format!("{error:#}")).unwrap_or_else(|| {
                if mask_retained {
                    "kept blocked".into()
                } else {
                    "ok".into()
                }
            })
        ),
    }
}

/// Preserve drain/action/mask diagnostics without letting one hide another.
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

/// Backend preflight completion, separate from any formal target capture.
/// A cancelled probe can admit an incomplete record only after observed reap;
/// an unquiesced preflight remains an error and admits no record or target.
pub(super) enum VersionProbeOutcome {
    Completed(Vec<u8>),
    Cancelled { supervision_error: Option<String> },
}

/// Bounded version output from one nonblocking pipe.
/// EOF belongs to this pipe, not necessarily to the direct child's lifetime:
/// a descendant may still hold another write end after the child is waitable.
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

    /// Exclude an exhausted pipe from polling so persistent HUP cannot spin.
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

    /// Consume currently available bytes only, stopping at EOF/WouldBlock.
    /// A stream limit prevents endless output from monopolizing supervision.
    fn drain(&mut self, deadline: Instant) -> Result<()> {
        if self.eof {
            return Ok(());
        }
        loop {
            // A nonblocking descriptor still can return endless EINTR. Recheck
            // the existing preflight clock before retrying instead of trapping
            // supervision inside this leaf until another syscall succeeds.
            if Instant::now() >= deadline {
                bail!(
                    "strace version probe timed out after {} seconds",
                    VERSION_PROBE_TIMEOUT.as_secs()
                );
            }
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

/// Fail preflight before backend/target startup if required pidfd syscalls fail.
/// Opening the recorder's own pidfd and sending signal zero creates no child
/// and does not deliver a signal; it does not guarantee later child attachment.
fn require_pidfd_capability() -> Result<()> {
    let self_fd =
        pidfd(std::process::id() as libc::pid_t).context("pidfd_open required (Linux 5.3+)")?;
    send_signal(&self_fd, 0).context("pidfd_send_signal required")
}

/// Bound diagnostic output independently of the larger captured-stream limit.
/// Truncation respects UTF-8 boundaries after lossy rendering and is explicit.
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

/// Cancel/reap preflight without manufacturing a collector or target exit fact.
/// Cleanup failures accompany cancellation rather than replacing its reason.
fn cancelled_version_probe(guard: CollectorGuard) -> Result<VersionProbeOutcome> {
    cancelled_probe_after_cleanup(guard.cancel())
}

/// Cancellation has precedence only after producer quiescence is established.
/// Keep the private error type intact: stringifying it here would incorrectly
/// admit a finalized cancellation record while the preflight child is unreaped.
fn cancelled_probe_after_cleanup(cleanup: Result<ExitStatus>) -> Result<VersionProbeOutcome> {
    let supervision_error = match cleanup {
        Ok(_) => None,
        Err(error) if is_unquiesced(&error) => return Err(error),
        Err(error) => Some(format!(
            "failed to terminate/reap cancelled version probe: {error:#}"
        )),
    };
    Ok(VersionProbeOutcome::Cancelled { supervision_error })
}

/// Give an already observable recorder cancellation precedence over probe error.
/// Without cancellation, retain the probe failure and explicit cleanup result.
fn fail_version_probe(
    scope: &mut SignalScope,
    guard: CollectorGuard,
    primary: anyhow::Error,
) -> Result<VersionProbeOutcome> {
    match scope.observe() {
        Ok(Some(_)) => cancelled_version_probe(guard),
        Ok(None) => Err(guard.abort(primary)),
        Err(observation) => Err(guard.abort(anyhow::anyhow!(
            "{primary:#}; additionally failed to observe recorder signal: {observation:#}"
        ))),
    }
}

/// Reap a ready direct probe after draining only currently available pipe bytes.
/// Check cancellation at both boundaries before admitting a completed preflight;
/// descendants holding pipes cannot extend this operation until EOF.
fn complete_version_probe(
    scope: &mut SignalScope,
    guard: CollectorGuard,
    stdout: &mut CapturedPipe<File>,
    stderr: &mut CapturedPipe<File>,
) -> Result<VersionProbeOutcome> {
    let deadline = guard.child.started + VERSION_PROBE_TIMEOUT;
    if let Err(error) = stdout.drain(deadline).and_then(|()| stderr.drain(deadline)) {
        return fail_version_probe(scope, guard, error);
    }
    if let Err(error) = scope.observe() {
        return Err(guard.abort(error));
    }
    if scope.first_observed.is_some() {
        return cancelled_version_probe(guard);
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

/// Poll probe identity, bounded output pipes, and recorder signals together.
/// Its existing timeout begins before private fork/pipe setup, including the
/// parent-side exec handshake. Individual kernel operations and scheduling still
/// have no unconditional wall-clock guarantee. Formal capture has its own clock.
fn supervise_version_probe(
    scope: &mut SignalScope,
    guard: CollectorGuard,
    mut stdout: CapturedPipe<File>,
    mut stderr: CapturedPipe<File>,
) -> Result<VersionProbeOutcome> {
    let probe_fd = match guard.pidfd_raw() {
        Ok(descriptor) => descriptor,
        Err(error) => return Err(guard.abort(error)),
    };
    let mut guard = guard;
    let deadline = guard.child.started + VERSION_PROBE_TIMEOUT;

    loop {
        if let Err(error) = scope.observe() {
            return Err(guard.abort(error));
        }
        if scope.first_observed.is_some() {
            return cancelled_version_probe(guard);
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
            libc::pollfd {
                fd: guard.child.handshake_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
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
                return cancelled_version_probe(guard);
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
            return cancelled_version_probe(guard);
        }

        if descriptors[2].revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) != 0
            && let Err(error) = stdout.drain(deadline)
        {
            return fail_version_probe(scope, guard, error);
        }
        if descriptors[3].revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) != 0
            && let Err(error) = stderr.drain(deadline)
        {
            return fail_version_probe(scope, guard, error);
        }
        if descriptors[4].revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) != 0
            && let Err(error) = guard.child.observe_handshake()
        {
            return fail_version_probe(scope, guard, error.context("failed to start supervised strace version probe; verify strace installation, RUNDELTA_STRACE, and process supervision support"));
        }
        if descriptors[1].revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) != 0 {
            if let Err(error) = guard.child.observe_handshake() {
                return fail_version_probe(scope, guard, error.context("failed to start supervised strace version probe; verify strace installation, RUNDELTA_STRACE, and process supervision support"));
            }
            // Drain only currently buffered bytes. Descendants may retain a pipe
            // fd, so waiting for EOF after the direct probe is pidfd-ready could
            // hang forever and would confuse descendant lifetime with the probe.
            return complete_version_probe(scope, guard, &mut stdout, &mut stderr);
        }
    }
}

/// Verify supervision support and ask the selected backend to parse its selector.
/// This runs only a version probe; preflight errors never run the target without
/// tracing, and cancellation is returned separately for the recording workflow.
pub(super) fn probe_backend(
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

    let mut command = Command::new(tracer);
    command.arg("-e").arg(trace_selector).arg("--version");
    let waitability = ChildWaitability::before_spawn()?;
    let child = CollectorChild::spawn(&command, true, scope.child_state()).context(
        "failed to start supervised strace version probe; verify strace installation, RUNDELTA_STRACE, and process supervision support",
    )?;
    let mut guard = CollectorGuard::attach_owned(child, waitability, CollectorFaults::default())?;
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

/// Monotonic deadlines anchored immediately before the formal collector spawn.
///
/// The version probe is intentionally outside this clock. Policy is validated
/// once before any side effect and checked again at the actual anchor. A later
/// representability failure still prevents target startup, but the writer may
/// already contain an unfinalized running record for failure inspection.
#[derive(Debug)]
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
            .max_duration()
            .map(|duration| {
                started
                    .checked_add(duration)
                    .context("validated capture duration")
            })
            .transpose()?;
        Ok(Self {
            duration_limit: limits.max_duration(),
            duration_deadline,
            terminate_grace: limits.terminate_grace(),
        })
    }

    /// Start grace from the first termination request, independently of the
    /// capture-duration anchor. Supervision retains that first deadline.
    fn termination_deadline(&self) -> Result<Instant> {
        Instant::now()
            .checked_add(self.terminate_grace)
            .context("capture termination grace cannot be represented by the monotonic clock")
    }
}

/// Facts returned by collector supervision without collapsing process status,
/// capture limits, or trace-length observation failures into one outcome.
/// `status` is the guard operation's result: a cleanup error may prevent a wait
/// status from being returned even after reaping. It is not a target outcome.
struct CollectorSupervision {
    status: Result<ExitStatus>,
    limits: CaptureLimitFacts,
    trace_len_error: Option<String>,
}

impl CollectorSupervision {
    fn finished(status: Result<ExitStatus>, limits: CaptureLimitFacts) -> Self {
        Self {
            status,
            limits,
            trace_len_error: None,
        }
    }

    /// Keep previously latched limits while explicitly cleaning up a failed loop.
    fn failed(guard: CollectorGuard, error: anyhow::Error, limits: CaptureLimitFacts) -> Self {
        Self::finished(Err(guard.abort(error)), limits)
    }

    /// A failed storage observation is a supervision error, not proof of excess.
    /// Keep its diagnostic independent of the cancellation/reap result.
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

/// Observe policy boundaries without deciding target/collector completion.
/// Only a successful anchored length read can establish a byte-limit fact;
/// a length error escapes so supervision can stop without guessing the size.
fn observe_capture_limits(
    trace_length: &mut impl FnMut() -> Result<u64>,
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
    if let Some(limit) = policy.max_trace_bytes()
        && !facts.trace_bytes_exceeded()
    {
        let observed =
            trace_length().context("inspect anchored trace length during collector supervision")?;
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

/// Choose the next required observation/termination wakeup, not a new budget.
/// Latching a boundary removes its polling deadline; termination grace remains
/// active while pending byte limits still require periodic inode observations.
fn supervision_poll_deadline(
    policy: CaptureLimits,
    schedule: &CaptureSchedule,
    facts: &CaptureLimitFacts,
    termination_deadline: Option<Instant>,
) -> Option<Instant> {
    let mut deadline = termination_deadline;
    if facts.duration_limit().is_none() {
        earlier_deadline(&mut deadline, schedule.duration_deadline);
    }
    if policy.max_trace_bytes().is_some() && !facts.trace_bytes_exceeded() {
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
    trace_length: &mut impl FnMut() -> Result<u64>,
    policy: CaptureLimits,
    schedule: &CaptureSchedule,
) -> CollectorSupervision {
    let collector_fd = match guard.pidfd_raw() {
        Ok(descriptor) => descriptor,
        Err(error) => {
            return CollectorSupervision::failed(guard, error, CaptureLimitFacts::default());
        }
    };
    let mut guard = guard;
    let mut facts = CaptureLimitFacts::default();
    // These latches have different meanings: one termination grace is shared
    // by cancellation/limits, recorder delivery is forwarded at most once, and
    // pidfd readiness still needs the next loop's higher-priority observations.
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
            // Do not restart grace when another limit crosses or cancellation
            // was already forwarded; doing so could postpone cleanup indefinitely.
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
            if let Err(error) = guard.child.observe_handshake() {
                return CollectorSupervision::failed(
                    guard,
                    error.context(
                        "failed to start supervised strace; command was not run without tracing",
                    ),
                    facts,
                );
            }
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
            libc::pollfd {
                fd: guard.child.handshake_fd(),
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
        if descriptors[2].revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) != 0
            && let Err(error) = guard.child.observe_handshake()
        {
            return CollectorSupervision::failed(
                guard,
                error.context(
                    "failed to start supervised strace; command was not run without tracing",
                ),
                facts,
            );
        }
        collector_ready =
            descriptors[1].revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) != 0;
    }
}

/// Declarative formal-capture arguments; no child or descriptor escapes it.
pub(super) struct CollectorLaunch<'a> {
    tracer: &'a std::ffi::OsStr,
    trace_selector: &'a str,
    trace: &'a std::path::Path,
    command: &'a [std::ffi::OsString],
    limits: CaptureLimits,
}

impl<'a> CollectorLaunch<'a> {
    /// Bind command configuration without creating deadlines or starting a child.
    pub(super) fn new(
        tracer: &'a std::ffi::OsStr,
        trace_selector: &'a str,
        trace: &'a std::path::Path,
        command: &'a [std::ffi::OsString],
        limits: CaptureLimits,
    ) -> Self {
        Self {
            tracer,
            trace_selector,
            trace,
            command,
            limits,
        }
    }
}

/// Independent facts returned after formal supervision has consumed child authority.
#[derive(Debug)]
pub(super) struct FormalOutcome {
    collector: CollectorFacts,
    limits: CaptureLimitFacts,
    trace_len_error: Option<String>,
}

impl FormalOutcome {
    /// Transfer only observed facts; ownership, wait status and descriptors stay private.
    pub(super) fn into_facts(self) -> (CollectorFacts, CaptureLimitFacts, Option<String>) {
        (self.collector, self.limits, self.trace_len_error)
    }
}

/// Launch and supervise the formal collector under the existing two error layers.
///
/// Deadline representability fails before Command construction. Reaped startup
/// failures may become collector-failure facts; unquiesced errors must leave the
/// workflow before raw reading or finalization. Length is a narrow mutable
/// observation port: disabled byte policy never reads it, and a first observed
/// excess stops periodic reads. The caller
/// retains storage and its distinct final-length check after this operation.
pub(super) fn capture_collector(
    scope: &mut SignalScope,
    launch: CollectorLaunch<'_>,
    mut trace_length: impl FnMut() -> Result<u64>,
) -> Result<FormalOutcome> {
    let schedule = CaptureSchedule::before_spawn(launch.limits)?;
    let result = (|| -> Result<CollectorGuard> {
        let mut cmd = Command::new(launch.tracer);
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
        .arg(launch.trace_selector)
        .arg("-o")
        .arg(launch.trace)
        .arg("--")
        .args(launch.command);
        let waitability = ChildWaitability::before_spawn()?;
        let child = CollectorChild::spawn(&cmd, false, scope.child_state())
            .context("failed to start supervised strace; command was not run without tracing")?;
        CollectorGuard::attach_owned(child, waitability, CollectorFaults::default())
    })();
    match result {
        Ok(guard) => {
            let supervision =
                supervise_collector(scope, guard, &mut trace_length, launch.limits, &schedule);
            let collector = match supervision.status {
                Err(error) if is_unquiesced(&error) => return Err(error),
                Ok(status) => CollectorFacts::Reaped(process_exit_from_status(&status)),
                Err(error) => CollectorFacts::Failed(format!("{error:#}")),
            };
            Ok(FormalOutcome {
                collector,
                limits: supervision.limits,
                trace_len_error: supervision.trace_len_error,
            })
        }
        Err(error) if is_unquiesced(&error) => Err(error),
        Err(error) => Ok(FormalOutcome {
            collector: CollectorFacts::Failed(format!("{error:#}")),
            limits: CaptureLimitFacts::default(),
            trace_len_error: None,
        }),
    }
}

/// Exercise the same early mathematical failure from a workflow boundary test.
#[cfg(test)]
pub(super) fn validate_formal_schedule(limits: CaptureLimits) -> Result<()> {
    CaptureSchedule::before_spawn(limits).map(|_| ())
}

/// Own a stable kernel process handle; numeric PID authority is checked elsewhere.
/// The returned descriptor closes with its Rust owner rather than with reaping.
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
/// Request delivery to the pidfd identity without releasing or reaping the child.
/// Success is delivery acceptance, not evidence of target termination/completion.
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
mod signal_scope_tests {
    // The Rust test harness has other threads. These mutex-serialized fixtures
    // exercise setup, restoration and state through with_faults; isolated CLI
    // tests exercise new's recorder-workflow acquisition guards instead.
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::sync::{Mutex, MutexGuard};

    #[test]
    fn formal_launch_keeps_schedule_errors_distinct_from_spawn_failure_facts() {
        let _lock = signal_test_lock();
        let _original = OriginalSignalState::capture();
        let mut scope = SignalScope::with_faults(SignalFaults::default())
            .expect("acquire fixture signal scope");
        let calls = std::cell::Cell::new(0usize);
        // A NUL forces spawn failure independently of filesystem/backend setup.
        let backend = std::ffi::OsStr::new("fixture-backend\0");
        let trace = std::path::Path::new("/unused-fixture-trace");
        let command = [std::ffi::OsString::from("unused-fixture-target")];
        let impossible =
            CaptureLimits::from_durations(Some(Duration::MAX), None, Duration::from_secs(2));
        let error = capture_collector(
            &mut scope,
            CollectorLaunch::new(backend, "trace=process", trace, &command, impossible),
            || {
                calls.set(calls.get() + 1);
                Ok(0)
            },
        )
        .expect_err("impossible schedule is a workflow error before formal startup");
        assert!(
            format!("{error:#}")
                .contains("capture duration limit cannot be represented by the monotonic clock")
        );
        assert_eq!(calls.get(), 0);

        let (collector, limits, length_error) = capture_collector(
            &mut scope,
            CollectorLaunch::new(
                backend,
                "trace=process",
                trace,
                &command,
                CaptureLimits::default(),
            ),
            || {
                calls.set(calls.get() + 1);
                Ok(0)
            },
        )
        .expect("spawn failure remains a recordable collector fact")
        .into_facts();
        assert!(matches!(collector, CollectorFacts::Failed(ref error)
            if error.contains("failed to start supervised strace; command was not run without tracing")));
        assert_eq!(limits, CaptureLimitFacts::default());
        assert_eq!(length_error, None);
        assert_eq!(calls.get(), 0);
        assert_eq!(scope.recorder_stop(), RecorderStop::None);
        scope
            .restore_parent()
            .expect("restore fixture caller state");
    }

    #[test]
    fn backend_command_admission_accepts_fixed_argv_and_rejects_detectable_mutations() {
        let mut command = Command::new("backend");
        command.args(["--version", "native-argument"]);
        admit_backend_command(&command).expect("fixed argv-only backend configuration");
        let mut changed = Command::new("backend");
        changed.current_dir("/fixture-cwd");
        assert_eq!(
            admit_backend_command(&changed).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        let mut changed = Command::new("backend");
        changed.env("FIXTURE", "value");
        assert_eq!(
            admit_backend_command(&changed).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        let mut changed = Command::new("backend");
        changed.env_remove("FIXTURE");
        assert_eq!(
            admit_backend_command(&changed).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn interrupted_poll_yields_without_restarting_or_adding_a_budget() {
        let now = Instant::now();
        assert!(yield_interrupted_poll(None, now));
        assert!(yield_interrupted_poll(Some(now), now));
        assert!(!yield_interrupted_poll(
            Some(now + Duration::from_secs(1)),
            now
        ));
    }

    #[test]
    fn interrupted_signal_observation_preserves_latched_fact_and_freeze_error() {
        let _lock = signal_test_lock();
        let _original = OriginalSignalState::capture();
        let mut scope = SignalScope::with_faults(SignalFaults::default())
            .expect("acquire fixture signal scope");
        let saved = scope.saved;
        let mask = scope.original_mask;
        send_to_current_thread(libc::SIGHUP);
        assert_eq!(scope.observe().unwrap(), Some(libc::SIGHUP));
        scope.faults.observe_interrupted = true;
        let error = scope
            .observe()
            .expect_err("EINTR is incomplete observation");
        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().raw_os_error(),
            Some(libc::EINTR)
        );
        assert_eq!(scope.recorder_stop(), RecorderStop::Signal(libc::SIGHUP));
        assert!(
            scope.freeze().is_err(),
            "freeze must not invent a completed observation"
        );
        assert!(scope.frozen);
        assert_eq!(scope.recorder_stop(), RecorderStop::Signal(libc::SIGHUP));
        // Acquisition has ended even though its final read failed. Restoration
        // must neither retry that read nor overwrite the existing stop fact.
        scope
            .restore_parent()
            .expect("restore after incomplete final observation");
        assert_saved_state(&saved, &mask);
    }

    // Fixture-only serialization of these tests' process-wide action changes.
    // It supplies no production concurrency guarantee; test signals are sent to
    // the fixture thread explicitly rather than to an arbitrary unblocked thread.
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

    fn assert_signal_mask(actual: &libc::sigset_t, expected: &libc::sigset_t) {
        // Compare every Linux signal membership, not struct bytes: libc/kernel
        // padding and unused storage do not describe signal-mask semantics.
        for number in 1..=libc::SIGRTMAX() {
            // SAFETY: both masks came from successful libc initialization/query;
            // the loop covers valid Linux signal numbers only.
            let (actual_member, expected_member) = unsafe {
                (
                    libc::sigismember(actual, number),
                    libc::sigismember(expected, number),
                )
            };
            assert!(actual_member >= 0 && expected_member >= 0);
            assert_eq!(
                actual_member, expected_member,
                "signal mask member {number}"
            );
        }
    }

    fn assert_action(actual: &SavedSignal, expected: &SavedSignal) {
        assert_eq!(actual.action.sa_sigaction, expected.action.sa_sigaction);
        assert_eq!(actual.action.sa_flags, expected.action.sa_flags);
        assert_signal_mask(&actual.action.sa_mask, &expected.action.sa_mask);
        assert_eq!(actual.blocked, expected.blocked);
    }

    fn assert_saved_state(expected: &[SavedSignal], expected_mask: &libc::sigset_t) {
        let mask = current_signal_mask().expect("read restored mask");
        assert_signal_mask(&mask, expected_mask);
        for saved in expected {
            let actual = saved_signal(saved.number, &mask).expect("read restored action");
            assert_action(&actual, saved);
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
    fn signal_scope_cannot_cross_or_share_threads() {
        // Inference becomes ambiguous if SignalScope gains Send/Sync. This
        // compile-time regression needs no unsafe impl or assertion dependency.
        trait AmbiguousIfSend<A> {
            fn check() {}
        }
        impl<T: ?Sized> AmbiguousIfSend<()> for T {}
        enum ForbiddenSend {}
        impl<T: ?Sized + Send> AmbiguousIfSend<ForbiddenSend> for T {}
        let _ = <SignalScope as AmbiguousIfSend<_>>::check;

        trait AmbiguousIfSync<A> {
            fn check() {}
        }
        impl<T: ?Sized> AmbiguousIfSync<()> for T {}
        enum ForbiddenSync {}
        impl<T: ?Sized + Sync> AmbiguousIfSync<ForbiddenSync> for T {}
        let _ = <SignalScope as AmbiguousIfSync<_>>::check;
    }

    #[test]
    fn restoration_preserves_action_flags_action_masks_and_entire_thread_mask() {
        let _lock = signal_test_lock();
        let original = OriginalSignalState::capture();
        let watched = control_signal_set().expect("build fixture signal set");
        block_signal_set(&watched).expect("block controls before fixture setup");
        extern "C" fn fixture_handler(_signal: i32) {}
        let mut action = default_signal_action().expect("build fixture action");
        action.sa_sigaction = fixture_handler as *const () as usize;
        action.sa_flags = libc::SA_RESTART | libc::SA_NODEFER;
        // SAFETY: libc initialized both masks; all supplied signals are valid.
        unsafe {
            assert_eq!(libc::sigaddset(&mut action.sa_mask, libc::SIGUSR2), 0);
            assert_eq!(libc::sigaddset(&mut action.sa_mask, libc::SIGTERM), 0);
        }
        for number in CONTROL_SIGNALS {
            set_signal_action(number, &action).expect("install fixture action");
        }
        let mut fixture_mask = current_signal_mask().expect("read fixture mask");
        // Include non-control signals so restoring only HUP/INT/TERM fails.
        // SAFETY: the queried mask is initialized and every signal is valid.
        unsafe {
            assert_eq!(libc::sigdelset(&mut fixture_mask, libc::SIGINT), 0);
            assert_eq!(libc::sigaddset(&mut fixture_mask, libc::SIGUSR1), 0);
            assert_eq!(libc::sigdelset(&mut fixture_mask, libc::SIGUSR2), 0);
        }
        set_signal_mask(&fixture_mask).expect("install fixture mask");
        // Query normalized libc actions rather than assuming sa_flags bytes or
        // padding remain identical to the structure passed into sigaction.
        let expected_mask = current_signal_mask().expect("query fixture mask");
        let expected = CONTROL_SIGNALS
            .map(|number| saved_signal(number, &expected_mask).expect("query fixture action"));
        let disturb_unrelated_mask = || {
            let mut active_mask = current_signal_mask().expect("read active fixture mask");
            // Force restoration to recover saved non-control bits, rather than
            // passing because they happened to stay untouched during the scope.
            // SAFETY: the queried mask is initialized and both signals are valid.
            unsafe {
                assert_eq!(libc::sigdelset(&mut active_mask, libc::SIGUSR1), 0);
                assert_eq!(libc::sigaddset(&mut active_mask, libc::SIGUSR2), 0);
            }
            set_signal_mask(&active_mask).expect("disturb unrelated fixture mask");
        };
        {
            let mut scope = SignalScope::with_faults(SignalFaults::default())
                .expect("create child-state fixture scope");
            disturb_unrelated_mask();
            // Exercise the pre-exec restoration helper against an active scope.
            // No signal is sent; this unit test uses its own thread in place of
            // a forked child so all queried state can be asserted directly.
            scope
                .child_state()
                .restore()
                .expect("restore copied child state");
            assert_saved_state(&expected, &expected_mask);
            scope.restore_parent().expect("release fixture scope");
        }
        for (faults, first_restore_fails) in [
            (SignalFaults::default(), false),
            (
                SignalFaults {
                    restore_action_once: true,
                    ..SignalFaults::default()
                },
                true,
            ),
            (
                SignalFaults {
                    restore_mask_once: true,
                    ..SignalFaults::default()
                },
                true,
            ),
        ] {
            let mut scope = SignalScope::with_faults(faults).expect("create fixture scope");
            let child = scope.child_state();
            assert_signal_mask(&child.mask, &expected_mask);
            for (actual, expected) in child.signals.iter().zip(&expected) {
                assert_action(actual, expected);
            }
            disturb_unrelated_mask();
            if first_restore_fails {
                scope
                    .restore_parent()
                    .expect_err("injected restore failure");
                assert_control_signals_blocked();
            }
            scope
                .restore_parent()
                .expect("restore complete fixture state");
            assert_saved_state(&expected, &expected_mask);
        }
        drop(original);
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
            assert_saved_state(&expected, &expected_mask);
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

        let mut scope =
            SignalScope::with_faults(SignalFaults::default()).expect("create signal scope");
        assert!(scope.requires_direct_termination(libc::SIGHUP));
        send_to_current_thread(libc::SIGHUP);
        assert_eq!(
            scope.observe().expect("observe ignored HUP"),
            Some(libc::SIGHUP)
        );
        scope.restore_parent().expect("restore caller signal state");
        assert_saved_state(&expected, &expected_mask);
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

        let mut scope =
            SignalScope::with_faults(SignalFaults::default()).expect("create signal scope");
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
        assert_saved_state(&expected, &expected_mask);
        drop(scope);
        drop(original);
    }

    #[test]
    fn first_observed_signal_is_stable_across_later_observations() {
        let _lock = signal_test_lock();
        let _original = OriginalSignalState::capture();
        let mut scope =
            SignalScope::with_faults(SignalFaults::default()).expect("create signal scope");

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

        // Establish actions through libc before saving the retry expectation.
        // An exec-inherited default can omit libc's signal-restorer flag; the
        // first sigaction installation supplies it. Query that explicit initial
        // state so test order cannot choose whether the complete flags match.
        // The raw original snapshot remains the cleanup guard, and this setup
        // must preserve every handler and action/thread mask membership.
        let watched = control_signal_set().expect("build explicit retry fixture signal set");
        block_signal_set(&watched).expect("block controls during retry fixture setup");
        for saved in &original.saved {
            set_signal_action(saved.number, &saved.action).expect("install retry fixture action");
        }
        set_signal_mask(&original.mask).expect("restore retry fixture thread mask");
        let expected_mask = current_signal_mask().expect("query explicit retry fixture mask");
        assert_signal_mask(&expected_mask, &original.mask);
        let expected = CONTROL_SIGNALS.map(|number| {
            saved_signal(number, &expected_mask).expect("query retry fixture action")
        });
        for (actual, raw_original) in expected.iter().zip(&original.saved) {
            assert_eq!(actual.action.sa_sigaction, raw_original.action.sa_sigaction);
            assert_signal_mask(&actual.action.sa_mask, &raw_original.action.sa_mask);
            assert_eq!(actual.blocked, raw_original.blocked);
        }

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
            assert_saved_state(&expected, &expected_mask);
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
                let mut reader = SignalScope::with_faults(SignalFaults::default()).unwrap();
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
        let mut reader = SignalScope::with_faults(SignalFaults::default()).unwrap();
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
        let mut scope = SignalScope::with_faults(SignalFaults::default()).unwrap();
        send_to_current_thread(libc::SIGHUP);
        assert_eq!(scope.freeze().unwrap(), RecorderStop::Signal(libc::SIGHUP));
        send_to_current_thread(libc::SIGTERM);
        let frozen = scope.freeze().unwrap();
        scope.restore_parent().unwrap();
        let mut reader = SignalScope::with_faults(SignalFaults::default()).unwrap();
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
        let mut trace_length = || Ok(0);
        let error = supervise_collector(&mut scope, guard, &mut trace_length, policy, &schedule)
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
        let mut scope =
            SignalScope::with_faults(SignalFaults::default()).expect("create signal scope");
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
        let mut trace_length = || Ok(0);
        let status = supervise_collector(&mut scope, guard, &mut trace_length, policy, &schedule)
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
        let mut scope =
            SignalScope::with_faults(SignalFaults::default()).expect("create signal scope");
        let child_signals = scope.child_state();
        let policy =
            CaptureLimits::from_durations(Some(Duration::ZERO), None, Duration::from_millis(20));
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
        let mut trace_length = || Ok(0);
        let supervised =
            supervise_collector(&mut scope, guard, &mut trace_length, policy, &schedule);
        let status = supervised.status.expect("terminate limited collector");
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        assert_eq!(supervised.limits.duration_limit(), Some(Duration::ZERO));
        assert_eq!(supervised.limits.trace_bytes_observation(), None);
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
        let mut scope =
            SignalScope::with_faults(SignalFaults::default()).expect("create signal scope");
        let policy = CaptureLimits::from_millis(None, Some(0), 2_000);
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
        let mut trace_length = || bail!("injected anchored fstat failure");

        let supervised =
            supervise_collector(&mut scope, guard, &mut trace_length, policy, &schedule);
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
        let mut scope =
            SignalScope::with_faults(SignalFaults::default()).expect("create signal scope");
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
        let mut scope =
            SignalScope::with_faults(SignalFaults::default()).expect("create signal scope");
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
    fn interrupted_version_read_rechecks_its_deadline() {
        struct InterruptedReader {
            calls: usize,
        }
        impl Read for InterruptedReader {
            fn read(&mut self, _bytes: &mut [u8]) -> io::Result<usize> {
                self.calls += 1;
                std::thread::sleep(Duration::from_millis(10));
                Err(io::Error::from_raw_os_error(libc::EINTR))
            }
        }
        impl AsRawFd for InterruptedReader {
            fn as_raw_fd(&self) -> i32 {
                -1 // The deterministic leaf never performs real descriptor I/O.
            }
        }
        let mut pipe = CapturedPipe {
            reader: InterruptedReader { calls: 0 },
            bytes: Vec::new(),
            eof: false,
            name: "stdout",
        };
        let deadline = Instant::now() + Duration::from_millis(5);
        assert!(pipe.drain(deadline).is_err());
        assert!(
            pipe.reader.calls <= 1,
            "no new read after EINTR exhausts clock"
        );
        assert!(pipe.bytes.is_empty());
        pipe.reader.calls = 0;
        assert!(pipe.drain(Instant::now()).is_err());
        assert_eq!(pipe.reader.calls, 0, "expired clock must precede I/O");
    }

    #[test]
    fn ready_probe_does_not_wait_for_descendant_pipe_eof() {
        let _lock = signal_test_lock();
        let _original = OriginalSignalState::capture();
        let mut scope =
            SignalScope::with_faults(SignalFaults::default()).expect("create signal scope");
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
mod thread_precondition_tests {
    use super::*;

    #[test]
    fn task_enumeration_accepts_one_and_rejects_zero_multiple_or_late_error() {
        require_single_recorder_task([Ok(())]).unwrap();
        for tasks in [vec![], vec![Ok(()), Ok(())]] {
            let error = require_single_recorder_task(tasks).unwrap_err();
            assert!(error.to_string().contains("single-threaded recorder"));
        }
        for prefix in [vec![], vec![Ok(())], vec![Ok(()), Ok(())]] {
            let tasks = prefix
                .into_iter()
                .chain([Err(io::Error::other("late task listing failure"))]);
            let error = require_single_recorder_task(tasks).unwrap_err();
            assert!(format!("{error:#}").contains("late task listing failure"));
        }
    }
}

#[cfg(test)]
mod length_port_tests {
    use super::*;

    #[test]
    fn length_port_preserves_disabled_equal_and_first_excess_boundaries() {
        let calls = std::cell::Cell::new(0usize);
        let length = std::cell::Cell::new(8u64);
        let mut read = || {
            calls.set(calls.get() + 1);
            Ok(length.get())
        };
        let disabled = CaptureLimits::default();
        let schedule = CaptureSchedule::before_spawn(disabled).unwrap();
        let mut facts = CaptureLimitFacts::default();
        assert!(!observe_capture_limits(&mut read, disabled, &schedule, &mut facts).unwrap());
        assert_eq!(
            calls.get(),
            0,
            "disabled byte policy must not inspect length"
        );

        let enabled = CaptureLimits::from_millis(None, Some(8), 2_000);
        let schedule = CaptureSchedule::before_spawn(enabled).unwrap();
        assert!(!observe_capture_limits(&mut read, enabled, &schedule, &mut facts).unwrap());
        assert_eq!(calls.get(), 1);
        assert!(
            !facts.trace_bytes_exceeded(),
            "equality does not exceed the soft bound"
        );

        length.set(9);
        assert!(observe_capture_limits(&mut read, enabled, &schedule, &mut facts).unwrap());
        assert_eq!(calls.get(), 2);
        assert_eq!(facts.trace_bytes_observation(), Some((9, 8)));
        length.set(12);
        assert!(!observe_capture_limits(&mut read, enabled, &schedule, &mut facts).unwrap());
        assert_eq!(calls.get(), 2, "first excess stops periodic length reads");
        assert_eq!(facts.trace_bytes_observation(), Some((9, 8)));
    }

    #[test]
    fn failed_length_port_does_not_invent_a_byte_limit_fact() {
        let policy = CaptureLimits::from_millis(None, Some(0), 2_000);
        let schedule = CaptureSchedule::before_spawn(policy).unwrap();
        let calls = std::cell::Cell::new(0usize);
        let mut read = || {
            calls.set(calls.get() + 1);
            bail!("injected length port failure")
        };
        let mut facts = CaptureLimitFacts::default();
        let error = observe_capture_limits(&mut read, policy, &schedule, &mut facts)
            .expect_err("read failure cannot establish an observed excess");
        assert!(format!("{error:#}").contains("injected length port failure"));
        assert_eq!(calls.get(), 1);
        assert_eq!(facts, CaptureLimitFacts::default());
    }
}
