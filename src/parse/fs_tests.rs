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

fn event_at(p: &Parsed, line: usize) -> &crate::model::Event {
    p.events
        .iter()
        .find(|event| event.evidence.line == line)
        .unwrap()
}

#[test]
fn task_timeline_accepts_initial_and_each_fresh_spawn_generation() {
    let p = parse_with_cwd(
        concat!(
            "1 execve(\"/tool\", [], 0) = 0\n",
            "1 clone(flags=SIGCHLD <unfinished ...>\n",
            "2 open(\"/child\", O_RDONLY) = 3\n",
            "1 <... clone resumed>) = 2\n",
            "2 +++ exited with 0 +++\n",
            "1 clone(flags=SIGCHLD) = 2\n",
            "2 open(\"/second\", O_RDONLY) = 3\n",
            "2 +++ exited with 0 +++\n",
            "1 +++ exited with 0 +++\n",
        ),
        "trace",
        Some("/A"),
    );
    assert!(p.warnings.is_empty(), "{:?}", p.warnings);

    let first_child_event = p
        .events
        .iter()
        .position(|event| event.evidence.line == 3)
        .unwrap();
    let first_spawn = p
        .events
        .iter()
        .position(|event| event.evidence.line == 2)
        .unwrap();
    let first_child = p.timeline.spawned_generation(first_spawn).unwrap();
    assert_eq!(p.timeline.event_generation(first_child_event), first_child);
    assert!(matches!(
        p.timeline.origin(first_child),
        TaskGenerationOrigin::Spawned { event, .. } if event == first_spawn
    ));

    let second_spawn = p
        .events
        .iter()
        .position(|event| event.evidence.line == 6)
        .unwrap();
    let second_child = p.timeline.spawned_generation(second_spawn).unwrap();
    assert_ne!(first_child, second_child);
    assert_eq!(p.timeline.label(first_child), "2#1");
    assert_eq!(p.timeline.label(second_child), "2#2");
    assert!(matches!(
        p.timeline.origin(second_child),
        TaskGenerationOrigin::Spawned { event, .. } if event == second_spawn
    ));
}

#[test]
fn task_timeline_fails_closed_for_each_untrusted_origin() {
    for (raw, expected_warning, affected_line) in [
        (
            "1 execve(\"/tool\", [], 0) = 0\n2 open(\"/unintroduced\", O_RDONLY) = 3\n",
            "trace:2: task.identity: Unintroduced: task 2#1 appeared without a successful spawn; generation identity is incomplete",
            2,
        ),
        (
            "1 execve(\"/tool\", [], 0) = 0\n1 clone(flags=SIGCHLD) = 2\n2 +++ exited with 0 +++\n2 open(\"/reappeared\", O_RDONLY) = 3\n",
            "trace:4: task.identity: Reappeared: task 2#2 appeared after 2#1 exited at trace:3 without a successful spawn; generation identity is incomplete",
            4,
        ),
        (
            "1 execve(\"/tool\", [], 0) = 0\n1 clone(flags=SIGCHLD) = 2\n1 clone(flags=SIGCHLD) = 2\n2 open(\"/collision\", O_RDONLY) = 3\n",
            "trace:3: task.identity: LiveCollision: successful spawn assigned 2#2 while 2#1 was still live; generation identity is incomplete",
            4,
        ),
    ] {
        let p = parse_with_cwd(raw, "trace", Some("/A"));
        assert_eq!(
            p.warnings
                .iter()
                .filter(|warning| warning.contains("task.identity:"))
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec![expected_warning],
            "{:?}",
            p.warnings,
        );
        assert!(event_at(&p, affected_line).context_uncertain);
        assert_eq!(event_at(&p, affected_line).cwd_before, None);
    }
}

