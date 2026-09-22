//! Pure normalization of raw strace lines into evidence-bearing events.
//!
//! Parsing has no filesystem or process side effects: callers supply raw text,
//! its evidence-file label, and an optional last-observed CWD. Line syntax is
//! decoded incrementally, while task generations and filesystem contexts are
//! resolved once at [`TraceParser::finish`]. Those transient relationships are
//! available to capture completion but never enter the persisted event schema.

use crate::model::{Event, Evidence, bytes};
use std::collections::BTreeMap;
#[cfg(test)]
mod fs_tests;

/// Normalized observations plus transient capture-time interpretation.
///
/// Events retain their original evidence file and start/end lines. Warnings are
/// ordered by parser observation and finish-time resolution; callers must not
/// sort them or treat the transient task timeline as persisted evidence.
#[derive(Default)]
pub(crate) struct Parsed {
    /// Normalized event-v2 values in parser insertion order.
    pub(crate) events: Vec<Event>,
    /// Evidence limitations in deterministic parser order.
    pub(crate) warnings: Vec<String>,
    /// Number of logical raw trace lines consumed.
    pub(crate) lines: usize,
    /// Evidence-ordered task generations used by capture completion.
    pub(crate) timeline: TaskTimeline,
}

/// Opaque identity for one lifetime of a numeric task ID within a trace.
///
/// The value is meaningful only inside its owning [`TaskTimeline`] and is never
/// serialized into event or metadata schemas.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct TaskGenerationId(usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TaskGenerationOrigin {
    Initial {
        event: usize,
    },
    Spawned {
        parent: TaskGenerationId,
        event: usize,
    },
    Unintroduced {
        event: usize,
    },
    Reappeared {
        previous: TaskGenerationId,
        event: usize,
    },
    LiveCollision {
        previous: TaskGenerationId,
        event: usize,
    },
}

struct TaskGeneration {
    pid: String,
    ordinal: usize,
    origin: TaskGenerationOrigin,
    first_actor_event: Option<usize>,
    exit_event: Option<usize>,
}

/// Transient task identity interpretation in raw evidence order.
///
/// Generation IDs never enter Event v2. Keeping them transient prevents a
/// newer parser from retroactively changing historical record identity. Capture
/// may query completion facts but cannot mutate or reconstruct this state.
#[derive(Default)]
pub(crate) struct TaskTimeline {
    generations: Vec<TaskGeneration>,
    event_order: Vec<usize>,
    event_generations: Vec<TaskGenerationId>,
    spawned_generations: Vec<Option<TaskGenerationId>>,
}

impl TaskTimeline {
    fn push_generation(
        generations: &mut Vec<TaskGeneration>,
        ordinals: &mut BTreeMap<String, usize>,
        pid: &str,
        origin: TaskGenerationOrigin,
    ) -> TaskGenerationId {
        let ordinal = ordinals.entry(pid.to_owned()).or_default();
        *ordinal += 1;
        let id = TaskGenerationId(generations.len());
        generations.push(TaskGeneration {
            pid: pid.to_owned(),
            ordinal: *ordinal,
            origin,
            first_actor_event: None,
            exit_event: None,
        });
        id
    }

    fn from_events(events: &[Event]) -> Self {
        let mut order: Vec<usize> = (0..events.len()).collect();
        order.sort_by_key(|&index| {
            let evidence = &events[index].evidence;
            (evidence.line, evidence.end_line, index)
        });
        let mut generations = Vec::new();
        let mut event_generations = vec![None; events.len()];
        let mut spawned_generations = vec![None; events.len()];
        let mut active = BTreeMap::<String, TaskGenerationId>::new();
        let mut latest = BTreeMap::<String, TaskGenerationId>::new();
        let mut ordinals = BTreeMap::<String, usize>::new();
        let mut has_initial = false;

        for &index in &order {
            let event = &events[index];
            let actor = active.get(&event.pid).copied().unwrap_or_else(|| {
                let origin = if !has_initial {
                    has_initial = true;
                    TaskGenerationOrigin::Initial { event: index }
                } else if let Some(previous) = latest.get(&event.pid).copied() {
                    TaskGenerationOrigin::Reappeared {
                        previous,
                        event: index,
                    }
                } else {
                    TaskGenerationOrigin::Unintroduced { event: index }
                };
                let generation =
                    Self::push_generation(&mut generations, &mut ordinals, &event.pid, origin);
                active.insert(event.pid.clone(), generation);
                latest.insert(event.pid.clone(), generation);
                generation
            });
            event_generations[index] = Some(actor);
            if generations[actor.0].first_actor_event.is_none() {
                generations[actor.0].first_actor_event = Some(index);
            }

            if event.kind == "exit" {
                generations[actor.0].exit_event = Some(index);
                active.remove(&event.pid);
            }

            if let Some(child) = event.child_pid.as_deref() {
                let origin = active.get(child).copied().map_or(
                    TaskGenerationOrigin::Spawned {
                        parent: actor,
                        event: index,
                    },
                    |previous| TaskGenerationOrigin::LiveCollision {
                        previous,
                        event: index,
                    },
                );
                let generation =
                    Self::push_generation(&mut generations, &mut ordinals, child, origin);
                active.insert(child.to_owned(), generation);
                latest.insert(child.to_owned(), generation);
                spawned_generations[index] = Some(generation);
            }
        }

        Self {
            generations,
            event_order: order,
            event_generations: event_generations
                .into_iter()
                .map(|generation| generation.expect("every parsed event enters the timeline"))
                .collect(),
            spawned_generations,
        }
    }

    /// Return the task generation that produced one normalized event index.
    pub(crate) fn event_generation(&self, event: usize) -> TaskGenerationId {
        self.event_generations[event]
    }

