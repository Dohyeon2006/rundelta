//! Pure target-completion selection over one parsed trace.
//!
//! The parser owns evidence-ordered task lifetimes. Selection retains the saved
//! event vector's indices and original coordinates; only missing-generation
//! labels are sorted for the existing warning contract. The capture parent owns
//! streaming I/O, signal/collector facts, outcome decoding, reduction and storage.
//! No process result can manufacture a root event or complete another lifetime.

use crate::{
    model::{
        EventReference, TARGET_SELECTION_RULE_V1, TASK_IDENTITY_CONTRACT_V1, TargetProvenance,
        TaskIdentity,
    },
    parse,
};

/// Derive process completion from the parser's evidence-ordered task generations.
///
/// The root is the generation containing the first successful exec. An exit
/// satisfies only its own generation, so an earlier incarnation of the same
/// numeric PID cannot prove either root or child completion.
pub(super) struct Completion {
    root_exec: Option<usize>,
    root_generation: Option<parse::TaskGenerationId>,
    root_exit: Option<usize>,
    missing_exits: Vec<String>,
}

impl Completion {
    /// Use the parser's generation identities, never numeric PID coincidence.
    /// The first successful exec selects the root generation; an unsuccessful
    /// execution attempt alone cannot establish that root under this contract.
    pub(super) fn from_timeline(parsed: &parse::Parsed) -> Self {
        let timeline = &parsed.timeline;
        let root_exec = timeline.events_in_evidence_order().find(|&index| {
            let event = &parsed.events[index];
            event.kind == "exec" && event.outcome == "success"
        });
        let root_generation = root_exec.map(|index| timeline.event_generation(index));
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
            root_exec,
            root_generation,
            root_exit,
            missing_exits,
        }
    }

    /// Return the selected exit's original index in the saved event vector.
    /// Evidence-order selection never reorders that vector or its coordinates.
    pub(super) fn root_exit_index(&self) -> Option<usize> {
        self.root_exit
    }

    /// Borrow missing-generation labels in the frozen lexical warning order.
    pub(super) fn missing_exits(&self) -> &[String] {
        &self.missing_exits
    }

    /// Copy the already-selected root's exact event references and PID lifetime.
    ///
    /// The Parsed value must be the one used by from_timeline. Unknown target
    /// signals retain these references; absent root/exit evidence returns None.
    /// No collector result or newer parser participates in this projection.
    pub(super) fn provenance(&self, parsed: &parse::Parsed) -> Option<TargetProvenance> {
        let generation = self.root_generation?;
        let root_exec = self.root_exec?;
        let target_exit = self.root_exit?;
        Some(TargetProvenance {
            schema_version: 1,
            selection_rule: TARGET_SELECTION_RULE_V1.into(),
            identity_contract: TASK_IDENTITY_CONTRACT_V1.into(),
            root: TaskIdentity {
                pid: parsed.timeline.pid(generation).into(),
                generation: parsed.timeline.generation_ordinal(generation),
            },
            root_exec: EventReference {
                index: root_exec,
                evidence: parsed.events[root_exec].evidence.clone(),
            },
            target_exit: EventReference {
                index: target_exit,
                evidence: parsed.events[target_exit].evidence.clone(),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn completion(trace: &str) -> (parse::Parsed, Completion) {
        let parsed = parse::parse_with_cwd(trace, "fixture", Some("/"));
        let completion = Completion::from_timeline(&parsed);
        (parsed, completion)
    }

    #[test]
    fn provenance_locates_root_among_equal_child_and_reused_pid_exits() {
        let (mut parsed, actual) = completion(concat!(
            "20 execve(\"/root\", [], 0) = 0\n",
            "20 clone(flags=SIGCHLD <unfinished ...>\n",
            "3 execve(\"/child\", [], 0) = 0\n",
            "20 <... clone resumed>) = 3\n",
            "3 +++ exited with 0 +++\n",
            "20 fork() = 3\n",
            "3 +++ exited with 0 +++\n",
            "20 +++ exited with 0 +++\n",
        ));
        parsed.bind_task_generations();
        let provenance = actual.provenance(&parsed).expect("observed root outcome");
        assert_eq!(provenance.schema_version, 1);
        assert_eq!(provenance.selection_rule, TARGET_SELECTION_RULE_V1);
        assert_eq!(provenance.identity_contract, TASK_IDENTITY_CONTRACT_V1);
        assert_eq!(provenance.root.pid, "20");
        assert_eq!(provenance.root.generation, 1);
        assert_eq!(provenance.root_exec.index, 0);
        assert_eq!(provenance.root_exec.evidence.line, 1);
        assert_eq!(provenance.target_exit.index, 6);
        assert_eq!(provenance.target_exit.evidence.file, "fixture");
        assert_eq!(provenance.target_exit.evidence.line, 8);
        assert_eq!(provenance.target_exit.evidence.end_line, 8);
        assert_eq!(
            parsed.events[provenance.target_exit.index].task_generation,
            Some(1)
        );
        let child_exits = parsed
            .events
            .iter()
            .filter(|event| event.kind == "exit" && event.pid == "3")
            .map(|event| (event.outcome.as_str(), event.task_generation))
            .collect::<Vec<_>>();
        assert_eq!(
            child_exits,
            [
                ("+++ exited with 0 +++", Some(1)),
                ("+++ exited with 0 +++", Some(2))
            ]
        );

        // Equal outcome text and PID cannot replace the first selected root's
        // exit with a later incarnation. The saved index remains the first exit.
        let (mut parsed, actual) = completion(concat!(
            "20 execve(\"/root\", [], 0) = 0\n",
            "20 +++ exited with 0 +++\n",
            "20 execve(\"/reappeared\", [], 0) = 0\n",
            "20 +++ exited with 0 +++\n",
        ));
        parsed.bind_task_generations();
        let provenance = actual.provenance(&parsed).expect("original root exit");
        assert_eq!(provenance.root.generation, 1);
        assert_eq!(provenance.target_exit.index, 1);
        assert_eq!(provenance.target_exit.evidence.line, 2);
        assert_eq!(parsed.events[3].task_generation, Some(2));
        assert!(parsed.events[3].context_uncertain);
    }

    #[test]
    fn provenance_preserves_unfinished_root_exec_coordinates_and_evidence_order() {
        let (mut parsed, actual) = completion(concat!(
            "20 execve(\"/root\", [], 0 <unfinished ...>\n",
            "3 execve(\"/later\", [], 0) = 0\n",
            "20 <... execve resumed>) = 0\n",
            "20 +++ exited with 7 +++\n",
            "3 +++ exited with 0 +++\n",
        ));
        parsed.bind_task_generations();
        let provenance = actual.provenance(&parsed).expect("selected root exit");
        assert_eq!(provenance.root.pid, "20");
        assert_eq!(provenance.root_exec.index, 1);
        assert_eq!(provenance.root_exec.evidence.line, 1);
        assert_eq!(provenance.root_exec.evidence.end_line, 3);
        assert_eq!(provenance.target_exit.index, 2);
        assert_eq!(provenance.target_exit.evidence.line, 4);
        assert!(
            parsed
                .warnings
                .iter()
                .any(|warning| warning.contains("Unintroduced"))
        );
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