#[test]
fn spawned_child_inherits_an_untrusted_parents_fs_uncertainty() {
    let p = parse_with_cwd(
        concat!(
            "1 execve(\"/tool\", [], 0) = 0\n",
            "2 open(\"/unintroduced-parent\", O_RDONLY) = 3\n",
            "2 fork() = 3\n",
            "3 open(\"/spawned-child\", O_RDONLY) = 3\n",
        ),
        "trace",
        Some("/A"),
    );
    let spawn = p
        .events
        .iter()
        .position(|event| event.evidence.line == 3)
        .unwrap();
    let parent = p.timeline.event_generation(spawn);
    let child = p.timeline.spawned_generation(spawn).unwrap();
    assert!(matches!(
        p.timeline.origin(parent),
        TaskGenerationOrigin::Unintroduced { .. }
    ));
    assert!(matches!(
        p.timeline.origin(child),
        TaskGenerationOrigin::Spawned { parent: actual, .. } if actual == parent
    ));
    assert!(event_at(&p, 4).context_uncertain);
    assert_eq!(event_at(&p, 4).cwd_before, None);
    assert_eq!(
        p.warnings
            .iter()
            .filter(|warning| warning.contains("task.identity:"))
            .count(),
        1
    );
}

#[test]
fn live_collision_does_not_lend_the_new_exit_to_the_old_generation() {
    let p = parse_with_cwd(
        concat!(
            "1 execve(\"/tool\", [], 0) = 0\n",
            "1 fork() = 2\n",
            "1 fork() = 2\n",
            "2 open(\"/collision\", O_RDONLY) = 3\n",
            "2 +++ exited with 0 +++\n",
            "1 +++ exited with 0 +++\n",
        ),
        "trace",
        Some("/A"),
    );
    let first_spawn = p
        .events
        .iter()
        .position(|event| event.evidence.line == 2)
        .unwrap();
    let collision_spawn = p
        .events
        .iter()
        .position(|event| event.evidence.line == 3)
        .unwrap();
    let first_child = p.timeline.spawned_generation(first_spawn).unwrap();
    let collision_child = p.timeline.spawned_generation(collision_spawn).unwrap();
    assert_eq!(p.timeline.exit_event(first_child), None);
    let collision_exit = p.timeline.exit_event(collision_child).unwrap();
    assert_eq!(p.events[collision_exit].evidence.line, 5);
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
fn syscall_specific_flag_decoders_accept_only_their_audited_abi() {
    for (expression, expected) in [
        ("0", SpawnEffect::PrivateFs),
        ("SIGCHLD", SpawnEffect::PrivateFs),
        ("CLONE_FS|SIGCHLD", SpawnEffect::SharedFs),
        ("0x211", SpawnEffect::SharedFs),
        (
            "CLONE_VM|CLONE_FS|CLONE_SIGHAND|CLONE_THREAD",
            SpawnEffect::SharedFs,
        ),
        (
            "CLONE_VM|CLONE_SIGHAND|CLONE_THREAD|CLONE_PIDFD",
            SpawnEffect::PrivateFs,
        ),
        (
            "CLONE_NEWNS|SIGCHLD",
            SpawnEffect::PrivateFsNamespaceUncertain,
        ),
    ] {
        assert_eq!(
            decode_legacy_clone_flags(expression),
            expected,
            "legacy clone accepted {expression}"
        );
    }

    for (flags, exit_signal, expected) in [
        ("0", "0", SpawnEffect::PrivateFs),
        ("0x80", "SIGCHLD", SpawnEffect::PrivateFsNamespaceUncertain),
        (
            "CLONE_NEWTIME",
            "0",
            SpawnEffect::PrivateFsNamespaceUncertain,
        ),
        ("CLONE_FS", "SIGCHLD", SpawnEffect::SharedFs),
        (
            "CLONE_VM|CLONE_SIGHAND|CLONE_THREAD|CLONE_PIDFD",
            "0",
            SpawnEffect::PrivateFs,
        ),
        (
            "CLONE_NEWNS",
            "SIGCHLD",
            SpawnEffect::PrivateFsNamespaceUncertain,
        ),
        (
            "CLONE_EMPTY_MNTNS",
            "SIGCHLD",
            SpawnEffect::PrivateFsNamespaceUncertain,
        ),
        (
            "CLONE_PIDFD|CLONE_AUTOREAP|CLONE_NNP|CLONE_PIDFD_AUTOKILL",
            "0",
            SpawnEffect::PrivateFs,
        ),
    ] {
        assert_eq!(
            decode_clone3_flags(flags, exit_signal),
            expected,
            "clone3 accepted flags={flags}, exit_signal={exit_signal}"
        );
    }

    for (expression, expected) in [
        ("0", UnshareEffect::NoFsChange),
        ("CLONE_NEWTIME", UnshareEffect::SplitFsNamespaceUncertain),
        ("0x80", UnshareEffect::SplitFsNamespaceUncertain),
        ("CLONE_FS", UnshareEffect::SplitFs),
        ("CLONE_NEWUSER", UnshareEffect::SplitFsNamespaceUncertain),
        ("CLONE_NEWNS", UnshareEffect::SplitFsNamespaceUncertain),
        (
            "UNSHARE_EMPTY_MNTNS",
            UnshareEffect::SplitFsNamespaceUncertain,
        ),
        ("0x00100000", UnshareEffect::SplitFsNamespaceUncertain),
    ] {
        assert_eq!(
            decode_unshare_flags(expression),
            expected,
            "unshare accepted {expression}"
        );
    }
}

#[test]
fn syscall_specific_flag_decoders_fail_closed() {
    for expression in [
        "0x80",
        "0x8000000000000000",
        "CLONE_NEWTIME",
        "UNSHARE_EMPTY_MNTNS",
        "CLONE_CLEAR_SIGHAND",
        "CLONE_FS|CLONE_FUTURE|SIGCHLD",
        "CLONE_FS|CLONE_NEWNS|SIGCHLD",
        "CLONE_FS|CLONE_NEWNET|SIGCHLD",
        "CLONE_SIGHAND|SIGCHLD",
        "CLONE_PIDFD|CLONE_PARENT_SETTID|SIGCHLD",
        "CLONE_NEWIPC|CLONE_SYSVSEM|SIGCHLD",
        "CLONE_NEWPID|CLONE_PARENT",
        "CLONE_NEWUSER|CLONE_PARENT",
        "SIGCHLD|SIGTERM",
    ] {
        assert_eq!(
            decode_legacy_clone_flags(expression),
            SpawnEffect::Unknown,
            "legacy clone must reject {expression}"
        );
    }

    for (flags, exit_signal) in [
        ("0x11", "0"),
        ("0x8000000000000000", "0"),
        ("SIGCHLD", "0"),
        ("CLONE_DETACHED", "0"),
        ("UNSHARE_EMPTY_MNTNS", "0"),
        ("CLONE_FS|CLONE_FUTURE", "0"),
        ("CLONE_FS|CLONE_NEWNS", "0"),
        ("CLONE_FS|CLONE_NEWNET", "0"),
        ("CLONE_FS|CLONE_NEWTIME", "0"),
        ("CLONE_SIGHAND", "0"),
        ("CLONE_VM|CLONE_SIGHAND|CLONE_CLEAR_SIGHAND", "0"),
        ("CLONE_THREAD|CLONE_SIGHAND|CLONE_VM", "SIGCHLD"),
        ("CLONE_NEWPID|CLONE_PARENT", "0"),
        ("CLONE_NEWUSER|CLONE_PARENT", "0"),
        ("CLONE_AUTOREAP", "SIGCHLD"),
        ("CLONE_AUTOREAP|CLONE_PARENT", "0"),
        ("CLONE_NNP|CLONE_VM|CLONE_SIGHAND|CLONE_THREAD", "0"),
        ("CLONE_PIDFD_AUTOKILL", "0"),
        ("CLONE_PIDFD|CLONE_PIDFD_AUTOKILL", "0"),
        ("0", "SIGFUTURE"),
        ("0", "128"),
    ] {
        assert_eq!(
            decode_clone3_flags(flags, exit_signal),
            SpawnEffect::Unknown,
            "clone3 must reject flags={flags}, exit_signal={exit_signal}"
        );
    }

    for expression in [
        "0x8000000000000000",
        "CLONE_PARENT_SETTID",
        "CLONE_EMPTY_MNTNS",
        "CLONE_CLEAR_SIGHAND",
        "SIGCHLD",
        "CLONE_FS|CLONE_FUTURE",
        "CLONE_FS|0x8000000000000000",
    ] {
        assert_eq!(
            decode_unshare_flags(expression),
            UnshareEffect::Unknown,
            "unshare must reject {expression}"
        );
    }
}

#[test]
fn all_recognized_namespace_flags_have_typed_effects() {
    for flag in [
        "CLONE_NEWNS",
        "CLONE_NEWCGROUP",
        "CLONE_NEWUTS",
        "CLONE_NEWIPC",
        "CLONE_NEWUSER",
        "CLONE_NEWPID",
        "CLONE_NEWNET",
    ] {
        assert_eq!(
            decode_legacy_clone_flags(&format!("{flag}|SIGCHLD")),
            SpawnEffect::PrivateFsNamespaceUncertain,
            "legacy clone {flag}"
        );
    }
    for flag in [
        "CLONE_NEWTIME",
        "CLONE_NEWNS",
        "CLONE_NEWCGROUP",
        "CLONE_NEWUTS",
        "CLONE_NEWIPC",
        "CLONE_NEWUSER",
        "CLONE_NEWPID",
        "CLONE_NEWNET",
        "CLONE_EMPTY_MNTNS",
    ] {
        assert_eq!(
            decode_clone3_flags(flag, "SIGCHLD"),
            SpawnEffect::PrivateFsNamespaceUncertain,
            "clone3 {flag}"
        );
    }
    for flag in [
        "CLONE_NEWTIME",
        "CLONE_NEWNS",
        "CLONE_NEWCGROUP",
        "CLONE_NEWUTS",
        "CLONE_NEWIPC",
        "CLONE_NEWUSER",
        "CLONE_NEWPID",
        "CLONE_NEWNET",
        "UNSHARE_EMPTY_MNTNS",
    ] {
        assert_eq!(
            decode_unshare_flags(flag),
            UnshareEffect::SplitFsNamespaceUncertain,
            "unshare {flag}"
        );
    }
}

#[test]
fn clone_namespace_uncertainty_is_scoped_to_the_new_child() {
    let p = parse_with_cwd(
        concat!(
            "1 execve(\"/tool\", [], 0) = 0\n",
            "1 fork() = 3\n",
            "1 clone(flags=CLONE_NEWNET|SIGCHLD) = 2\n",
            "2 open(\"/child\", O_RDONLY) = 3\n",
            "1 open(\"/parent\", O_RDONLY) = 3\n",
            "3 open(\"/sibling\", O_RDONLY) = 3\n",
        ),
        "trace",
        Some("/A"),
    );
    assert!(
        p.warnings
            .iter()
            .any(|message| message.contains("namespace") && message.contains("new child")),
        "{:?}",
        p.warnings
    );
    assert!(event_at(&p, 3).context_uncertain);
    assert!(event_at(&p, 4).context_uncertain);
    assert!(!event_at(&p, 5).context_uncertain);
    assert!(!event_at(&p, 6).context_uncertain);
    assert_eq!(event_at(&p, 5).cwd_before.as_deref(), Some("/A"));
    assert_eq!(event_at(&p, 6).cwd_before.as_deref(), Some("/A"));
}

#[test]
fn unshare_namespace_uncertainty_is_scoped_to_the_caller() {
    let p = parse_with_cwd(
        concat!(
            "1 execve(\"/tool\", [], 0) = 0\n",
            "1 clone(flags=CLONE_FS|SIGCHLD) = 2\n",
            "1 clone(flags=CLONE_FS|SIGCHLD) = 3\n",
            "2 unshare(CLONE_NEWNET) = 0\n",
            "2 open(\"/caller\", O_RDONLY) = 3\n",
            "1 open(\"/parent\", O_RDONLY) = 3\n",
            "3 open(\"/sibling\", O_RDONLY) = 3\n",
        ),
        "trace",
        Some("/A"),
    );
    assert!(event_at(&p, 4).context_uncertain);
    assert!(event_at(&p, 5).context_uncertain);
    assert!(!event_at(&p, 6).context_uncertain);
    assert!(!event_at(&p, 7).context_uncertain);
    assert_eq!(event_at(&p, 6).cwd_before.as_deref(), Some("/A"));
    assert_eq!(event_at(&p, 7).cwd_before.as_deref(), Some("/A"));
}

#[test]
fn unfinished_unshare_namespace_keeps_the_old_group_certain() {
    let p = parse_with_cwd(
        concat!(
            "1 execve(\"/tool\", [], 0) = 0\n",
            "1 clone(flags=CLONE_FS|SIGCHLD) = 2\n",
            "1 clone(flags=CLONE_FS|SIGCHLD) = 3\n",
            "2 unshare(CLONE_NEWNET <unfinished ...>\n",
            "3 open(\"/sibling-during-unshare\", O_RDONLY) = 3\n",
            "2 <... unshare resumed>) = 0\n",
            "2 open(\"/caller-after-unshare\", O_RDONLY) = 3\n",
            "1 open(\"/parent-after-unshare\", O_RDONLY) = 3\n",
        ),
        "trace",
        Some("/A"),
    );
    assert!(event_at(&p, 4).context_uncertain);
    assert!(!event_at(&p, 5).context_uncertain);
    assert_eq!(event_at(&p, 5).cwd_before.as_deref(), Some("/A"));
    assert!(event_at(&p, 7).context_uncertain);
    assert!(!event_at(&p, 8).context_uncertain);
    assert_eq!(event_at(&p, 8).cwd_before.as_deref(), Some("/A"));
}

#[test]
fn namespace_branch_absorbs_overlapping_source_cwd_uncertainty() {
    let cloned = parse_with_cwd(
        concat!(
            "1 execve(\"/tool\", [], 0) = 0\n",
            "1 clone(flags=CLONE_FS|SIGCHLD) = 2\n",
            "1 chdir(\"/B\" <unfinished ...>\n",
            "2 clone(flags=CLONE_NEWNET|SIGCHLD) = 3\n",
            "1 <... chdir resumed>) = 0\n",
            "3 open(\"/namespace-child\", O_RDONLY) = 3\n",
            "1 open(\"/parent\", O_RDONLY) = 3\n",
            "2 open(\"/shared-sibling\", O_RDONLY) = 3\n",
        ),
        "trace",
        Some("/A"),
    );
    assert!(event_at(&cloned, 6).context_uncertain);
    for line in [7, 8] {
        assert!(!event_at(&cloned, line).context_uncertain, "line {line}");
        assert_eq!(event_at(&cloned, line).cwd_before.as_deref(), Some("/B"));
    }

    let unshared = parse_with_cwd(
        concat!(
            "1 execve(\"/tool\", [], 0) = 0\n",
            "1 clone(flags=CLONE_FS|SIGCHLD) = 2\n",
            "1 clone(flags=CLONE_FS|SIGCHLD) = 3\n",
            "1 chdir(\"/B\" <unfinished ...>\n",
            "2 unshare(CLONE_NEWNET) = 0\n",
            "1 <... chdir resumed>) = 0\n",
            "3 open(\"/old-group-sibling\", O_RDONLY) = 3\n",
            "2 open(\"/namespace-caller\", O_RDONLY) = 3\n",
            "1 open(\"/old-group-parent\", O_RDONLY) = 3\n",
        ),
        "trace",
        Some("/A"),
    );
    for line in [7, 9] {
        assert!(!event_at(&unshared, line).context_uncertain, "line {line}");
        assert_eq!(event_at(&unshared, line).cwd_before.as_deref(), Some("/B"));
    }
    assert!(event_at(&unshared, 8).context_uncertain);
    assert_eq!(event_at(&unshared, 8).cwd_before, None);
}

#[test]
fn failed_namespace_calls_do_not_transition_contexts() {
    let p = parse_with_cwd(
        concat!(
            "1 execve(\"/tool\", [], 0) = 0\n",
            "1 clone(flags=CLONE_FS|SIGCHLD) = 2\n",
            "1 clone(flags=CLONE_NEWNET|SIGCHLD) = -1 EPERM (denied)\n",
            "2 unshare(CLONE_NEWNS) = -1 EPERM (denied)\n",
            "2 setns(3, CLONE_NEWNET) = -1 EPERM (denied)\n",
            "1 chdir(\"/B\") = 0\n",
            "2 open(\"/still-shared\", O_RDONLY) = 3\n",
        ),
        "trace",
        Some("/A"),
    );
    assert!(p.warnings.is_empty(), "{:?}", p.warnings);
    assert_eq!(event_at(&p, 7).cwd_before.as_deref(), Some("/B"));
    assert!(!event_at(&p, 7).context_uncertain);
}

#[test]
fn successful_root_changes_invalidate_only_the_connected_context() {
    for operation in ["chroot(\"/new-root\")", "setns(3, CLONE_NEWNS)"] {
        let p = parse_with_cwd(
            &format!(
                "1 execve(\"/tool\", [], 0) = 0\n1 fork() = 3\n1 clone(flags=CLONE_FS|SIGCHLD) = 2\n2 {operation} = 0\n1 open(\"/parent\", O_RDONLY) = 3\n2 open(\"/member\", O_RDONLY) = 3\n3 open(\"/private\", O_RDONLY) = 3\n"
            ),
            "trace",
            Some("/A"),
        );
        assert!(event_at(&p, 4).context_uncertain, "{operation}");
        assert!(event_at(&p, 5).context_uncertain, "{operation}");
        assert!(event_at(&p, 6).context_uncertain, "{operation}");
        assert!(!event_at(&p, 7).context_uncertain, "{operation}");
        assert_eq!(event_at(&p, 7).cwd_before.as_deref(), Some("/A"));
    }
}

#[test]
fn successful_pivot_root_invalidates_all_active_contexts() {
    let p = parse_with_cwd(
        concat!(
            "1 execve(\"/tool\", [], 0) = 0\n",
            "1 fork() = 3\n",
            "1 clone(flags=CLONE_FS|SIGCHLD) = 2\n",
            "2 pivot_root(\"/new-root\", \"/old-root\") = 0\n",
            "1 open(\"/parent\", O_RDONLY) = 3\n",
            "2 open(\"/member\", O_RDONLY) = 3\n",
            "3 open(\"/private\", O_RDONLY) = 3\n",
        ),
        "trace",
        Some("/A"),
    );
    for line in 4..=7 {
        assert!(event_at(&p, line).context_uncertain, "line {line}");
    }
    assert!(
        p.warnings
            .iter()
            .any(|warning| warning.contains("process-wide unmodeled reach")),
        "{:?}",
        p.warnings
    );
}

#[test]
fn mount_operations_are_global_only_on_success() {
    let operations = [
        "mount(\"none\", \"/mnt\", \"tmpfs\", 0, NULL)",
        "umount2(\"/mnt\", MNT_DETACH)",
        "move_mount(AT_FDCWD, \"/old\", AT_FDCWD, \"/new\", 0)",
        "mount_setattr(AT_FDCWD, \"/mnt\", 0, {attr_set=MOUNT_ATTR_RDONLY}, 32)",
    ];
    for operation in operations {
        let success = parse_with_cwd(
            &format!(
                "1 execve(\"/tool\", [], 0) = 0\n1 fork() = 3\n1 clone(flags=CLONE_FS|SIGCHLD) = 2\n2 {operation} = 0\n1 open(\"/parent\", O_RDONLY) = 3\n2 open(\"/member\", O_RDONLY) = 3\n3 open(\"/private\", O_RDONLY) = 3\n"
            ),
            "trace",
            Some("/A"),
        );
        assert_eq!(success.warnings.len(), 1, "{operation}");
        assert!(
            success.warnings[0].starts_with("trace:4: fs.context:"),
            "{operation}: {:?}",
            success.warnings
        );
        let transition = event_at(&success, 4);
        assert_eq!(transition.operation, operation.split_once('(').unwrap().0);
        assert_eq!(
            (transition.evidence.line, transition.evidence.end_line),
            (4, 4)
        );
        assert_eq!(transition.kind, "context");
        assert_eq!(transition.outcome, "success");
        for line in 4..=7 {
            assert!(
                event_at(&success, line).context_uncertain,
                "{operation}: line {line}"
            );
        }

        let failed = parse_with_cwd(
            &format!(
                "1 execve(\"/tool\", [], 0) = 0\n1 fork() = 3\n1 clone(flags=CLONE_FS|SIGCHLD) = 2\n2 {operation} = -1 EPERM (denied)\n1 chdir(\"/B\") = 0\n2 open(\"/member\", O_RDONLY) = 3\n3 open(\"/private\", O_RDONLY) = 3\n"
            ),
            "trace",
            Some("/A"),
        );
        assert!(
            failed.warnings.is_empty(),
            "{operation}: {:?}",
            failed.warnings
        );
        assert_eq!(event_at(&failed, 4).outcome, "EPERM");
        assert!(!event_at(&failed, 4).context_uncertain);
        assert_eq!(event_at(&failed, 6).cwd_before.as_deref(), Some("/B"));
        assert_eq!(event_at(&failed, 7).cwd_before.as_deref(), Some("/A"));
        for line in 4..=7 {
            assert!(
                !event_at(&failed, line).context_uncertain,
                "{operation}: line {line}"
            );
        }
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
fn private_fork_snapshot_overlap_fails_closed() {
    let p = parse_with_cwd(
        concat!(
            "1 execve(\"/tool\", [], 0) = 0\n",
            "1 clone(flags=CLONE_FS|SIGCHLD) = 2\n",
            "1 fork( <unfinished ...>\n",
            "2 chdir(\"/B\") = 0\n",
            "1 <... fork resumed>) = 3\n",
            "3 open(\"payload\", O_RDONLY) = 4<\\x2f\\x42\\x2f\\x70>\n",
            "3 +++ exited with 0 +++\n",
            "2 +++ exited with 0 +++\n",
            "1 +++ exited with 0 +++\n",
        ),
        "trace",
        Some("/"),
    );
    assert_eq!(
        p.warnings,
        vec![
            "trace:4: fs.context: directory change overlapped an unresolved private clone snapshot; affected relative path context is incomplete"
        ]
    );
    for line in [3, 4, 6] {
        assert!(event_at(&p, line).context_uncertain, "line {line}");
    }
    assert_eq!(event_at(&p, 3).evidence.end_line, 5);
    assert_eq!(event_at(&p, 6).cwd_before, None);
}

#[test]
fn private_fork_snapshot_bound_accepts_child_before_resume_and_later_transition() {
    let p = parse_with_cwd(
        concat!(
            "1 execve(\"/tool\", [], 0) = 0\n",
            "1 fork( <unfinished ...>\n",
            "2 open(\"/child-before-resume\", O_RDONLY) = 3\n",
            "1 <... fork resumed>) = 2\n",
            "1 chdir(\"/B\") = 0\n",
            "2 open(\"/child-after-parent-change\", O_RDONLY) = 3\n",
        ),
        "trace",
        Some("/A"),
    );
    assert!(p.warnings.is_empty(), "{:?}", p.warnings);
    for line in [2, 3, 6] {
        assert!(!event_at(&p, line).context_uncertain, "line {line}");
    }
    assert_eq!(event_at(&p, 3).cwd_before.as_deref(), Some("/A"));
    assert_eq!(event_at(&p, 6).cwd_before.as_deref(), Some("/A"));
}

#[test]
fn concurrent_private_forks_without_a_context_transition_are_accepted() {
    let p = parse_with_cwd(
        concat!(
            "1 execve(\"/tool\", [], 0) = 0\n",
            "1 clone(flags=CLONE_FS|SIGCHLD) = 9\n",
            "1 fork( <unfinished ...>\n",
            "9 fork( <unfinished ...>\n",
            "1 <... fork resumed>) = 2\n",
            "9 <... fork resumed>) = 3\n",
            "2 open(\"/first-child\", O_RDONLY) = 3\n",
            "3 open(\"/second-child\", O_RDONLY) = 3\n",
        ),
        "trace",
        Some("/A"),
    );
    assert!(p.warnings.is_empty(), "{:?}", p.warnings);
    for line in [3, 4, 7, 8] {
        assert!(!event_at(&p, line).context_uncertain, "line {line}");
    }
    for line in [7, 8] {
        assert_eq!(event_at(&p, line).cwd_before.as_deref(), Some("/A"));
    }
}

#[test]
fn multiple_pending_forks_invalidate_only_the_still_open_window() {
    let p = parse_with_cwd(
        concat!(
            "1 execve(\"/tool\", [], 0) = 0\n",
            "1 clone(flags=CLONE_FS|SIGCHLD) = 8\n",
            "1 clone(flags=CLONE_FS|SIGCHLD) = 9\n",
            "1 fork( <unfinished ...>\n",
            "9 fork( <unfinished ...>\n",
            "3 open(\"/early-child\", O_RDONLY) = 3\n",
            "8 chdir(\"/B\") = 0\n",
            "9 <... fork resumed>) = 3\n",
            "2 open(\"/late-child\", O_RDONLY) = 3\n",
            "1 <... fork resumed>) = 2\n",
            "3 open(\"/early-child-later\", O_RDONLY) = 3\n",
        ),
        "trace",
        Some("/A"),
    );
    assert_eq!(p.warnings.len(), 1, "{:?}", p.warnings);
    assert!(p.warnings[0].starts_with("trace:7: fs.context:"));
    assert!(event_at(&p, 4).context_uncertain);
    assert!(event_at(&p, 7).context_uncertain);
    assert!(event_at(&p, 9).context_uncertain);
    for line in [5, 6, 11] {
        assert!(!event_at(&p, line).context_uncertain, "line {line}");
    }
    assert_eq!(event_at(&p, 11).cwd_before.as_deref(), Some("/A"));
}

#[test]
fn failed_transition_does_not_invalidate_a_pending_fork_snapshot() {
    let p = parse_with_cwd(
        concat!(
            "1 execve(\"/tool\", [], 0) = 0\n",
            "1 clone(flags=CLONE_FS|SIGCHLD) = 2\n",
            "1 fork( <unfinished ...>\n",
            "2 chdir(\"/B\") = -1 ENOENT (missing)\n",
            "1 <... fork resumed>) = 3\n",
            "3 open(\"/child\", O_RDONLY) = 3\n",
            "2 open(\"/source\", O_RDONLY) = 3\n",
        ),
        "trace",
        Some("/A"),
    );
    assert!(p.warnings.is_empty(), "{:?}", p.warnings);
    for line in [3, 4, 6, 7] {
        assert!(!event_at(&p, line).context_uncertain, "line {line}");
    }
    assert_eq!(event_at(&p, 6).cwd_before.as_deref(), Some("/A"));
    assert_eq!(event_at(&p, 7).cwd_before.as_deref(), Some("/A"));
}

#[test]
fn private_fork_snapshot_detects_other_context_transitions() {
    for transition in [
        "unshare(CLONE_FS)",
        "unshare(CLONE_NEWNET)",
        "chroot(\"/new-root\")",
        "setns(3, CLONE_NEWNS)",
    ] {
        let p = parse_with_cwd(
            &format!(
                "1 execve(\"/tool\", [], 0) = 0\n1 clone(flags=CLONE_FS|SIGCHLD) = 2\n1 fork( <unfinished ...>\n2 {transition} = 0\n1 <... fork resumed>) = 3\n3 open(\"/child\", O_RDONLY) = 3\n"
            ),
            "trace",
            Some("/A"),
        );
        assert!(
            p.warnings
                .iter()
                .any(|warning| warning.starts_with("trace:4: fs.context:")),
            "{transition}: {:?}",
            p.warnings
        );
        for line in [3, 4, 6] {
            assert!(
                event_at(&p, line).context_uncertain,
                "{transition}: line {line}"
            );
        }
    }
}

#[test]
fn unknown_flags_and_namespace_changes_remain_uncertain() {
    for call in [
        "clone(child_stack=NULL)",
        "clone(flags=CLONE_FUTURE)",
        "clone(flags=CLONE_FS|CLONE_FUTURE|SIGCHLD)",
        "clone(flags=0x8000000000000000)",
        "clone(flags=???)",
        "clone3(0x1234, 88)",
        "clone3({flags=CLONE_FS|CLONE_FUTURE, exit_signal=SIGCHLD}, 88)",
        "clone3({flags=0x8000000000000000, exit_signal=0}, 88)",
    ] {
        let p = observed(&format!("1 {call} = 2\n2 chdir(\"/B\") = 0\n"), "1");
        assert!(
            p.warnings.iter().any(|w| w.contains("fs.")),
            "{call}: {:?}",
            p.warnings
        );
        assert!(last_cwd(&p).is_none());
    }
    for call in [
        "unshare(???)",
        "unshare(0x8000000000000000)",
        "unshare(CLONE_FS|CLONE_FUTURE)",
        "unshare(CLONE_NEWNS)",
        "unshare(UNSHARE_EMPTY_MNTNS)",
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
