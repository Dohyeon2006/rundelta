//! Expectations registered before implementing the FsContext model.
use super::*;

fn observed(raw: &str, pid: &str) -> Parsed {
    parse_with_cwd(
        &format!(
            "1 execve(\"/tool\", [], 0) = 0\n{raw}{pid} open(\"payload\", O_RDONLY) = 3<\\x2f\\x42\\x2f\\x70>\n"
        ),
        "raw/trace.log",
        Some("/A"),
    )
}
fn last_cwd(p: &Parsed) -> Option<&str> {
    p.events.last().and_then(|e| e.cwd_before.as_deref())
}
#[test]
fn shared_across_thread_groups_and_thread_flags() {
    for flags in [
        "CLONE_FS|SIGCHLD",
        "CLONE_VM|CLONE_SIGHAND|CLONE_THREAD|CLONE_FS",
        "0x211",
    ] {
        let p = observed(
            &format!("1 clone(flags={flags}) = 2\n2 chdir(\"/B\") = 0\n"),
            "1",
        );
        assert_eq!(last_cwd(&p), Some("/B"), "{flags}");
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
        assert!(!p.events[1].context_uncertain);
    }
}
#[test]
fn clone3_structure_and_private_inheritance() {
    let p = observed(
        "1 clone3({flags=CLONE_FS, exit_signal=SIGCHLD}, 88) = 2\n2 chdir(\"/B\") = 0\n",
        "1",
    );
    assert_eq!(last_cwd(&p), Some("/B"));
    assert!(p.warnings.is_empty());
    for call in [
        "fork()",
        "vfork()",
        "clone(flags=SIGCHLD)",
        "clone3({flags=0, exit_signal=SIGCHLD}, 88)",
    ] {
        let p = observed(&format!("1 {call} = 2\n1 chdir(\"/B\") = 0\n"), "2");
        assert_eq!(last_cwd(&p), Some("/A"));
        assert!(p.warnings.is_empty());
    }
}
#[test]
fn mixed_private_branch_and_pre_spawn_history_survive() {
    let p = observed(
        "1 fork() = 2\n1 clone(flags=CLONE_FS|SIGCHLD) = 3\n3 chdir(\"/B\") = 0\n",
        "2",
    );
    assert_eq!(p.events[0].cwd_before.as_deref(), Some("/A"));
    assert_eq!(last_cwd(&p), Some("/A"));
    assert!(p.warnings.is_empty());
}
#[test]
fn successful_unshare_splits_but_failure_keeps_alias() {
    for (ret, expected) in [("0", "/A"), ("-1 EPERM (denied)", "/B")] {
        let p = observed(
            &format!(
                "1 clone(flags=CLONE_FS|SIGCHLD) = 2\n2 unshare(CLONE_FS) = {ret}\n1 chdir(\"/B\") = 0\n"
            ),
            "2",
        );
        assert_eq!(last_cwd(&p), Some(expected));
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
    }
}
#[test]
fn exec_does_not_unshare_fs() {
    for ret in ["0", "-1 ENOENT (missing)"] {
        let p = observed(
            &format!(
                "1 clone(flags=CLONE_FS|SIGCHLD) = 2\n2 execve(\"/next\", [], 0) = {ret}\n1 chdir(\"/B\") = 0\n"
            ),
            "2",
        );
        assert_eq!(last_cwd(&p), Some("/B"));
        assert!(p.warnings.is_empty());
    }
}
#[test]
fn failed_cwd_changes_do_not_update_shared_state() {
    let p = observed(
        "1 clone(flags=CLONE_FS|SIGCHLD) = 2\n2 chdir(\"/missing\") = -1 ENOENT (missing)\n2 fchdir(-1) = -1 EBADF (bad fd)\n",
        "1",
    );
    assert_eq!(last_cwd(&p), Some("/A"));
    // The existing unresolved fchdir evidence warning remains; failure isn't a new CWD.
    assert_eq!(p.warnings.len(), 1);
    let p = observed(
        "1 clone(flags=CLONE_FS|SIGCHLD) = 2\n2 fchdir(3<\\x2f\\x42>) = 0\n",
        "1",
    );
    assert_eq!(last_cwd(&p), Some("/B"));
    assert!(p.warnings.is_empty());
}
#[test]
fn members_exit_without_destroying_survivors_context() {
    for exits in [
        "1 +++ exited with 0 +++\n2 +++ exited with 0 +++\n",
        "2 +++ exited with 0 +++\n1 +++ exited with 0 +++\n",
    ] {
        let p = observed(
            &format!(
                "1 clone(flags=CLONE_FS|SIGCHLD) = 2\n2 clone(flags=CLONE_FS|SIGCHLD) = 3\n2 chdir(\"/B\") = 0\n{exits}"
            ),
            "3",
        );
        assert_eq!(last_cwd(&p), Some("/B"));
        assert!(p.warnings.is_empty());
    }
}
#[test]
fn unfinished_clone_allows_child_activity_before_resume() {
    let p = observed(
        "1 clone(flags=CLONE_FS|SIGCHLD <unfinished ...>\n2 chdir(\"/B\") = 0\n1 <... clone resumed>) = 2\n",
        "1",
    );
    assert_eq!(last_cwd(&p), Some("/B"));
    assert!(p.warnings.is_empty());
    let spawn = p.events.iter().find(|e| e.kind == "spawn").unwrap();
    assert_eq!((spawn.evidence.line, spawn.evidence.end_line), (2, 4));
}
#[test]
fn unknown_flags_and_namespace_changes_remain_uncertain() {
    for call in [
        "clone(child_stack=NULL)",
        "clone(flags=CLONE_FUTURE)",
        "clone(flags=???)",
        "clone3(0x1234, 88)",
    ] {
        let p = observed(&format!("1 {call} = 2\n2 chdir(\"/B\") = 0\n"), "1");
        assert!(
            p.warnings.iter().any(|w| w.contains("fs.")),
            "{call}: {:?}",
            p.warnings
        );
        assert!(last_cwd(&p).is_none());
    }
    // An explicit FS bit proves this one relationship even if a future,
    // unrelated flag is not in this parser's vocabulary.
    let p = observed(
        "1 clone(flags=CLONE_FS|CLONE_FUTURE) = 2\n2 chdir(\"/B\") = 0\n",
        "1",
    );
    assert!(p.warnings.is_empty());
    assert_eq!(last_cwd(&p), Some("/B"));
    for call in [
        "unshare(???)",
        "unshare(CLONE_NEWNS)",
        "setns(3, 0)",
        "chroot(\"/B\")",
    ] {
        let p = observed(&format!("1 {call} = 0\n"), "1");
        assert!(!p.warnings.is_empty());
        assert!(last_cwd(&p).is_none());
    }
}
#[test]
fn rename_safety_and_returned_fd_observation_take_priority() {
    let p = parse_with_cwd(
        concat!(
            "1 clone(flags=CLONE_FS|SIGCHLD) = 2\n",
            "2 chdir(\"/old\") = 0\n",
            "1 openat(AT_FDCWD<\\x2f\\x6f\\x6c\\x64>, \"file\", O_RDONLY) = 3<\\x2f\\x6e\\x65\\x77\\x2f\\x66\\x69\\x6c\\x65>\n",
            "1 access(\"file\", F_OK) = 0\n"
        ),
        "trace",
        Some("/A"),
    );
    assert_eq!(p.events[2].path.as_deref(), Some("/new/file"));
    assert!(!p.events[2].context_uncertain);
    assert_eq!(p.events[3].cwd_before.as_deref(), Some("/old"));
    assert_eq!(
        p.events[3].path.as_deref(),
        Some("relative:file [cwd unresolved]")
    );
    assert!(p.events[3].context_uncertain);
    assert_eq!(p.warnings.len(), 1);
}
#[test]
fn overlapping_cwd_changes_are_not_serialized_as_proven() {
    let p = observed(
        "1 clone(flags=CLONE_FS|SIGCHLD) = 2\n1 chdir(\"/C\" <unfinished ...>\n2 chdir(\"/B\") = 0\n1 <... chdir resumed>) = 0\n",
        "2",
    );
    assert!(
        p.warnings
            .iter()
            .any(|w| w.contains("fs.context: directory changes overlap"))
    );
    assert!(last_cwd(&p).is_none());
}
