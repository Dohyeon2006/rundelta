use crate::model::{Event, Evidence, bytes};
use std::collections::BTreeMap;
#[cfg(test)]
mod fs_tests;
#[derive(Default)]
pub struct Parsed {
    pub events: Vec<Event>,
    pub warnings: Vec<String>,
    pub lines: usize,
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
// Decode only the complete -xx pathname annotation. No filesystem lookup.
fn annotated_path(value: &str) -> Option<String> {
    let encoded = value.split_once('<')?.1.strip_suffix('>')?;
    if encoded.is_empty()
        || !encoded.len().is_multiple_of(4)
        || !encoded.as_bytes().chunks(4).all(|c| c.starts_with(b"\\x"))
    {
        return None;
    }
    unquote(&format!("\"{encoded}\""))
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
            if encoded.len().is_multiple_of(4)
                && encoded.as_bytes().chunks(4).all(|c| c.starts_with(b"\\x"))
                && let Some(mut base) = unquote(&format!("\"{encoded}\""))
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
    Spawn(FsRelation),
    Unshare(UnshareAction),
    Invalidate,
    Exit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FsRelation {
    Shared,
    Private,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnshareAction {
    NoFsChange,
    Split,
    SplitAndInvalidate,
    Unknown,
}

#[derive(Clone, Copy)]
struct FlagInfo {
    clone_fs: bool,
    unshare_implies_fs: bool,
    changes_mount_namespace: bool,
}

// Extract only a strace flag field. The parser never turns an unrecognized
// spelling into a private-FS claim.
fn flags_expression<'a>(args: &'a [&'a str]) -> Option<&'a str> {
    args.iter().find_map(|arg| {
        let value = arg.split_once("flags=")?.1;
        let end = value
            .find(',')
            .or_else(|| value.find('}'))
            .unwrap_or(value.len());
        let value = value[..end].trim();
        (!value.is_empty()).then_some(value)
    })
}

fn known_flag(term: &str) -> bool {
    matches!(
        term,
        "0" | "SIGCHLD"
            | "CLONE_VM"
            | "CLONE_FS"
            | "CLONE_FILES"
            | "CLONE_SIGHAND"
            | "CLONE_PIDFD"
            | "CLONE_PTRACE"
            | "CLONE_VFORK"
            | "CLONE_PARENT"
            | "CLONE_THREAD"
            | "CLONE_NEWNS"
            | "CLONE_SYSVSEM"
            | "CLONE_SETTLS"
            | "CLONE_PARENT_SETTID"
            | "CLONE_CHILD_CLEARTID"
            | "CLONE_DETACHED"
            | "CLONE_UNTRACED"
            | "CLONE_CHILD_SETTID"
            | "CLONE_NEWCGROUP"
            | "CLONE_NEWUTS"
            | "CLONE_NEWIPC"
            | "CLONE_NEWUSER"
            | "CLONE_NEWPID"
            | "CLONE_NEWNET"
            | "CLONE_IO"
            | "CLONE_CLEAR_SIGHAND"
            | "CLONE_INTO_CGROUP"
            | "CLONE_NEWTIME"
    )
}

fn parse_number(value: &str) -> Option<u64> {
    value.strip_prefix("0x").map_or_else(
        || value.parse().ok(),
        |hex| u64::from_str_radix(hex, 16).ok(),
    )
}

fn flag_info_expression(expression: &str) -> Option<FlagInfo> {
    if let Some(bits) = parse_number(expression) {
        return Some(FlagInfo {
            clone_fs: bits & 0x0000_0200 != 0,
            // Linux ksys_unshare adds CLONE_FS for these two flags.
            unshare_implies_fs: bits & (0x0000_0200 | 0x0002_0000 | 0x1000_0000) != 0,
            changes_mount_namespace: bits & 0x0002_0000 != 0,
        });
    }
    let terms: Vec<_> = expression.split('|').map(str::trim).collect();
    if terms.is_empty() || terms.iter().any(|term| term.is_empty()) {
        return None;
    }
    let clone_fs = terms.contains(&"CLONE_FS");
    // An explicit CLONE_FS proves the FS relationship even if a future,
    // unrelated flag is not in this parser's vocabulary.
    if !clone_fs && terms.iter().any(|term| !known_flag(term)) {
        return None;
    }
    let mount = terms.contains(&"CLONE_NEWNS");
    Some(FlagInfo {
        clone_fs,
        unshare_implies_fs: clone_fs || mount || terms.contains(&"CLONE_NEWUSER"),
        changes_mount_namespace: mount,
    })
}

fn flag_info(args: &[&str]) -> Option<FlagInfo> {
    flag_info_expression(flags_expression(args)?)
}

fn spawn_action(operation: &str, args: &[&str]) -> FsAction {
    match operation {
        "fork" | "vfork" => FsAction::Spawn(FsRelation::Private),
        "clone" | "clone3" => FsAction::Spawn(match flag_info(args) {
            Some(flags) if flags.clone_fs => FsRelation::Shared,
            Some(_) => FsRelation::Private,
            None => FsRelation::Unknown,
        }),
        _ => FsAction::None,
    }
}

fn unshare_action(args: &[&str]) -> FsAction {
    // Unlike clone/clone3, strace prints unshare's bitmask as its first
    // positional argument rather than a `flags=` field.
    match flag_info(args).or_else(|| args.first().and_then(|arg| flag_info_expression(arg))) {
        Some(flags) if flags.changes_mount_namespace => {
            FsAction::Unshare(UnshareAction::SplitAndInvalidate)
        }
        Some(flags) if flags.unshare_implies_fs => FsAction::Unshare(UnshareAction::Split),
        Some(_) => FsAction::Unshare(UnshareAction::NoFsChange),
        None => FsAction::Unshare(UnshareAction::Unknown),
    }
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

struct FsContext {
    cwd: Option<String>,
    uncertain: bool,
    // A context only needs liveness, not a second copy of every task ID. Task
    // identity stays in FsState::tasks, so this remains O(tasks + contexts).
    members: usize,
    pending_cwd: Option<PendingCwd>,
    pending_transition: Option<PendingTransition>,
}

#[derive(Clone, Copy)]
struct Task {
    fs: usize,
    exited: bool,
}

struct FsState {
    tasks: BTreeMap<String, Task>,
    contexts: BTreeMap<usize, FsContext>,
    next_context: usize,
    initialized: bool,
}

impl FsState {
    fn new_context(&mut self, cwd: Option<String>, uncertain: bool, member: &str) -> usize {
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
            },
        );
        self.tasks.insert(
            member.to_owned(),
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

    fn ensure_task(&mut self, pid: &str, initial_cwd: Option<&str>) -> (usize, bool) {
        if let Some(task) = self.tasks.get(pid).copied()
            && !task.exited
            && self.contexts.contains_key(&task.fs)
        {
            return (task.fs, false);
        }
        let reuse = self.tasks.contains_key(pid);
        let (cwd, uncertain) = if self.initialized {
            (None, true)
        } else {
            self.initialized = true;
            (initial_cwd.map(str::to_owned), false)
        };
        (self.new_context(cwd, uncertain, pid), reuse)
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

    fn snapshot(&self, fs: usize) -> (Option<String>, bool) {
        self.contexts
            .get(&fs)
            .map(|context| (context.cwd.clone(), context.uncertain))
            .unwrap_or((None, true))
    }

    /// Mark only the connected FS contexts uncertain. The returned indexes are
    /// the earlier evidence rows whose context claim became ambiguous later.
    fn invalidate(&mut self, first: usize) -> Vec<usize> {
        let mut pending_events = vec![];
        let mut todo = vec![first];
        let mut seen = std::collections::BTreeSet::new();
        while let Some(fs) = todo.pop() {
            if !seen.insert(fs) {
                continue;
            }
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
            context.cwd = None;
            context.uncertain = true;
        }
        pending_events
    }

    fn replace_child(&mut self, child: &str, fs: usize) {
        if let Some(old) = self
            .tasks
            .insert(child.to_owned(), Task { fs, exited: false })
            && !old.exited
            && old.fs != fs
        {
            self.remove_member(old.fs);
        }
        if let Some(context) = self.contexts.get_mut(&fs) {
            context.members += 1;
        }
    }

    fn private_child(&mut self, child: &str, cwd: Option<String>, uncertain: bool) -> usize {
        if let Some(old) = self.tasks.remove(child)
            && !old.exited
        {
            self.remove_member(old.fs);
        }
        self.new_context(cwd, uncertain, child)
    }

    fn exit(&mut self, pid: &str) {
        let fs = match self.tasks.get_mut(pid) {
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
pub fn parse(raw: &str, file: &str) -> Parsed {
    parse_with_cwd(raw, file, None)
}
pub fn parse_with_cwd(raw: &str, file: &str, initial_cwd: Option<&str>) -> Parsed {
    let mut p = Parsed::default();
    let mut pending = BTreeMap::<String, (String, usize)>::new();
    let mut actions = vec![];
    for (idx, line) in raw.lines().enumerate() {
        let n = idx + 1;
        p.lines = n;
        let Some((pid, body)) = line.trim().split_once(char::is_whitespace) else {
            p.warnings.push(format!("{file}:{n}: malformed line"));
            continue;
        };
        if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
            p.warnings
                .push(format!("{file}:{n}: invalid process identifier"));
            continue;
        }
        let mut body = body.trim().to_string();
        let mut first = n;
        if let Some(prefix) = body.strip_suffix("<unfinished ...>") {
            if pending.insert(pid.into(), (prefix.into(), n)).is_some() {
                p.warnings
                    .push(format!("{file}:{n}: replaced unfinished call"));
            }
            continue;
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
                    continue;
                }
                body = format!("{prefix}{tail}");
                first = start;
            } else {
                p.warnings
                    .push(format!("{file}:{n}: unmatched resumed call"));
                continue;
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
            continue;
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
                continue;
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
            continue;
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
            let (kind, mut path) = match op {
                "execve" => ("exec", Some(path(&a, false)?)),
                "execveat" => ("exec", Some(path(&a, true)?)),
                "open" | "access" => ("file", Some(path(&a, false)?)),
                "openat" | "openat2" | "faccessat" | "faccessat2" => {
                    ("file", Some(path(&a, true)?))
                }
                "chdir" => ("cwd", Some(path(&a, false)?)),
                "fchdir" => ("cwd", a.first().and_then(|fd| annotated_path(fd))),
                "clone" | "clone3" | "fork" | "vfork" => ("spawn", Some(value.into())),
                "unshare" | "setns" | "chroot" | "pivot_root" => ("context", None),
                "exit" | "exit_group" => return None,
                _ => return None,
            };
            // A successful open's returned descriptor annotation is an observation
            // from this syscall, unlike a historical chdir spelling. Keep raw
            // evidence as the source of the original relative operand.
            let relative_operand = a
                .get(usize::from(matches!(op, "openat" | "openat2")))
                .and_then(|operand| unquote(operand))
                .is_some_and(|operand| !operand.starts_with(b"/"));
            if matches!(op, "open" | "openat" | "openat2")
                && outcome == "success"
                && relative_operand
                && let Some(observed) = annotated_path(value)
            {
                path = Some(observed);
            }
            let argv = if kind == "exec" {
                let index = if op == "execveat" { 2 } else { 1 };
                a.get(index)
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
                match op {
                    "clone" | "clone3" | "fork" | "vfork" => spawn_action(op, &a),
                    "unshare" => unshare_action(&a),
                    "setns" | "chroot" | "pivot_root" => FsAction::Invalidate,
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
        if let Some((e, action)) = result {
            p.events.push(e);
            actions.push(action);
        } else {
            p.warnings.push(format!(
                "{file}:{n}: unsupported or incomplete event: {body}"
            ));
        }
    }
    if !raw.is_empty() && !raw.ends_with('\n') {
        p.warnings.push(format!(
            "{file}: trace ends without newline; possible truncation"
        ));
    }
    for (_, (_, n)) in pending {
        p.warnings
            .push(format!("{file}:{n}: unfinished call at end of trace"));
    }
    resolve_context(&mut p, initial_cwd, &actions);
    p
}
fn lexical_cwd(path: Option<&String>, base: Option<&String>) -> Option<String> {
    let path = path?;
    if path.starts_with('/') {
        return Some(path.clone());
    }
    let relative = path
        .strip_prefix("relative:")?
        .strip_suffix(" [cwd unresolved]")?;
    base.map(|base| {
        format!(
            "{}{relative}",
            if base.ends_with('/') {
                base.clone()
            } else {
                format!("{base}/")
            }
        )
    })
}

fn mark_fs_uncertain(p: &mut Parsed, state: &mut FsState, fs: usize, event: usize, reason: &str) {
    let mut affected = state.invalidate(fs);
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

fn resolve_context(p: &mut Parsed, initial_cwd: Option<&str>, actions: &[FsAction]) {
    // Creation is ordered by syscall entry evidence, including unfinished calls,
    // so a child can receive its FS context before its first observed syscall.
    let mut order: Vec<usize> = (0..p.events.len()).collect();
    order.sort_by_key(|&i| (p.events[i].evidence.line, p.events[i].evidence.end_line, i));
    let mut state = FsState {
        tasks: BTreeMap::new(),
        contexts: BTreeMap::new(),
        next_context: 0,
        initialized: false,
    };

    for index in order {
        let action = actions.get(index).copied().unwrap_or(FsAction::None);
        let event = &p.events[index];
        let pid = event.pid.clone();
        let child = event.child_pid.clone();
        let kind = event.kind.clone();
        let path = event.path.clone();
        let outcome = event.outcome.clone();
        let argv_missing = event.kind == "exec" && event.argv.is_none();
        let start = event.evidence.line;
        let end = event.evidence.end_line;

        let (fs, reused) = state.ensure_task(&pid, initial_cwd);
        if reused {
            mark_fs_uncertain(
                p,
                &mut state,
                fs,
                index,
                "task identity appeared after an observed exit and cannot be safely reused",
            );
        }
        let base = state.cwd_before(fs, start);
        p.events[index].cwd_before = base.clone();

        match action {
            FsAction::Spawn(relation) => {
                let Some(child) = child.as_deref() else {
                    mark_fs_uncertain(
                        p,
                        &mut state,
                        fs,
                        index,
                        "spawn result lacked a usable child task identity",
                    );
                    continue;
                };
                let live_child = state.tasks.get(child).is_some_and(|task| !task.exited);
                match relation {
                    FsRelation::Shared if !live_child => state.replace_child(child, fs),
                    FsRelation::Private
                        if !live_child && !state.has_unsettled_change(fs, start) =>
                    {
                        let (cwd, uncertain) = state.snapshot(fs);
                        state.private_child(child, cwd, uncertain);
                    }
                    FsRelation::Private => {
                        mark_fs_uncertain(
                            p,
                            &mut state,
                            fs,
                            index,
                            "private clone overlapped an unresolved FS transition",
                        );
                        state.private_child(child, None, true);
                    }
                    FsRelation::Shared | FsRelation::Unknown => {
                        mark_fs_uncertain(
                            p,
                            &mut state,
                            fs,
                            index,
                            if live_child {
                                "spawn child identity was already live"
                            } else {
                                "clone flags did not prove whether filesystem state is shared"
                            },
                        );
                        state.private_child(child, None, true);
                    }
                }
            }
            FsAction::Unshare(action) => match action {
                UnshareAction::NoFsChange => {}
                UnshareAction::Unknown => mark_fs_uncertain(
                    p,
                    &mut state,
                    fs,
                    index,
                    "unshare flags did not prove its filesystem effect",
                ),
                UnshareAction::Split | UnshareAction::SplitAndInvalidate => {
                    if state.has_unsettled_change(fs, start) {
                        mark_fs_uncertain(
                            p,
                            &mut state,
                            fs,
                            index,
                            "unshare overlapped an unresolved FS transition",
                        );
                    }
                    let (cwd, uncertain) = state.snapshot(fs);
                    let new_fs = state.private_child(&pid, cwd, uncertain);
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
                    if action == UnshareAction::SplitAndInvalidate {
                        mark_fs_uncertain(
                            p,
                            &mut state,
                            new_fs,
                            index,
                            "mount namespace change leaves the new path context unproven",
                        );
                    }
                }
            },
            FsAction::Invalidate => mark_fs_uncertain(
                p,
                &mut state,
                fs,
                index,
                "root or namespace operation is outside the CWD model",
            ),
            FsAction::Exit => state.exit(&pid),
            FsAction::None => {}
        }

        if kind == "cwd" && outcome == "success" {
            if path.is_none() {
                mark_fs_uncertain(
                    p,
                    &mut state,
                    fs,
                    index,
                    "successful directory change has no verified directory evidence",
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
                let target = lexical_cwd(path.as_ref(), base.as_ref());
                if let Some(context) = state.contexts.get_mut(&fs) {
                    if end > start && !context.uncertain {
                        context.pending_cwd = Some(PendingCwd {
                            until: end,
                            target,
                            event: index,
                        });
                    } else {
                        context.cwd = target;
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
            // A chdir operand is an observed context change, not an absolute
            // access assertion. Unresolved file/exec targets do limit comparison.
            if kind != "cwd" || path.is_none() {
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
        assert_eq!(p.warnings.len(), 2);
    }
}

#[cfg(test)]
mod boundary_tests {
    use super::*;
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
            "1 chdir(\"\\x73\\x75\\x62\") = 0\n",
            "1 clone(flags=SIGCHLD <unfinished ...>\n",
            "2 open(\"\\x78\", O_RDONLY) = 3\n",
            "1 <... clone resumed>) = 2\n",
            "2 chdir(\"\\x6e\\x6f\") = -1 ENOENT (No such file)\n",
            "2 access(\"\\x79\", R_OK) = 0\n",
            "1 open(\"\\x7a\", O_RDONLY) = 3\n"
        );
        let p = parse_with_cwd(raw, "trace", Some("/project"));
        assert_eq!(p.warnings.len(), 3);
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
}