    /// Iterate normalized event indices by `(line, end_line, insertion_index)`.
    pub(crate) fn events_in_evidence_order(&self) -> impl Iterator<Item = usize> + '_ {
        self.event_order.iter().copied()
    }

    /// Iterate every observed generation in deterministic creation order.
    pub(crate) fn generations(&self) -> impl Iterator<Item = TaskGenerationId> + '_ {
        (0..self.generations.len()).map(TaskGenerationId)
    }

    fn spawned_generation(&self, event: usize) -> Option<TaskGenerationId> {
        self.spawned_generations.get(event).copied().flatten()
    }

    fn origin(&self, generation: TaskGenerationId) -> TaskGenerationOrigin {
        self.generations[generation.0].origin
    }

    /// Format a generation as the diagnostic-only `PID#ordinal` label.
    pub(crate) fn label(&self, generation: TaskGenerationId) -> String {
        let generation = &self.generations[generation.0];
        format!("{}#{}", generation.pid, generation.ordinal)
    }

    /// Return the raw numeric PID spelling for one generation.
    pub(crate) fn pid(&self, generation: TaskGenerationId) -> &str {
        &self.generations[generation.0].pid
    }

    /// Return the normalized exit-event index belonging to this generation.
    pub(crate) fn exit_event(&self, generation: TaskGenerationId) -> Option<usize> {
        self.generations[generation.0].exit_event
    }

    fn first_actor_event(&self, generation: TaskGenerationId) -> Option<usize> {
        self.generations[generation.0].first_actor_event
    }

    fn is_untrusted(&self, generation: TaskGenerationId) -> bool {
        matches!(
            self.origin(generation),
            TaskGenerationOrigin::Unintroduced { .. }
                | TaskGenerationOrigin::Reappeared { .. }
                | TaskGenerationOrigin::LiveCollision { .. }
        )
    }

    fn apply_identity_findings(&self, parsed: &mut Parsed) {
        for (index, actor) in self.event_generations.iter().copied().enumerate() {
            if self.is_untrusted(actor) {
                parsed.events[index].context_uncertain = true;
            }
        }

        for id in self.generations() {
            let generation = &self.generations[id.0];
            let (event, detail) = match generation.origin {
                TaskGenerationOrigin::Initial { .. } | TaskGenerationOrigin::Spawned { .. } => {
                    continue;
                }
                TaskGenerationOrigin::Unintroduced { event } => (
                    event,
                    format!(
                        "Unintroduced: task {} appeared without a successful spawn",
                        self.label(id)
                    ),
                ),
                TaskGenerationOrigin::Reappeared { previous, event } => {
                    let exit = self
                        .exit_event(previous)
                        .and_then(|index| parsed.events.get(index))
                        .map(|event| format!(" at {}:{}", event.evidence.file, event.evidence.line))
                        .unwrap_or_default();
                    (
                        event,
                        format!(
                            "Reappeared: task {} appeared after {} exited{exit} without a successful spawn",
                            self.label(id),
                            self.label(previous)
                        ),
                    )
                }
                TaskGenerationOrigin::LiveCollision { previous, event } => (
                    event,
                    format!(
                        "LiveCollision: successful spawn assigned {} while {} was still live",
                        self.label(id),
                        self.label(previous)
                    ),
                ),
            };

            parsed.events[event].context_uncertain = true;
            let evidence = &parsed.events[event].evidence;
            parsed.warnings.push(format!(
                "{}:{}: task.identity: {detail}; generation identity is incomplete",
                evidence.file, evidence.line
            ));
        }
    }
}
// Split syscall arguments without splitting quoted strings or nested structures.
fn args(s: &str) -> Vec<&str> {
    let (mut quote, mut escape, mut depth, mut start) = (false, false, 0i32, 0);
    let mut out = vec![];
    for (i, c) in s.char_indices() {
        if escape {
            escape = false;
            continue;
        }
        if c == '\\' && quote {
            escape = true;
            continue;
        }
        if c == '"' {
            quote = !quote;
        }
        if !quote {
            match c {
                '(' | '[' | '{' | '<' => depth += 1,
                ')' | ']' | '}' | '>' => depth -= 1,
                ',' if depth == 0 => {
                    out.push(s[start..i].trim());
                    start = i + 1;
                }
                _ => {}
            }
        }
    }
    out.push(s[start..].trim());
    out
}
// Locate the syscall's closing parenthesis, not punctuation in strings or nested args.
// strace pads the return column, especially on resumed calls.
fn syscall_result(rest: &str) -> Option<(&str, &str)> {
    let (mut depth, mut quoted, mut escaped) = (1usize, false, false);
    for (i, c) in rest.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if quoted && c == '\\' {
            escaped = true;
            continue;
        }
        if c == '"' {
            quoted = !quoted;
            continue;
        }
        if quoted {
            continue;
        }
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    let ret = rest[i + 1..].trim_start().strip_prefix('=')?.trim_start();
                    return Some((&rest[..i], ret));
                }
            }
            _ => {}
        }
    }
    None
}
fn unquote(s: &str) -> Option<Vec<u8>> {
    let s = s.strip_prefix('"')?.strip_suffix('"')?;
    if !s.is_ascii() {
        return None;
    }
    let mut out = vec![];
    let mut i = 0;
    let b = s.as_bytes();
    while i < b.len() {
        if b[i] == b'\\' {
            if i + 3 >= b.len() || b[i + 1] != b'x' {
                return None;
            }
            out.push(u8::from_str_radix(&s[i + 2..i + 4], 16).ok()?);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    Some(out)
}

// Decode a complete -xx annotation payload after its angle delimiters have
// been checked. Callers retain responsibility for pathname-role constraints,
// including whether the decoded bytes must spell an absolute path.
fn decode_xx_annotation(encoded: &str) -> Option<Vec<u8>> {
    if !encoded.len().is_multiple_of(4)
        || !encoded
            .as_bytes()
            .chunks(4)
            .all(|chunk| chunk.starts_with(b"\\x"))
    {
        return None;
    }
    unquote(&format!("\"{encoded}\""))
}

// Decode only the complete -xx pathname annotation. No filesystem lookup.
fn annotated_path(value: &str) -> Option<String> {
    let encoded = value.split_once('<')?.1.strip_suffix('>')?;
    if encoded.is_empty() {
        return None;
    }
    decode_xx_annotation(encoded)
        .filter(|p| p.starts_with(b"/"))
        .map(|p| bytes(&p))
}
fn path(a: &[&str], at: bool) -> Option<String> {
    let p = unquote(a.get(usize::from(at))?)?;
    if p.starts_with(b"/") {
        return Some(bytes(&p));
    }
    if at {
        let dir = a.first()?;
        if let Some((_, annotation)) = dir.split_once('<') {
            // With -xx, verified strace annotations encode every pathname byte.
            // Decode only that exact form; suffixes such as " (deleted)" stay unknown.
            let encoded = annotation.strip_suffix('>');
            let Some(encoded) = encoded else {
                return Some(format!(
                    "relative:{} [dirfd annotation incomplete]",
                    bytes(&p)
                ));
            };
            if let Some(mut base) = decode_xx_annotation(encoded)
                && base.starts_with(b"/")
                && !p.is_empty()
            {
                if !base.ends_with(b"/") {
                    base.push(b'/');
                }
                base.extend_from_slice(&p);
                return Some(bytes(&base));
            }
            return Some(format!("relative:{} [dirfd:{}]", bytes(&p), encoded));
        }
    }
    if at && a.first().is_some_and(|dir| *dir != "AT_FDCWD") {
        return Some(format!("relative:{} [dirfd unresolved]", bytes(&p)));
    }
    Some(format!("relative:{} [cwd unresolved]", bytes(&p)))
}

/// These actions are parser-private evidence. They deliberately are not
/// serialized: v1/v2/v3 records retain their recorded conclusions instead of
/// acquiring a new trust claim when read by a newer binary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FsAction {
    None,
    Spawn(SpawnEffect),
    Unshare(UnshareEffect),
    Invalidate(InvalidationScope),
    Exit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InvalidationScope {
    ConnectedFs,
    AllActiveContexts,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TraceOperation {
    Exec { at: bool, argv_index: usize },
    File { at: bool, returned_path: bool },
    Cwd { fd: bool },
    Spawn,
    Unshare,
    Invalidate(InvalidationScope),
}

// This is the single capture/parser syscall contract. Capture derives its
// selector from these names; parsing derives event and transition semantics
// from the paired operation, so adding a name cannot silently update one side.
const TRACE_CONTRACTS: &[(&str, TraceOperation)] = &[
    (
        "execve",
        TraceOperation::Exec {
            at: false,
            argv_index: 1,
        },
    ),
    (
        "execveat",
        TraceOperation::Exec {
            at: true,
            argv_index: 2,
        },
    ),
    (
        "open",
        TraceOperation::File {
            at: false,
            returned_path: true,
        },
    ),
    (
        "openat",
        TraceOperation::File {
            at: true,
            returned_path: true,
        },
    ),
    (
        "openat2",
        TraceOperation::File {
            at: true,
            returned_path: true,
        },
    ),
    (
        "access",
        TraceOperation::File {
            at: false,
            returned_path: false,
        },
    ),
    (
        "faccessat",
        TraceOperation::File {
            at: true,
            returned_path: false,
        },
    ),
    (
        "faccessat2",
        TraceOperation::File {
            at: true,
            returned_path: false,
        },
    ),
    ("chdir", TraceOperation::Cwd { fd: false }),
    ("fchdir", TraceOperation::Cwd { fd: true }),
    ("clone", TraceOperation::Spawn),
    ("clone3", TraceOperation::Spawn),
    ("fork", TraceOperation::Spawn),
    ("vfork", TraceOperation::Spawn),
    ("unshare", TraceOperation::Unshare),
    (
        "setns",
        TraceOperation::Invalidate(InvalidationScope::ConnectedFs),
    ),
    (
        "chroot",
        TraceOperation::Invalidate(InvalidationScope::ConnectedFs),
    ),
    (
        "pivot_root",
        TraceOperation::Invalidate(InvalidationScope::AllActiveContexts),
    ),
    (
        "mount",
        TraceOperation::Invalidate(InvalidationScope::AllActiveContexts),
    ),
    (
        "umount2",
        TraceOperation::Invalidate(InvalidationScope::AllActiveContexts),
    ),
    (
        "move_mount",
        TraceOperation::Invalidate(InvalidationScope::AllActiveContexts),
    ),
    (
        "mount_setattr",
        TraceOperation::Invalidate(InvalidationScope::AllActiveContexts),
    ),
];

fn trace_operation(name: &str) -> Option<TraceOperation> {
    TRACE_CONTRACTS
        .iter()
        .find_map(|(candidate, operation)| (*candidate == name).then_some(*operation))
}

/// Build capture's strace selector from the parser's single syscall contract.
///
/// Capture must use this value rather than maintaining a second syscall list;
/// otherwise an uncollected state transition could be reported as complete.
pub(crate) fn trace_selector() -> String {
    format!(
        "trace={}",
        TRACE_CONTRACTS
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
            .join(",")
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpawnEffect {
    SharedFs,
    PrivateFs,
    // Keep namespace evidence distinct so the transition layer can constrain
    // any uncertainty to the affected branch.
    PrivateFsNamespaceUncertain,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnshareEffect {
    NoFsChange,
    SplitFs,
    SplitFsNamespaceUncertain,
    Unknown,
}

// Extract one complete field from a strace structure. Missing or truncated
// fields stay unknown rather than being supplied with ABI defaults.
fn field_expression<'a>(args: &'a [&'a str], marker: &str) -> Option<&'a str> {
    args.iter().find_map(|arg| {
        let value = arg.split_once(marker)?.1;
        let end = value
            .find(',')
            .or_else(|| value.find('}'))
            .unwrap_or(value.len());
        let value = value[..end].trim();
        (!value.is_empty()).then_some(value)
    })
}

const CSIGNAL_MASK: u64 = 0x0000_00ff;
const CLONE_NEWTIME_BIT: u64 = 0x0000_0080;
const CLONE_VM_BIT: u64 = 0x0000_0100;
const CLONE_FS_BIT: u64 = 0x0000_0200;
const CLONE_FILES_BIT: u64 = 0x0000_0400;
const CLONE_SIGHAND_BIT: u64 = 0x0000_0800;
const CLONE_PIDFD_BIT: u64 = 0x0000_1000;
const CLONE_PTRACE_BIT: u64 = 0x0000_2000;
const CLONE_VFORK_BIT: u64 = 0x0000_4000;
const CLONE_PARENT_BIT: u64 = 0x0000_8000;
const CLONE_THREAD_BIT: u64 = 0x0001_0000;
const CLONE_NEWNS_BIT: u64 = 0x0002_0000;
const CLONE_SYSVSEM_BIT: u64 = 0x0004_0000;
const CLONE_SETTLS_BIT: u64 = 0x0008_0000;
const CLONE_PARENT_SETTID_BIT: u64 = 0x0010_0000;
const UNSHARE_EMPTY_MNTNS_BIT: u64 = 0x0010_0000;
const CLONE_CHILD_CLEARTID_BIT: u64 = 0x0020_0000;
const CLONE_DETACHED_BIT: u64 = 0x0040_0000;
const CLONE_UNTRACED_BIT: u64 = 0x0080_0000;
const CLONE_CHILD_SETTID_BIT: u64 = 0x0100_0000;
const CLONE_NEWCGROUP_BIT: u64 = 0x0200_0000;
const CLONE_NEWUTS_BIT: u64 = 0x0400_0000;
const CLONE_NEWIPC_BIT: u64 = 0x0800_0000;
const CLONE_NEWUSER_BIT: u64 = 0x1000_0000;
const CLONE_NEWPID_BIT: u64 = 0x2000_0000;
const CLONE_NEWNET_BIT: u64 = 0x4000_0000;
const CLONE_IO_BIT: u64 = 0x8000_0000;
const CLONE_CLEAR_SIGHAND_BIT: u64 = 1 << 32;
const CLONE_INTO_CGROUP_BIT: u64 = 1 << 33;
const CLONE_AUTOREAP_BIT: u64 = 1 << 34;
const CLONE_NNP_BIT: u64 = 1 << 35;
const CLONE_PIDFD_AUTOKILL_BIT: u64 = 1 << 36;
const CLONE_EMPTY_MNTNS_BIT: u64 = 1 << 37;

const CLONE_NAMESPACE_FLAGS: u64 = CLONE_NEWTIME_BIT
    | CLONE_NEWNS_BIT
    | CLONE_NEWCGROUP_BIT
    | CLONE_NEWUTS_BIT
    | CLONE_NEWIPC_BIT
    | CLONE_NEWUSER_BIT
    | CLONE_NEWPID_BIT
    | CLONE_NEWNET_BIT
    | CLONE_EMPTY_MNTNS_BIT;

const UNSHARE_NAMESPACE_FLAGS: u64 = CLONE_NEWTIME_BIT
    | CLONE_NEWNS_BIT
    | UNSHARE_EMPTY_MNTNS_BIT
    | CLONE_NEWCGROUP_BIT
    | CLONE_NEWUTS_BIT
    | CLONE_NEWIPC_BIT
    | CLONE_NEWUSER_BIT
    | CLONE_NEWPID_BIT
    | CLONE_NEWNET_BIT;

const LEGACY_CLONE_ALLOWED: u64 = CSIGNAL_MASK
    | CLONE_VM_BIT
    | CLONE_FS_BIT
    | CLONE_FILES_BIT
    | CLONE_SIGHAND_BIT
    | CLONE_PIDFD_BIT
    | CLONE_PTRACE_BIT
    | CLONE_VFORK_BIT
    | CLONE_PARENT_BIT
    | CLONE_THREAD_BIT
    | CLONE_NEWNS_BIT
    | CLONE_SYSVSEM_BIT
    | CLONE_SETTLS_BIT
    | CLONE_PARENT_SETTID_BIT
    | CLONE_CHILD_CLEARTID_BIT
    | CLONE_DETACHED_BIT
    | CLONE_UNTRACED_BIT
    | CLONE_CHILD_SETTID_BIT
    | CLONE_NEWCGROUP_BIT
    | CLONE_NEWUTS_BIT
    | CLONE_NEWIPC_BIT
    | CLONE_NEWUSER_BIT
    | CLONE_NEWPID_BIT
    | CLONE_NEWNET_BIT
    | CLONE_IO_BIT;

const CLONE3_ALLOWED: u64 = (LEGACY_CLONE_ALLOWED & !(CSIGNAL_MASK | CLONE_DETACHED_BIT))
    | CLONE_NEWTIME_BIT
    | CLONE_CLEAR_SIGHAND_BIT
    | CLONE_INTO_CGROUP_BIT
    | CLONE_AUTOREAP_BIT
    | CLONE_NNP_BIT
    | CLONE_PIDFD_AUTOKILL_BIT
    | CLONE_EMPTY_MNTNS_BIT;

const UNSHARE_ALLOWED: u64 = CLONE_NEWTIME_BIT
    | CLONE_VM_BIT
    | CLONE_FS_BIT
    | CLONE_FILES_BIT
    | CLONE_SIGHAND_BIT
    | CLONE_THREAD_BIT
    | CLONE_NEWNS_BIT
    | CLONE_SYSVSEM_BIT
    | UNSHARE_EMPTY_MNTNS_BIT
    | CLONE_NEWCGROUP_BIT
    | CLONE_NEWUTS_BIT
    | CLONE_NEWIPC_BIT
    | CLONE_NEWUSER_BIT
    | CLONE_NEWPID_BIT
    | CLONE_NEWNET_BIT;

fn parse_number(value: &str) -> Option<u64> {
    value.strip_prefix("0x").map_or_else(
        || value.parse().ok(),
        |hex| u64::from_str_radix(hex, 16).ok(),
    )
}

fn standard_signal_number(term: &str) -> Option<u64> {
    let number = match term {
        "SIGHUP" => 1,
        "SIGINT" => 2,
        "SIGQUIT" => 3,
        "SIGILL" => 4,
        "SIGTRAP" => 5,
        "SIGABRT" => 6,
        "SIGBUS" => 7,
        "SIGFPE" => 8,
        "SIGKILL" => 9,
        "SIGUSR1" => 10,
        "SIGSEGV" => 11,
        "SIGUSR2" => 12,
        "SIGPIPE" => 13,
        "SIGALRM" => 14,
        "SIGTERM" => 15,
        "SIGSTKFLT" => 16,
        "SIGCHLD" => 17,
        "SIGCONT" => 18,
        "SIGSTOP" => 19,
        "SIGTSTP" => 20,
        "SIGTTIN" => 21,
        "SIGTTOU" => 22,
        "SIGURG" => 23,
        "SIGXCPU" => 24,
        "SIGXFSZ" => 25,
        "SIGVTALRM" => 26,
        "SIGPROF" => 27,
        "SIGWINCH" => 28,
        "SIGIO" => 29,
        "SIGPWR" => 30,
        "SIGSYS" => 31,
        "SIGRTMIN" => 32,
        "SIGRTMAX" => 64,
        _ => {
            let offset = term
                .strip_prefix("SIGRT_")
                .or_else(|| term.strip_prefix("SIGRTMIN+"))?
                .parse::<u64>()
                .ok()?;
            return (offset <= 32).then_some(32 + offset);
        }
    };
    Some(number)
}

fn legacy_clone_flag_bit(term: &str) -> Option<u64> {
    Some(match term {
        "0" => 0,
        "CLONE_VM" => CLONE_VM_BIT,
        "CLONE_FS" => CLONE_FS_BIT,
        "CLONE_FILES" => CLONE_FILES_BIT,
        "CLONE_SIGHAND" => CLONE_SIGHAND_BIT,
        "CLONE_PIDFD" => CLONE_PIDFD_BIT,
        "CLONE_PTRACE" => CLONE_PTRACE_BIT,
        "CLONE_VFORK" => CLONE_VFORK_BIT,
        "CLONE_PARENT" => CLONE_PARENT_BIT,
        "CLONE_THREAD" => CLONE_THREAD_BIT,
        "CLONE_NEWNS" => CLONE_NEWNS_BIT,
        "CLONE_SYSVSEM" => CLONE_SYSVSEM_BIT,
        "CLONE_SETTLS" => CLONE_SETTLS_BIT,
        "CLONE_PARENT_SETTID" => CLONE_PARENT_SETTID_BIT,
        "CLONE_CHILD_CLEARTID" => CLONE_CHILD_CLEARTID_BIT,
        "CLONE_DETACHED" => CLONE_DETACHED_BIT,
        "CLONE_UNTRACED" => CLONE_UNTRACED_BIT,
        "CLONE_CHILD_SETTID" => CLONE_CHILD_SETTID_BIT,
        "CLONE_NEWCGROUP" => CLONE_NEWCGROUP_BIT,
        "CLONE_NEWUTS" => CLONE_NEWUTS_BIT,
        "CLONE_NEWIPC" => CLONE_NEWIPC_BIT,
        "CLONE_NEWUSER" => CLONE_NEWUSER_BIT,
        "CLONE_NEWPID" => CLONE_NEWPID_BIT,
        "CLONE_NEWNET" => CLONE_NEWNET_BIT,
        "CLONE_IO" => CLONE_IO_BIT,
        _ => return None,
    })
}

fn clone3_flag_bit(term: &str) -> Option<u64> {
    Some(match term {
        "0" => 0,
        "CLONE_NEWTIME" => CLONE_NEWTIME_BIT,
        "CLONE_VM" => CLONE_VM_BIT,
        "CLONE_FS" => CLONE_FS_BIT,
        "CLONE_FILES" => CLONE_FILES_BIT,
        "CLONE_SIGHAND" => CLONE_SIGHAND_BIT,
        "CLONE_PIDFD" => CLONE_PIDFD_BIT,
        "CLONE_PTRACE" => CLONE_PTRACE_BIT,
        "CLONE_VFORK" => CLONE_VFORK_BIT,
        "CLONE_PARENT" => CLONE_PARENT_BIT,
        "CLONE_THREAD" => CLONE_THREAD_BIT,
        "CLONE_NEWNS" => CLONE_NEWNS_BIT,
        "CLONE_SYSVSEM" => CLONE_SYSVSEM_BIT,
        "CLONE_SETTLS" => CLONE_SETTLS_BIT,
        "CLONE_PARENT_SETTID" => CLONE_PARENT_SETTID_BIT,
        "CLONE_CHILD_CLEARTID" => CLONE_CHILD_CLEARTID_BIT,
        "CLONE_UNTRACED" => CLONE_UNTRACED_BIT,
        "CLONE_CHILD_SETTID" => CLONE_CHILD_SETTID_BIT,
        "CLONE_NEWCGROUP" => CLONE_NEWCGROUP_BIT,
        "CLONE_NEWUTS" => CLONE_NEWUTS_BIT,
        "CLONE_NEWIPC" => CLONE_NEWIPC_BIT,
        "CLONE_NEWUSER" => CLONE_NEWUSER_BIT,
        "CLONE_NEWPID" => CLONE_NEWPID_BIT,
        "CLONE_NEWNET" => CLONE_NEWNET_BIT,
        "CLONE_IO" => CLONE_IO_BIT,
        "CLONE_CLEAR_SIGHAND" => CLONE_CLEAR_SIGHAND_BIT,
        "CLONE_INTO_CGROUP" => CLONE_INTO_CGROUP_BIT,
        "CLONE_AUTOREAP" => CLONE_AUTOREAP_BIT,
        "CLONE_NNP" => CLONE_NNP_BIT,
        "CLONE_PIDFD_AUTOKILL" => CLONE_PIDFD_AUTOKILL_BIT,
        "CLONE_EMPTY_MNTNS" => CLONE_EMPTY_MNTNS_BIT,
        _ => return None,
    })
}

fn unshare_flag_bit(term: &str) -> Option<u64> {
    Some(match term {
        "0" => 0,
        "CLONE_NEWTIME" => CLONE_NEWTIME_BIT,
        "CLONE_VM" => CLONE_VM_BIT,
        "CLONE_FS" => CLONE_FS_BIT,
        "CLONE_FILES" => CLONE_FILES_BIT,
        "CLONE_SIGHAND" => CLONE_SIGHAND_BIT,
        "CLONE_THREAD" => CLONE_THREAD_BIT,
        "CLONE_NEWNS" => CLONE_NEWNS_BIT,
        "CLONE_SYSVSEM" => CLONE_SYSVSEM_BIT,
        "UNSHARE_EMPTY_MNTNS" => UNSHARE_EMPTY_MNTNS_BIT,
        "CLONE_NEWCGROUP" => CLONE_NEWCGROUP_BIT,
        "CLONE_NEWUTS" => CLONE_NEWUTS_BIT,
        "CLONE_NEWIPC" => CLONE_NEWIPC_BIT,
        "CLONE_NEWUSER" => CLONE_NEWUSER_BIT,
        "CLONE_NEWPID" => CLONE_NEWPID_BIT,
        "CLONE_NEWNET" => CLONE_NEWNET_BIT,
        _ => return None,
    })
}

fn symbolic_bits(expression: &str, flag_bit: fn(&str) -> Option<u64>) -> Option<u64> {
    let mut bits = 0;
    let mut count = 0;
    for term in expression.split('|').map(str::trim) {
        if term.is_empty() {
            return None;
        }
        bits |= flag_bit(term)?;
        count += 1;
    }
    (count > 0).then_some(bits)
}

fn valid_clone_combination(bits: u64, clone3: bool, exit_signal: u64) -> bool {
    let has = |mask| bits & mask != 0;
    if has(CLONE_SIGHAND_BIT) && !has(CLONE_VM_BIT)
        || has(CLONE_THREAD_BIT) && !has(CLONE_SIGHAND_BIT)
        || has(CLONE_FS_BIT) && has(CLONE_NAMESPACE_FLAGS)
        || has(CLONE_NEWUSER_BIT) && has(CLONE_FS_BIT | CLONE_THREAD_BIT | CLONE_PARENT_BIT)
        || has(CLONE_NEWIPC_BIT) && has(CLONE_SYSVSEM_BIT)
        || has(CLONE_NEWPID_BIT) && has(CLONE_THREAD_BIT | CLONE_PARENT_BIT)
    {
        return false;
    }
    if exit_signal != 0 && has(CLONE_THREAD_BIT | CLONE_PARENT_BIT) {
        return false;
    }
    if clone3 {
        !(has(CLONE_CLEAR_SIGHAND_BIT) && has(CLONE_SIGHAND_BIT))
            && !(has(CLONE_AUTOREAP_BIT)
                && (has(CLONE_THREAD_BIT | CLONE_PARENT_BIT) || exit_signal != 0))
            && !(has(CLONE_NNP_BIT) && has(CLONE_THREAD_BIT))
            && !(has(CLONE_PIDFD_AUTOKILL_BIT)
                && (!has(CLONE_PIDFD_BIT) || !has(CLONE_AUTOREAP_BIT)))
    } else {
        !(has(CLONE_PIDFD_BIT) && has(CLONE_DETACHED_BIT | CLONE_PARENT_SETTID_BIT))
    }
}

fn spawn_effect(bits: u64) -> SpawnEffect {
    if bits & CLONE_NAMESPACE_FLAGS != 0 {
        SpawnEffect::PrivateFsNamespaceUncertain
    } else if bits & CLONE_FS_BIT != 0 {
        SpawnEffect::SharedFs
    } else {
        SpawnEffect::PrivateFs
    }
}

fn decode_legacy_clone_flags(expression: &str) -> SpawnEffect {
    let bits = if let Some(bits) = parse_number(expression) {
        bits
    } else {
        let mut bits = 0;
        let mut signal_seen = false;
        for term in expression.split('|').map(str::trim) {
            if term.is_empty() {
                return SpawnEffect::Unknown;
            }
            if let Some(bit) = legacy_clone_flag_bit(term) {
                bits |= bit;
            } else if let Some(signal) = standard_signal_number(term) {
                if signal_seen {
                    return SpawnEffect::Unknown;
                }
                signal_seen = true;
                bits |= signal;
            } else {
                return SpawnEffect::Unknown;
            }
        }
        bits
    };
    let exit_signal = bits & CSIGNAL_MASK;
    if bits & !LEGACY_CLONE_ALLOWED != 0
        || exit_signal > 64
        || !valid_clone_combination(bits, false, exit_signal)
    {
        SpawnEffect::Unknown
    } else {
        spawn_effect(bits)
    }
}

fn decode_clone3_flags(flags_expression: &str, exit_signal_expression: &str) -> SpawnEffect {
    let Some(bits) =
        parse_number(flags_expression).or_else(|| symbolic_bits(flags_expression, clone3_flag_bit))
    else {
        return SpawnEffect::Unknown;
    };
    let Some(exit_signal) = parse_number(exit_signal_expression)
        .or_else(|| standard_signal_number(exit_signal_expression))
    else {
        return SpawnEffect::Unknown;
    };
    if bits & !CLONE3_ALLOWED != 0
        || exit_signal > 64
        || !valid_clone_combination(bits, true, exit_signal)
    {
        SpawnEffect::Unknown
    } else {
        spawn_effect(bits)
    }
}

fn decode_unshare_flags(expression: &str) -> UnshareEffect {
    let Some(bits) =
        parse_number(expression).or_else(|| symbolic_bits(expression, unshare_flag_bit))
    else {
        return UnshareEffect::Unknown;
    };
    if bits & !UNSHARE_ALLOWED != 0 {
        return UnshareEffect::Unknown;
    }
    if bits & UNSHARE_NAMESPACE_FLAGS != 0 {
        UnshareEffect::SplitFsNamespaceUncertain
    } else if bits & CLONE_FS_BIT != 0 {
        UnshareEffect::SplitFs
    } else {
        UnshareEffect::NoFsChange
    }
}

fn spawn_action(operation: &str, args: &[&str]) -> FsAction {
    match operation {
        "fork" | "vfork" => FsAction::Spawn(SpawnEffect::PrivateFs),
        "clone" => FsAction::Spawn(
            field_expression(args, "flags=")
                .map_or(SpawnEffect::Unknown, decode_legacy_clone_flags),
        ),
        "clone3" => FsAction::Spawn(
            field_expression(args, "flags=")
                .zip(field_expression(args, "exit_signal="))
                .map_or(SpawnEffect::Unknown, |(flags, signal)| {
                    decode_clone3_flags(flags, signal)
                }),
        ),
        _ => FsAction::None,
    }
}

fn unshare_action(args: &[&str]) -> FsAction {
    // Unlike clone/clone3, strace prints unshare's bitmask as its first
    // positional argument rather than a named structure field.
    FsAction::Unshare(
        args.first()
            .map_or(UnshareEffect::Unknown, |arg| decode_unshare_flags(arg)),
    )
}

#[derive(Clone)]
struct PendingCwd {
    until: usize,
    target: Option<String>,
    event: usize,
}

#[derive(Clone, Copy)]
struct PendingTransition {
    until: usize,
    event: usize,
    peer: usize,
}

#[derive(Clone, Copy)]
struct PendingForkSnapshot {
    until: usize,
    spawn_event: usize,
    child_fs: usize,
}

struct FsContext {
    cwd: Option<String>,
    uncertain: bool,
    // A context only needs liveness, not a second copy of every task ID. Task
    // identity stays in FsState::tasks, so this remains O(tasks + contexts).
    members: usize,
    pending_cwd: Option<PendingCwd>,
    pending_transition: Option<PendingTransition>,
    // These live only on the source context. A child that runs before the
    // parent's resumed return must not inherit its own unresolved window.
    pending_fork_snapshots: Vec<PendingForkSnapshot>,
}

#[derive(Clone, Copy)]
struct Task {
    fs: usize,
    exited: bool,
}

struct FsState {
    tasks: BTreeMap<TaskGenerationId, Task>,
    contexts: BTreeMap<usize, FsContext>,
    next_context: usize,
}

impl FsState {
    fn new_context(
        &mut self,
        cwd: Option<String>,
        uncertain: bool,
        member: TaskGenerationId,
    ) -> usize {
        let id = self.next_context;
        self.next_context += 1;
        self.contexts.insert(
            id,
            FsContext {
                cwd,
                uncertain,
                members: 1,
                pending_cwd: None,
                pending_transition: None,
                pending_fork_snapshots: Vec::new(),
            },
        );
        self.tasks.insert(
            member,
            Task {
                fs: id,
                exited: false,
            },
        );
        id
    }

    fn remove_member(&mut self, fs: usize) {
        if let Some(context) = self.contexts.get_mut(&fs) {
            if context.members > 0 {
                context.members -= 1;
            }
            if context.members == 0 {
                self.contexts.remove(&fs);
            }
        }
    }

    fn ensure_task(
        &mut self,
        generation: TaskGenerationId,
        origin: TaskGenerationOrigin,
        initial_cwd: Option<&str>,
    ) -> usize {
        if let Some(task) = self.tasks.get(&generation).copied()
            && !task.exited
            && self.contexts.contains_key(&task.fs)
        {
            return task.fs;
        }
        let (cwd, uncertain) = if matches!(origin, TaskGenerationOrigin::Initial { .. }) {
            (initial_cwd.map(str::to_owned), false)
        } else {
            (None, true)
        };
        self.new_context(cwd, uncertain, generation)
    }

    fn settle(&mut self, fs: usize, line: usize) {
        let Some(context) = self.contexts.get_mut(&fs) else {
            return;
        };
        if context
            .pending_cwd
            .as_ref()
            .is_some_and(|pending| pending.until <= line)
        {
            let pending = context.pending_cwd.take();
            if !context.uncertain {
                context.cwd = pending.and_then(|pending| pending.target);
            }
        }
        if context
            .pending_transition
            .is_some_and(|pending| pending.until <= line)
        {
            context.pending_transition = None;
        }
        context
            .pending_fork_snapshots
            .retain(|pending| pending.until > line);
    }

    fn cwd_before(&mut self, fs: usize, line: usize) -> Option<String> {
        self.settle(fs, line);
        self.contexts.get(&fs).and_then(|context| {
            (!context.uncertain
                && context.pending_cwd.is_none()
                && context.pending_transition.is_none())
            .then(|| context.cwd.clone())
            .flatten()
        })
    }

    fn has_unsettled_change(&self, fs: usize, line: usize) -> bool {
        self.contexts.get(&fs).is_some_and(|context| {
            context
                .pending_cwd
                .as_ref()
                .is_some_and(|pending| pending.until > line)
                || context
                    .pending_transition
                    .is_some_and(|pending| pending.until > line)
        })
    }

    fn has_pending_fork_snapshot(&self, fs: usize, line: usize) -> bool {
        self.contexts.get(&fs).is_some_and(|context| {
            context
                .pending_fork_snapshots
                .iter()
                .any(|pending| pending.until > line)
        })
    }

    fn add_pending_fork_snapshot(
        &mut self,
        source_fs: usize,
        until: usize,
        spawn_event: usize,
        child_fs: usize,
    ) {
        if let Some(source) = self.contexts.get_mut(&source_fs) {
            source.pending_fork_snapshots.push(PendingForkSnapshot {
                until,
                spawn_event,
                child_fs,
            });
        }
    }

    fn snapshot(&self, fs: usize) -> (Option<String>, bool) {
        self.contexts
            .get(&fs)
            .map(|context| (context.cwd.clone(), context.uncertain))
            .unwrap_or((None, true))
    }

    fn is_uncertain(&self, fs: usize) -> bool {
        self.contexts
            .get(&fs)
            .is_none_or(|context| context.uncertain)
    }

    fn task_fs(&self, generation: TaskGenerationId) -> Option<usize> {
        self.tasks
            .get(&generation)
            .filter(|task| !task.exited)
            .map(|task| task.fs)
    }

    /// Mark only the connected FS contexts uncertain. The returned indexes are
    /// the earlier evidence rows whose context claim became ambiguous later.
    fn invalidate(&mut self, first: usize, line: usize) -> Vec<usize> {
        let mut pending_events = vec![];
        let mut todo = vec![first];
        let mut seen = std::collections::BTreeSet::new();
        while let Some(fs) = todo.pop() {
            if !seen.insert(fs) {
                continue;
            }
            self.settle(fs, line);
            let Some(context) = self.contexts.get_mut(&fs) else {
                continue;
            };
            if let Some(pending) = context.pending_cwd.take() {
                pending_events.push(pending.event);
            }
            if let Some(pending) = context.pending_transition.take() {
                pending_events.push(pending.event);
                todo.push(pending.peer);
            }
            context
                .pending_fork_snapshots
                .retain(|pending| pending.until > line);
            for pending in context.pending_fork_snapshots.drain(..) {
                pending_events.push(pending.spawn_event);
                todo.push(pending.child_fs);
            }
            context.cwd = None;
            context.uncertain = true;
        }
        pending_events
    }

    fn replace_child(&mut self, child: TaskGenerationId, fs: usize) {
        if let Some(old) = self.tasks.insert(child, Task { fs, exited: false })
            && !old.exited
            && old.fs != fs
        {
            self.remove_member(old.fs);
        }
        if let Some(context) = self.contexts.get_mut(&fs) {
            context.members += 1;
        }
    }

    fn private_child(
        &mut self,
        child: TaskGenerationId,
        cwd: Option<String>,
        uncertain: bool,
    ) -> usize {
        if let Some(old) = self.tasks.remove(&child)
            && !old.exited
        {
            self.remove_member(old.fs);
        }
        self.new_context(cwd, uncertain, child)
    }

    fn exit(&mut self, generation: TaskGenerationId) {
        let fs = match self.tasks.get_mut(&generation) {
            Some(task) if !task.exited => {
                task.exited = true;
                task.fs
            }
            _ => return,
        };
        self.remove_member(fs);
    }
}
#[cfg(test)]
fn parse(raw: &str, file: &str) -> Parsed {
    parse_with_cwd(raw, file, None)
}

/// Incremental raw-trace parser. Line-local syntax is decoded eagerly, while
/// task identity and filesystem-context backfills remain a finish-time pass.
///
/// This type retains normalized events, parser-private actions, and unfinished
/// syscall prefixes, but not the complete raw input. It performs no I/O and
/// cannot be serialized as part of a stored run.
pub(crate) struct TraceParser<'a> {
    file: &'a str,
    initial_cwd: Option<&'a str>,
    parsed: Parsed,
    pending: BTreeMap<String, (String, usize)>,
    actions: Vec<FsAction>,
    saw_line: bool,
    last_line_terminated: bool,
}

impl<'a> TraceParser<'a> {
    /// Start an incremental parse for one evidence file and optional observed CWD.
    ///
    /// `initial_cwd` is context only; it never supplies absolute identity for a
    /// relative syscall operand.
    pub(crate) fn new(file: &'a str, initial_cwd: Option<&'a str>) -> Self {
        Self {
            file,
            initial_cwd,
            parsed: Parsed::default(),
            pending: BTreeMap::new(),
            actions: Vec::new(),
            saw_line: false,
            last_line_terminated: false,
        }
    }

    /// Push one logical trace line.
    ///
    /// The caller may retain or omit its trailing newline; only the final
    /// pushed line controls the truncation warning. Embedded newlines are not a
    /// chunking API: callers must identify logical lines before this boundary.
    pub(crate) fn push_line(&mut self, line: &str) {
        let (line, terminated) = match line.strip_suffix('\n') {
            Some(line) => (line.strip_suffix('\r').unwrap_or(line), true),
            None => (line, false),
        };
        self.saw_line = true;
        self.last_line_terminated = terminated;
        self.parsed.lines += 1;
        parse_trace_line(
            self.file,
            self.parsed.lines,
            line,
            &mut self.parsed,
            &mut self.pending,
            &mut self.actions,
        );
    }

    /// Finalize pending calls and apply identity/context resolution exactly once.
    ///
    /// Consuming `self` prevents later lines from invalidating the returned
    /// TaskTimeline or its finish-time uncertainty backfills.
    pub(crate) fn finish(mut self) -> Parsed {
        if self.saw_line && !self.last_line_terminated {
            self.parsed.warnings.push(format!(
                "{}: trace ends without newline; possible truncation",
                self.file
            ));
        }
        for (_, (_, line)) in self.pending {
            self.parsed.warnings.push(format!(
                "{}:{line}: unfinished call at end of trace",
                self.file
            ));
        }
        let timeline = TaskTimeline::from_events(&self.parsed.events);
        timeline.apply_identity_findings(&mut self.parsed);
        resolve_context(&mut self.parsed, self.initial_cwd, &self.actions, &timeline);
        self.parsed.timeline = timeline;
        self.parsed
    }
}

fn parse_trace_line(
    file: &str,
    n: usize,
    line: &str,
    p: &mut Parsed,
    pending: &mut BTreeMap<String, (String, usize)>,
    actions: &mut Vec<FsAction>,
) {
    let Some((pid, body)) = line.trim().split_once(char::is_whitespace) else {
        p.warnings.push(format!("{file}:{n}: malformed line"));
        return;
    };
    if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
        p.warnings
            .push(format!("{file}:{n}: invalid process identifier"));
        return;
    }
    let mut body = body.trim().to_string();
    let mut first = n;
    if let Some(prefix) = body.strip_suffix("<unfinished ...>") {
        if pending.insert(pid.into(), (prefix.into(), n)).is_some() {
            p.warnings
                .push(format!("{file}:{n}: replaced unfinished call"));
        }
        return;
    }
    if body.starts_with("<... ") {
        if let (Some((prefix, start)), Some((_, tail))) =
            (pending.remove(pid), body.split_once(" resumed>"))
        {
            let resumed = body
                .strip_prefix("<... ")
                .and_then(|s| s.split_once(" resumed>"))
                .map(|(op, _)| op);
            if resumed != prefix.split_once('(').map(|(op, _)| op) {
                p.warnings.push(format!(
                    "{file}:{n}: resumed syscall does not match line {start}"
                ));
                return;
            }
            body = format!("{prefix}{tail}");
            first = start;
        } else {
            p.warnings
                .push(format!("{file}:{n}: unmatched resumed call"));
            return;
        }
    }
    if body.contains("...") {
        p.warnings.push(format!(
            "{file}:{n}: abbreviated trace data; inspect raw evidence"
        ));
    }
    let evidence = Evidence {
        file: file.into(),
        line: first,
        end_line: n,
    };
    if body.starts_with("--- ") {
        return;
    }
    if body.starts_with("+++ ") {
        let exited = body
            .strip_prefix("+++ exited with ")
            .and_then(|s| s.strip_suffix(" +++"))
            .is_some_and(|s| s.parse::<u8>().is_ok());
        let killed = body
            .strip_prefix("+++ killed by ")
            .and_then(|s| s.strip_suffix(" +++"))
            .is_some_and(|s| {
                !s.is_empty()
                    && s.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "_+- ()".contains(c))
            });
        if !exited && !killed {
            p.warnings
                .push(format!("{file}:{n}: unrecognized process status: {body}"));
            return;
        }
        p.events.push(Event {
            schema_version: 2,
            pid: pid.into(),
            kind: "exit".into(),
            operation: "exit".into(),
            path: None,
            outcome: body,
            evidence,
            argv: None,
            child_pid: None,
            cwd_before: None,
            context_uncertain: false,
        });
        actions.push(FsAction::Exit);
        return;
    }
    let result = (|| -> Option<(Event, FsAction)> {
        let (op, rest) = body.split_once('(')?;
        let (arg, ret) = syscall_result(rest)?;
        let a = args(arg);
        let value = ret.split_whitespace().next()?;
        let outcome = if value == "-1" {
            ret.split_whitespace().nth(1)?.to_string()
        } else if value
            .split('<')
            .next()?
            .parse::<i64>()
            .is_ok_and(|v| v >= 0)
        {
            "success".into()
        } else {
            return None;
        };
        let operation = trace_operation(op)?;
        let (kind, mut path) = match operation {
            TraceOperation::Exec { at, .. } => ("exec", Some(path(&a, at)?)),
            TraceOperation::File { at, .. } => ("file", Some(path(&a, at)?)),
            TraceOperation::Cwd { fd: false } => ("cwd", Some(path(&a, false)?)),
            TraceOperation::Cwd { fd: true } => {
                ("cwd", a.first().and_then(|fd| annotated_path(fd)))
            }
            TraceOperation::Spawn => ("spawn", Some(value.into())),
            TraceOperation::Unshare | TraceOperation::Invalidate(_) => ("context", None),
        };
        // A successful open's returned descriptor annotation is an observation
        // from this syscall, unlike a historical chdir spelling. Keep raw
        // evidence as the source of the original relative operand.
        let relative_operand = if let TraceOperation::File { at, .. } = operation {
            a.get(usize::from(at))
                .and_then(|operand| unquote(operand))
                .is_some_and(|operand| !operand.starts_with(b"/"))
        } else {
            false
        };
        if matches!(
            operation,
            TraceOperation::File {
                returned_path: true,
                ..
            }
        ) && outcome == "success"
            && relative_operand
            && let Some(observed) = annotated_path(value)
        {
            path = Some(observed);
        }
        let argv = if let TraceOperation::Exec { argv_index, .. } = operation {
            a.get(argv_index)
                .and_then(|s| s.strip_prefix('[')?.strip_suffix(']'))
                .and_then(|s| {
                    if s.is_empty() {
                        Some(vec![])
                    } else {
                        args(s)
                            .into_iter()
                            .map(|v| unquote(v).map(|b| bytes(&b)))
                            .collect()
                    }
                })
        } else {
            None
        };
        let action = if outcome == "success" {
            match operation {
                TraceOperation::Spawn => spawn_action(op, &a),
                TraceOperation::Unshare => unshare_action(&a),
                TraceOperation::Invalidate(scope) => FsAction::Invalidate(scope),
                _ => FsAction::None,
            }
        } else {
            FsAction::None
        };
        Some((
            Event {
                schema_version: 2,
                pid: pid.into(),
                kind: kind.into(),
                operation: op.into(),
                path,
                child_pid: (kind == "spawn" && outcome == "success").then(|| value.to_string()),
                context_uncertain: false,
                argv,
                cwd_before: None,
                outcome,
                evidence,
            },
            action,
        ))
    })();
    if let Some((event, action)) = result {
        p.events.push(event);
        actions.push(action);
    } else {
        p.warnings.push(format!(
            "{file}:{n}: unsupported or incomplete event: {body}"
        ));
    }
}

/// Parse one complete UTF-8 trace while preserving the legacy whole-input API.
///
/// This adapter defines line boundaries with `split_inclusive`, so evidence
/// coordinates, CRLF handling, EOF warnings, warning order, and finish-time
/// backfills are identical to feeding the same lines through [`TraceParser`].
pub(crate) fn parse_with_cwd(raw: &str, file: &str, initial_cwd: Option<&str>) -> Parsed {
    let mut parser = TraceParser::new(file, initial_cwd);
    for line in raw.split_inclusive('\n') {
        parser.push_line(line);
    }
    parser.finish()
}
fn apply_fs_uncertainty(p: &mut Parsed, state: &mut FsState, fs: usize, line: usize) {
    let affected = state.invalidate(fs, line);
    for index in affected {
        if let Some(previous) = p.events.get_mut(index) {
            previous.context_uncertain = true;
        }
    }
}

fn mark_fs_uncertain(p: &mut Parsed, state: &mut FsState, fs: usize, event: usize, reason: &str) {
    let line = p.events[event].evidence.line;
    let mut affected = state.invalidate(fs, line);
    affected.push(event);
    affected.sort_unstable();
    affected.dedup();
    for index in affected {
        if let Some(previous) = p.events.get_mut(index) {
            previous.context_uncertain = true;
        }
    }
    if let Some(current) = p.events.get(event) {
        p.warnings.push(format!(
            "{}:{}: fs.context: {reason}; affected relative path context is incomplete",
            current.evidence.file, current.evidence.line
        ));
    }
}

fn mark_all_fs_uncertain(p: &mut Parsed, state: &mut FsState, event: usize, reason: &str) {
    let line = p.events[event].evidence.line;
    let contexts: Vec<usize> = state.contexts.keys().copied().collect();
    let mut affected = vec![event];
    for fs in contexts {
        affected.extend(state.invalidate(fs, line));
    }
    affected.sort_unstable();
    affected.dedup();
    for index in affected {
        if let Some(previous) = p.events.get_mut(index) {
            previous.context_uncertain = true;
        }
    }
    if let Some(current) = p.events.get(event) {
        p.warnings.push(format!(
            "{}:{}: fs.context: {reason}; affected relative path context is incomplete",
            current.evidence.file, current.evidence.line
        ));
    }
}

fn resolve_context(
    p: &mut Parsed,
    initial_cwd: Option<&str>,
    actions: &[FsAction],
    timeline: &TaskTimeline,
) {
    // Creation is ordered by syscall entry evidence, including unfinished calls,
    // so a child can receive its FS context before its first observed syscall.
    let mut state = FsState {
        tasks: BTreeMap::new(),
        contexts: BTreeMap::new(),
        next_context: 0,
    };

    for index in timeline.events_in_evidence_order() {
        let action = actions.get(index).copied().unwrap_or(FsAction::None);
        let event = &p.events[index];
        let kind = event.kind.clone();
        let path = event.path.clone();
        let outcome = event.outcome.clone();
        let argv_missing = event.kind == "exec" && event.argv.is_none();
        let start = event.evidence.line;
        let end = event.evidence.end_line;

        let generation = timeline.event_generation(index);
        let fs = state.ensure_task(generation, timeline.origin(generation), initial_cwd);
        let base = state.cwd_before(fs, start);
        p.events[index].cwd_before = base.clone();
        if state.is_uncertain(fs) || timeline.is_untrusted(generation) {
            p.events[index].context_uncertain = true;
        }

        match action {
            FsAction::Spawn(relation) => {
                let Some(child) = timeline.spawned_generation(index) else {
                    mark_fs_uncertain(
                        p,
                        &mut state,
                        fs,
                        index,
                        "spawn result lacked a usable child task identity",
                    );
                    continue;
                };
                if let TaskGenerationOrigin::LiveCollision { previous, .. } = timeline.origin(child)
                {
                    if let Some(previous_fs) = state.task_fs(previous) {
                        apply_fs_uncertainty(p, &mut state, previous_fs, start);
                    }
                    let child_fs = state.private_child(child, None, true);
                    match relation {
                        SpawnEffect::PrivateFsNamespaceUncertain => mark_fs_uncertain(
                            p,
                            &mut state,
                            child_fs,
                            index,
                            "namespace creation leaves only the new child path context unproven",
                        ),
                        SpawnEffect::Unknown => mark_fs_uncertain(
                            p,
                            &mut state,
                            fs,
                            index,
                            "clone flags did not prove whether filesystem state is shared",
                        ),
                        SpawnEffect::SharedFs | SpawnEffect::PrivateFs => {}
                    }
                } else {
                    debug_assert!(matches!(
                        timeline.origin(child),
                        TaskGenerationOrigin::Spawned { parent, event }
                            if parent == generation && event == index
                    ));
                    match relation {
                        SpawnEffect::SharedFs => state.replace_child(child, fs),
                        SpawnEffect::PrivateFs if !state.has_unsettled_change(fs, start) => {
                            let (cwd, uncertain) = state.snapshot(fs);
                            let child_fs = state.private_child(child, cwd, uncertain);
                            let until = timeline
                                .first_actor_event(child)
                                .map(|event| p.events[event].evidence.line)
                                .unwrap_or(end)
                                .min(end);
                            if !uncertain && until > start {
                                state.add_pending_fork_snapshot(fs, until, index, child_fs);
                            }
                        }
                        SpawnEffect::PrivateFs => {
                            mark_fs_uncertain(
                                p,
                                &mut state,
                                fs,
                                index,
                                "private clone overlapped an unresolved FS transition",
                            );
                            state.private_child(child, None, true);
                        }
                        SpawnEffect::PrivateFsNamespaceUncertain => {
                            // The namespace branch is uncertain by definition,
                            // so an in-flight source transition cannot weaken
                            // the parent any further. Keep that uncertainty on
                            // the new child rather than destroying source facts.
                            let child_fs = state.private_child(child, None, true);
                            mark_fs_uncertain(
                                p,
                                &mut state,
                                child_fs,
                                index,
                                "namespace creation leaves only the new child path context unproven",
                            );
                        }
                        SpawnEffect::Unknown => {
                            mark_fs_uncertain(
                                p,
                                &mut state,
                                fs,
                                index,
                                "clone flags did not prove whether filesystem state is shared",
                            );
                            state.private_child(child, None, true);
                        }
                    }
                }
            }
            FsAction::Unshare(action) => match action {
                UnshareEffect::NoFsChange => {}
                UnshareEffect::Unknown => mark_fs_uncertain(
                    p,
                    &mut state,
                    fs,
                    index,
                    "unshare flags did not prove its filesystem effect",
                ),
                UnshareEffect::SplitFs => {
                    if state.has_pending_fork_snapshot(fs, start) {
                        mark_fs_uncertain(
                            p,
                            &mut state,
                            fs,
                            index,
                            "unshare overlapped an unresolved private clone snapshot",
                        );
                    } else if state.has_unsettled_change(fs, start) {
                        mark_fs_uncertain(
                            p,
                            &mut state,
                            fs,
                            index,
                            "unshare overlapped an unresolved FS transition",
                        );
                    }
                    let (cwd, uncertain) = state.snapshot(fs);
                    let new_fs = state.private_child(generation, cwd, uncertain);
                    // A caller cannot issue another syscall until unshare returns,
                    // but a former sharing member can. Keep that race explicit.
                    if end > start && !uncertain && state.contexts.contains_key(&fs) {
                        if let Some(old) = state.contexts.get_mut(&fs) {
                            old.pending_transition = Some(PendingTransition {
                                until: end,
                                event: index,
                                peer: new_fs,
                            });
                        }
                        if let Some(new) = state.contexts.get_mut(&new_fs) {
                            new.pending_transition = Some(PendingTransition {
                                until: end,
                                event: index,
                                peer: fs,
                            });
                        }
                    }
                }
                UnshareEffect::SplitFsNamespaceUncertain => {
                    let overlaps_fork = state.has_pending_fork_snapshot(fs, start);
                    if overlaps_fork {
                        mark_fs_uncertain(
                            p,
                            &mut state,
                            fs,
                            index,
                            "namespace change overlapped an unresolved private clone snapshot",
                        );
                    }
                    // The caller's new namespace context is already unknown.
                    // Preserve any in-flight CWD fact on the old shared group;
                    // it remains true for those tasks regardless of when the
                    // caller detached during the syscall interval.
                    let new_fs = state.private_child(generation, None, true);
                    if !overlaps_fork {
                        mark_fs_uncertain(
                            p,
                            &mut state,
                            new_fs,
                            index,
                            "namespace change leaves only the caller path context unproven",
                        );
                    }
                }
            },
            FsAction::Invalidate(scope) => match scope {
                InvalidationScope::ConnectedFs => mark_fs_uncertain(
                    p,
                    &mut state,
                    fs,
                    index,
                    "root or namespace operation is outside the CWD model",
                ),
                InvalidationScope::AllActiveContexts => mark_all_fs_uncertain(
                    p,
                    &mut state,
                    index,
                    "mount namespace transition has process-wide unmodeled reach",
                ),
            },
            FsAction::Exit => state.exit(generation),
            FsAction::None => {}
        }

        if kind == "cwd" && outcome == "success" {
            let verified_target = path
                .as_ref()
                .filter(|target| target.starts_with('/'))
                .cloned();
            if state.has_pending_fork_snapshot(fs, start) {
                mark_fs_uncertain(
                    p,
                    &mut state,
                    fs,
                    index,
                    "directory change overlapped an unresolved private clone snapshot",
                );
            } else if verified_target.is_none() {
                mark_fs_uncertain(
                    p,
                    &mut state,
                    fs,
                    index,
                    "successful directory change has no absolute per-syscall directory evidence",
                );
            } else if state.has_unsettled_change(fs, start) {
                mark_fs_uncertain(
                    p,
                    &mut state,
                    fs,
                    index,
                    "directory changes overlap and their order is not proven",
                );
            } else {
                if let Some(context) = state.contexts.get_mut(&fs) {
                    if end > start && !context.uncertain {
                        context.pending_cwd = Some(PendingCwd {
                            until: end,
                            target: verified_target,
                            event: index,
                        });
                    } else {
                        context.cwd = verified_target;
                    }
                }
            }
        }

        if path
            .as_ref()
            .is_some_and(|value| value.starts_with("relative:"))
            || (kind == "cwd" && path.is_none())
        {
            p.events[index].context_uncertain = true;
            // A successful unresolved directory change already emitted the
            // context-level warning above. Failed calls do not transition the
            // context, but their own relative operand remains unverified.
            if kind != "cwd" || outcome != "success" {
                p.warnings.push(format!(
                    "{}:{}: path context unresolved; last observed CWD is not a verified current path; consult raw evidence",
                    p.events[index].evidence.file, p.events[index].evidence.line
                ));
            }
        }
        if argv_missing {
            p.warnings.push(format!(
                "{}:{}: execution arguments incomplete",
                p.events[index].evidence.file, p.events[index].evidence.line
            ));
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn assert_parsed_equivalent(expected: &Parsed, actual: &Parsed) {
        assert_eq!(
            serde_json::to_value(&expected.events).unwrap(),
            serde_json::to_value(&actual.events).unwrap()
        );
        assert_eq!(expected.warnings, actual.warnings);
        assert_eq!(expected.lines, actual.lines);
        assert_eq!(expected.timeline.event_order, actual.timeline.event_order);
        assert_eq!(
            expected.timeline.event_generations,
            actual.timeline.event_generations
        );
        assert_eq!(
            expected.timeline.spawned_generations,
            actual.timeline.spawned_generations
        );
        assert_eq!(
            expected.timeline.generations.len(),
            actual.timeline.generations.len()
        );
        for (expected, actual) in expected
            .timeline
            .generations
            .iter()
            .zip(&actual.timeline.generations)
        {
            assert_eq!(expected.pid, actual.pid);
            assert_eq!(expected.ordinal, actual.ordinal);
            assert_eq!(expected.origin, actual.origin);
            assert_eq!(expected.first_actor_event, actual.first_actor_event);
            assert_eq!(expected.exit_event, actual.exit_event);
        }
    }

    #[test]
    fn incremental_line_chunks_match_the_compatibility_adapter() {
        let raw = concat!(
            "10 execve(\"/tool\", [\"tool\"], 0x0) = 0\r\n",
            "10 clone(flags=SIGCHLD <unfinished ...>\n",
            "11 open(\"/child\", O_RDONLY) = 3\n",
            "10 <... clone resumed>) = 11\n",
            "11 +++ exited with 0 +++\n",
            "10 +++ exited with 0 +++\n",
        );
        let expected = parse_with_cwd(raw, "trace", Some("/initial"));
        let mut parser = TraceParser::new("trace", Some("/initial"));
        for line in [
            "10 execve(\"/tool\", [\"tool\"], 0x0) = 0\r\n",
            "10 clone(flags=SIGCHLD <unfinished ...>",
            "11 open(\"/child\", O_RDONLY) = 3\n",
            "10 <... clone resumed>) = 11",
            "11 +++ exited with 0 +++\n",
            "10 +++ exited with 0 +++\n",
        ] {
            parser.push_line(line);
        }
        let actual = parser.finish();

        assert_parsed_equivalent(&expected, &actual);
    }

    #[test]
    fn incremental_empty_and_final_unterminated_inputs_match_adapter() {
        for raw in [
            "",
            "1 execve(\"/tool\", [\"tool\"], 0x0) = 0\n",
            "1 execve(\"/tool\", [\"tool\"], 0x0) = 0",
            "1 open(\"/file\", O_RDONLY <unfinished ...>",
        ] {
            let expected = parse_with_cwd(raw, "trace", Some("/initial"));
            let mut parser = TraceParser::new("trace", Some("/initial"));
            for line in raw.split_inclusive('\n') {
                parser.push_line(line);
            }
            let actual = parser.finish();
            assert_parsed_equivalent(&expected, &actual);
        }

        let parsed = parse_with_cwd("1 open(\"/file\", O_RDONLY <unfinished ...>", "trace", None);
        assert_eq!(
            parsed.warnings,
            [
                "trace: trace ends without newline; possible truncation",
                "trace:1: unfinished call at end of trace",
            ]
        );
    }

    #[test]
    fn unfinished_resumed_lines_keep_evidence_across_pushes() {
        let raw = concat!(
            "1 open(\"/file\", O_RDONLY <unfinished ...>\n",
            "1 --- SIGCHLD {si_signo=SIGCHLD} ---\n",
            "1 <... open resumed>) = 3<\\x2f\\x66\\x69\\x6c\\x65>\n",
            "1 +++ exited with 0 +++\n",
        );
        let expected = parse_with_cwd(raw, "trace", Some("/initial"));
        let mut parser = TraceParser::new("trace", Some("/initial"));
        for line in raw.split_inclusive('\n') {
            parser.push_line(line);
        }
        let actual = parser.finish();

        assert_parsed_equivalent(&expected, &actual);
        assert_eq!(actual.events[0].evidence.line, 1);
        assert_eq!(actual.events[0].evidence.end_line, 3);
        assert_eq!(actual.events[0].path.as_deref(), Some("/file"));
    }

    #[test]
    fn finish_applies_task_identity_backfill_after_line_decoding() {
        let mut parser = TraceParser::new("trace", Some("/initial"));
        parser.push_line("1 execve(\"/tool\", [\"tool\"], 0x0) = 0\n");
        parser.push_line("2 open(\"/observed\", O_RDONLY) = 3\n");
        parser.push_line("2 +++ exited with 0 +++\n");
        parser.push_line("1 +++ exited with 0 +++\n");
        assert!(!parser.parsed.events[1].context_uncertain);

        let parsed = parser.finish();

        assert!(parsed.events[1].context_uncertain);
        assert!(
            parsed
                .warnings
                .iter()
                .any(|warning| warning.contains("Unintroduced"))
        );
    }

    #[test]
    fn fixture() {
        let p = parse(
            include_str!("../../tests/fixtures/trace.txt"),
            "raw/trace.log",
        );
        assert_eq!(p.events.len(), 5);
        assert_eq!(p.events[0].path.as_deref(), Some("/bin/x"));
        assert_eq!(p.events[1].outcome, "ENOENT");
        assert_eq!(p.events[2].evidence.line, 3);
        assert_eq!(p.events[2].evidence.end_line, 4);
        assert_eq!(p.events[3].path.as_deref(), Some("/a\" \\xff"));
        assert_eq!(p.warnings.len(), 3);
        assert!(
            p.warnings
                .iter()
                .any(|warning| warning.contains("task.identity: Unintroduced"))
        );
    }
}

#[cfg(test)]
mod boundary_tests {
    use super::*;

    #[test]
    fn trace_selector_is_derived_from_the_parser_contract() {
        let expected = [
            "execve",
            "execveat",
            "open",
            "openat",
            "openat2",
            "access",
            "faccessat",
            "faccessat2",
            "chdir",
            "fchdir",
            "clone",
            "clone3",
            "fork",
            "vfork",
            "unshare",
            "setns",
            "chroot",
            "pivot_root",
            "mount",
            "umount2",
            "move_mount",
            "mount_setattr",
        ];
        let names = TRACE_CONTRACTS
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>();
        assert_eq!(names, expected);
        assert_eq!(
            names
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            names.len(),
            "trace contract names must be unique"
        );
        assert_eq!(trace_selector(), format!("trace={}", expected.join(",")));
        assert!(names.iter().all(|name| trace_operation(name).is_some()));
        for name in ["mount", "umount2", "move_mount", "mount_setattr"] {
            assert_eq!(
                trace_operation(name),
                Some(TraceOperation::Invalidate(
                    InvalidationScope::AllActiveContexts
                ))
            );
        }
    }

    #[test]
    fn relative_dirfd_and_unknown_cwd() {
        let p = parse(
            "1 openat(3<\\x2f\\x64>, \"\\x78\", O_RDONLY) = 4\n1 open(\"\\x78\", O_RDONLY) = -1 EACCES (Permission denied)\n",
            "trace",
        );
        assert_eq!(p.events[0].path.as_deref(), Some("/d/x"));
        assert_eq!(p.events[1].outcome, "EACCES");
        assert_eq!(p.warnings.len(), 1);
    }
    #[test]
    fn exec_attempt_is_not_success() {
        let p = parse(
            "1 execve(\"\\x2f\\x78\", [], 0x0) = -1 ENOENT (No such file)\n",
            "trace",
        );
        assert_eq!(p.events[0].kind, "exec");
        assert_eq!(p.events[0].outcome, "ENOENT");
    }
    #[test]
    fn malformed_truncated_and_mismatched_calls_warn() {
        for raw in [
            "1 open(\"\\x2f\"...",
            "1 madeup() = 0\n",
            "1 <... open resumed>) = 0\n",
            "1 open( <unfinished ...>\n1 <... access resumed>) = 0\n",
            "bad execve(\"\\x2f\", [], 0) = 0\n",
        ] {
            assert!(!parse(raw, "trace").warnings.is_empty(), "{raw}");
        }
    }
    #[test]
    fn byte_encoding_is_unambiguous() {
        assert_ne!(bytes(b"\\xff"), bytes(&[255]));
        assert_eq!(bytes(b"a\n\x1b"), "a\\x0a\\x1b");
    }
}

#[cfg(test)]
mod context_tests {
    use super::*;
    #[test]
    fn fork_inherits_cwd_before_parent_resume_and_failed_chdir_preserves_it() {
        let raw = concat!(
            "1 execve(\"\\x2f\\x70\", [\"\\x70\"], 0x0) = 0\n",
            // This test isolates fork ordering, so provide sufficient
            // per-syscall evidence for the preceding CWD transition.
            "1 chdir(\"\\x2f\\x70\\x72\\x6f\\x6a\\x65\\x63\\x74\\x2f\\x73\\x75\\x62\") = 0\n",
            "1 clone(flags=SIGCHLD <unfinished ...>\n",
            "2 open(\"\\x78\", O_RDONLY) = 3\n",
            "1 <... clone resumed>) = 2\n",
            "2 chdir(\"\\x6e\\x6f\") = -1 ENOENT (No such file)\n",
            "2 access(\"\\x79\", R_OK) = 0\n",
            "1 open(\"\\x7a\", O_RDONLY) = 3\n"
        );
        let p = parse_with_cwd(raw, "trace", Some("/project"));
        // Three unresolved file operands plus the failed relative chdir's own
        // unverified operand; the failure still leaves the context unchanged.
        assert_eq!(p.warnings.len(), 4);
        assert!(
            p.events
                .iter()
                .filter(|e| e.kind == "file")
                .all(|e| e.context_uncertain && e.cwd_before.as_deref() == Some("/project/sub"))
        );
        let paths: Vec<_> = p
            .events
            .iter()
            .filter(|e| e.kind == "file")
            .map(|e| e.path.as_deref().unwrap())
            .collect();
        assert_eq!(
            paths,
            [
                "relative:x [cwd unresolved]",
                "relative:y [cwd unresolved]",
                "relative:z [cwd unresolved]"
            ]
        );
        let spawn = p.events.iter().find(|e| e.kind == "spawn").unwrap();
        assert_eq!(spawn.child_pid.as_deref(), Some("2"));
        assert_eq!(spawn.cwd_before.as_deref(), Some("/project/sub"));
    }
    #[test]
    fn child_chdir_does_not_change_parent_and_fchdir_uses_evidence() {
        let raw = concat!(
            "1 fork() = 2\n",
            "2 fchdir(3<\\x2f\\x64>) = 0\n",
            "2 open(\"\\x78\", O_RDONLY) = 4\n",
            "1 open(\"\\x78\", O_RDONLY) = 4\n"
        );
        let p = parse_with_cwd(raw, "trace", Some("/original"));
        assert_eq!(p.warnings.len(), 2);
        assert_eq!(
            p.events[2].path.as_deref(),
            Some("relative:x [cwd unresolved]")
        );
        assert_eq!(
            p.events[3].path.as_deref(),
            Some("relative:x [cwd unresolved]")
        );
        assert_eq!(p.events[2].cwd_before.as_deref(), Some("/d"));
        assert_eq!(p.events[3].cwd_before.as_deref(), Some("/original"));
        assert!(p.events[2].context_uncertain && p.events[3].context_uncertain);
    }
    #[test]
    fn shared_cwd_and_unknown_dirfd_never_guess() {
        let raw = concat!(
            "1 clone(flags=CLONE_FS|SIGCHLD) = 2\n",
            "1 open(\"\\x78\", O_RDONLY) = 4\n",
            "2 openat(5, \"\\x78\", O_RDONLY) = 4\n"
        );
        let p = parse_with_cwd(raw, "trace", Some("/original"));
        assert!(p.events[1].path.as_ref().unwrap().starts_with("relative:"));
        assert!(
            p.events[2]
                .path
                .as_ref()
                .unwrap()
                .contains("dirfd unresolved")
        );
        // A proven CLONE_FS relationship is modeled per context; it no longer
        // produces the former run-global warning.
        assert_eq!(p.warnings.len(), 2);
    }
    #[test]
    fn namespace_change_invalidates_fallback() {
        let p = parse_with_cwd(
            "1 unshare(CLONE_NEWNS) = 0\n1 open(\"\\x78\", O_RDONLY) = 4\n",
            "trace",
            Some("/original"),
        );
        assert!(!p.warnings.is_empty());
        assert!(p.events[1].path.as_ref().unwrap().starts_with("relative:"));
    }
    #[test]
    fn malformed_non_ascii_escape_does_not_panic() {
        let p = parse("1 open(\"\\x世\", O_RDONLY) = 4\n", "trace");
        assert_eq!(p.events.len(), 0);
        assert!(!p.warnings.is_empty());
    }
}

#[cfg(test)]
mod signal_status_tests {
    use super::*;
    #[test]
    fn realtime_and_unknown_signal_evidence_survives() {
        let p = parse(
            "1 +++ killed by SIGRT_2 +++\n2 +++ killed by SIGFUTURE_1 +++\n3 +++ killed by 99 +++\n",
            "trace",
        );
        assert_eq!(p.events.len(), 3);
        assert_eq!(p.events[1].outcome, "+++ killed by SIGFUTURE_1 +++");
    }
}

#[cfg(test)]
mod field_regressions {
    use super::*;
    #[test]
    fn aligned_resumed_calls_preserve_success_and_evidence() {
        let raw = concat!(
            "1 execve(\"/tool\", [\"tool\"], 0x0 <unfinished ...>\n",
            "1 <... execve resumed>)             = 0\n",
            "1 clone(flags=SIGCHLD <unfinished ...>\n",
            "2 open(\"input\", O_RDONLY)       = 3\n",
            "1 <... clone resumed>)\t = 2\n",
            "1 open(\"/absent\", O_RDONLY) = -1 ENOENT (No such file)\n",
        );
        let p = parse_with_cwd(raw, "trace", Some("/work"));
        assert_eq!(p.warnings.len(), 1);
        assert_eq!(p.events.len(), 4);
        assert_eq!(p.events[0].outcome, "success");
        assert_eq!(
            (p.events[0].evidence.line, p.events[0].evidence.end_line),
            (1, 2)
        );
        assert_eq!(
            p.events[1].path.as_deref(),
            Some("relative:input [cwd unresolved]")
        );
        assert_eq!(p.events[1].cwd_before.as_deref(), Some("/work"));
        assert!(p.events[1].context_uncertain);
        assert_eq!(p.events[1].evidence.line, 4);
        assert_eq!(p.events[2].child_pid.as_deref(), Some("2"));
        assert_eq!(p.events[3].outcome, "ENOENT");
    }
    #[test]
    fn separators_inside_quoted_paths_do_not_split_returns() {
        let p = parse(
            "1 open(\"/name) = literal\", O_RDONLY)       = 3\n",
            "trace",
        );
        assert!(p.warnings.is_empty());
        assert_eq!(p.events[0].path.as_deref(), Some("/name) = literal"));
        assert!(
            !parse("1 open(\"/x\", O_RDONLY) garbage = 3\n", "trace")
                .warnings
                .is_empty()
        );
    }
}

#[cfg(test)]
mod cwd_uncertainty_tests {
    use super::*;

    #[test]
    fn xx_annotation_callers_preserve_their_existing_path_contracts() {
        for (value, expected) in [
            ("3<\\x2f\\x66>", Some("/f")),
            ("3<\\x2f\\xff>", Some("/\\xff")),
            ("3<\\x72\\x65\\x6c>", None),
            ("3<\\x2f\\x78 (deleted)>", None),
            ("3<\\x2f\\x78", None),
            ("3<>", None),
            ("3<\\x2f\\x7g>", None),
        ] {
            assert_eq!(annotated_path(value).as_deref(), expected, "{value}");
        }

        for (dirfd, operand, expected) in [
            ("3<\\x2f\\x64>", "\\x66", "/d/f"),
            ("3<\\x2f\\xff>", "\\x66", "/\\xff/f"),
            (
                "3<\\x72\\x65\\x6c>",
                "\\x66",
                "relative:f [dirfd:\\x72\\x65\\x6c]",
            ),
            (
                "3<\\x2f\\x78 (deleted)>",
                "\\x66",
                "relative:f [dirfd:\\x2f\\x78 (deleted)]",
            ),
            (
                "3<\\x2f\\x78",
                "\\x66",
                "relative:f [dirfd annotation incomplete]",
            ),
            ("3<>", "\\x66", "relative:f [dirfd:]"),
            ("3<\\x2f>", "", "relative: [dirfd:\\x2f]"),
        ] {
            let arguments = [dirfd, &format!("\"{operand}\"")];
            assert_eq!(path(&arguments, true).as_deref(), Some(expected), "{dirfd}");
        }
    }

    #[test]
    fn xx_annotation_observations_keep_warning_uncertainty_and_evidence() {
        for (raw, expected_path, uncertain, warning) in [
            (
                "1 open(\"file\", O_RDONLY) = 3<\\x2f\\xff>\n",
                "/\\xff",
                false,
                None,
            ),
            (
                "1 open(\"file\", O_RDONLY) = 3<\\x2f\\x78 (deleted)>\n",
                "relative:file [cwd unresolved]",
                true,
                Some("trace:1: path context unresolved;"),
            ),
            (
                "1 open(\"file\", O_RDONLY) = 3<\\x2f\\x78\n",
                "relative:file [cwd unresolved]",
                true,
                Some("trace:1: path context unresolved;"),
            ),
            (
                "1 openat(3<\\x2f\\xff>, \"file\", O_RDONLY) = 4\n",
                "/\\xff/file",
                false,
                None,
            ),
            (
                "1 openat(3<\\x2f\\x78 (deleted)>, \"file\", O_RDONLY) = 4\n",
                "relative:file [dirfd:\\x2f\\x78 (deleted)]",
                true,
                Some("trace:1: path context unresolved;"),
            ),
        ] {
            let parsed = parse_with_cwd(raw, "trace", Some("/history"));
            assert_eq!(parsed.events.len(), 1, "{raw}");
            let event = &parsed.events[0];
            assert_eq!(event.path.as_deref(), Some(expected_path), "{raw}");
            assert_eq!(event.context_uncertain, uncertain, "{raw}");
            assert_eq!((event.evidence.line, event.evidence.end_line), (1, 1));
            match warning {
                Some(prefix) => {
                    assert_eq!(parsed.warnings.len(), 1, "{raw}");
                    assert!(parsed.warnings[0].starts_with(prefix), "{raw}");
                }
                None => assert!(parsed.warnings.is_empty(), "{raw}"),
            }
        }

        // An unclosed angle annotation also prevents argument splitting, so
        // the parser preserves it only as raw evidence and a warning.
        let incomplete = parse_with_cwd(
            "1 openat(3<\\x2f\\x78, \"file\", O_RDONLY) = 4\n",
            "trace",
            Some("/history"),
        );
        assert!(incomplete.events.is_empty());
        assert_eq!(
            incomplete.warnings,
            vec![
                "trace:1: unsupported or incomplete event: openat(3<\\x2f\\x78, \"file\", O_RDONLY) = 4"
            ]
        );
    }

    #[test]
    fn descriptor_observation_overrides_history_only_for_this_access() {
        let p = parse_with_cwd(
            "1 open(\"file\", O_RDONLY) = 3<\\x2f\\x6e\\x65\\x77\\x2f\\x66\\x69\\x6c\\x65>\n1 access(\"file\", F_OK) = 0\n",
            "trace",
            Some("/old"),
        );
        assert_eq!(p.events[0].path.as_deref(), Some("/new/file"));
        assert!(!p.events[0].context_uncertain);
        assert_eq!(
            p.events[1].path.as_deref(),
            Some("relative:file [cwd unresolved]")
        );
        assert_eq!(p.events[1].cwd_before.as_deref(), Some("/old"));
        assert!(p.events[1].context_uncertain);
        assert_eq!(p.warnings.len(), 1);
    }
    #[test]
    fn resumed_open_annotation_keeps_both_evidence_lines() {
        let p = parse_with_cwd(
            "1 open(\"file\", O_RDONLY <unfinished ...>\n1 <... open resumed>) = 3<\\x2f\\x6e\\x65\\x77>\n",
            "trace",
            Some("/old"),
        );
        assert_eq!(p.events[0].path.as_deref(), Some("/new"));
        assert_eq!(
            (p.events[0].evidence.line, p.events[0].evidence.end_line),
            (1, 2)
        );
        assert!(p.warnings.is_empty());
    }
    #[test]
    fn missing_deleted_or_malformed_annotations_never_use_cwd() {
        for result in [
            "3",
            "3<\\x2f\\x78 (deleted)>",
            "3<\\x2f\\x78",
            "-1 ENOENT (No file)",
        ] {
            let p = parse_with_cwd(
                &format!("1 open(\"file\", O_RDONLY) = {result}\n"),
                "trace",
                Some("/old"),
            );
            assert_eq!(
                p.events[0].path.as_deref(),
                Some("relative:file [cwd unresolved]")
            );
            assert!(p.events[0].context_uncertain);
            assert_eq!(p.warnings.len(), 1);
        }
    }
    #[test]
    fn failed_switches_do_not_replace_last_observed_context() {
        let p = parse_with_cwd(
            "1 chdir(\"/missing\") = -1 ENOENT (No file)\n1 fchdir(-1) = -1 EBADF (Bad fd)\n1 access(\"file\", F_OK) = 0\n",
            "trace",
            Some("/old"),
        );
        assert_eq!(p.events[2].cwd_before.as_deref(), Some("/old"));
        assert_eq!(
            p.events[2].path.as_deref(),
            Some("relative:file [cwd unresolved]")
        );
        assert!(p.events[2].context_uncertain);
    }
    #[test]
    fn explicit_absolute_operand_and_complete_dirfd_are_observations() {
        let p = parse_with_cwd(
            "1 access(\"/given/file\", F_OK) = 0\n1 openat(3<\\x2f\\x64>, \"file\", O_RDONLY) = 4\n",
            "trace",
            Some("/old"),
        );
        assert_eq!(p.events[0].path.as_deref(), Some("/given/file"));
        assert_eq!(p.events[1].path.as_deref(), Some("/d/file"));
        assert!(p.warnings.is_empty());
        assert!(p.events.iter().all(|e| !e.context_uncertain));
    }

    #[test]
    fn cwd_transitions_require_absolute_per_syscall_evidence() {
        let accepted = parse_with_cwd(
            concat!(
                "1 chdir(\"/next\") = 0\n",
                "1 open(\"/probe\", O_RDONLY) = 3\n",
                "1 fchdir(3<\\x2f\\x66\\x64>) = 0\n",
                "1 open(\"/probe-again\", O_RDONLY) = 3\n",
            ),
            "trace",
            Some("/old"),
        );
        assert!(accepted.warnings.is_empty());
        assert_eq!(accepted.events[1].cwd_before.as_deref(), Some("/next"));
        assert_eq!(accepted.events[3].cwd_before.as_deref(), Some("/fd"));
        assert!(accepted.events.iter().all(|event| !event.context_uncertain));

        let relative = parse_with_cwd(
            "1 chdir(\"child\") = 0\n1 open(\"/probe\", O_RDONLY) = 3\n",
            "trace",
            Some("/old"),
        );
        assert_eq!(
            relative.events[0].path.as_deref(),
            Some("relative:child [cwd unresolved]")
        );
        assert_eq!(relative.events[0].cwd_before.as_deref(), Some("/old"));
        assert!(relative.events[0].context_uncertain);
        assert_eq!(relative.events[1].cwd_before, None);
        assert!(relative.events[1].context_uncertain);
        assert_eq!(
            relative.warnings,
            vec![
                "trace:1: fs.context: successful directory change has no absolute per-syscall directory evidence; affected relative path context is incomplete"
            ]
        );

        let missing_fd = parse_with_cwd(
            "1 fchdir(3) = 0\n1 open(\"/probe\", O_RDONLY) = 3\n",
            "trace",
            Some("/old"),
        );
        assert_eq!(missing_fd.events[0].path, None);
        assert!(missing_fd.events[0].context_uncertain);
        assert_eq!(missing_fd.events[1].cwd_before, None);
        assert!(missing_fd.events[1].context_uncertain);
        assert_eq!(missing_fd.warnings.len(), 1);
        assert!(missing_fd.warnings[0].starts_with("trace:1: fs.context:"));

        let failed = parse_with_cwd(
            "1 chdir(\"child\") = -1 ENOENT (No file)\n1 open(\"/probe\", O_RDONLY) = 3\n",
            "trace",
            Some("/old"),
        );
        assert_eq!(
            failed.events[0].path.as_deref(),
            Some("relative:child [cwd unresolved]")
        );
        assert!(failed.events[0].context_uncertain);
        assert_eq!(failed.events[1].cwd_before.as_deref(), Some("/old"));
        assert!(!failed.events[1].context_uncertain);
        assert_eq!(failed.warnings.len(), 1);
        assert!(failed.warnings[0].starts_with("trace:1: path context unresolved;"));
    }
}
