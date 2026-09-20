use crate::model::{Evidence, Run};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
#[derive(Debug, Serialize)]
pub struct Change {
    pub category: String,
    pub subject: String,
    /// Presentation-only grouping; observations and evidence remain in changes.
    pub detail_group: Option<String>,
    pub left: Vec<String>,
    pub right: Vec<String>,
    pub left_evidence: Vec<Evidence>,
    pub right_evidence: Vec<Evidence>,
}
#[derive(Serialize)]
pub struct Report {
    pub schema_version: u32,
    pub observed_differences: Option<bool>,
    pub reliability: Reliability,
    pub exit_code: i32,
    pub errors: Vec<String>,
    pub left: String,
    pub right: String,
    pub left_id: String,
    pub right_id: String,
    pub changes: Vec<Change>,
    pub warnings: Vec<String>,
}
type Aggregate = BTreeMap<(String, String, String), BTreeMap<String, Vec<Evidence>>>;
fn aggregate(r: &Run) -> Aggregate {
    let mut a = Aggregate::new();
    for e in &r.events {
        if e.kind != "file" && e.kind != "exec" {
            continue;
        }
        if let Some(path) = &e.path {
            a.entry((e.kind.clone(), e.operation.clone(), path.clone()))
                .or_default()
                .entry(e.outcome.clone())
                .or_default()
                .push(e.evidence.clone());
        }
    }
    a
}
// Narrow, documented spelling heuristics, never equivalence/identity rules.
fn temporary_candidate(path: &str) -> bool {
    fn suffix(s: &str, prefix: &str) -> bool {
        s.strip_prefix(prefix)
            .is_some_and(|s| s.len() == 6 && s.bytes().all(|b| b.is_ascii_alphanumeric()))
    }
    path.split('/')
        .any(|part| suffix(part, "rustc") || suffix(part, "rmeta") || suffix(part, ".tmp"))
        || (path.contains("/objects/")
            && path
                .rsplit('/')
                .next()
                .is_some_and(|p| suffix(p, "tmp_obj_")))
        || (path.starts_with("/tmp/")
            && path
                .strip_suffix(".s")
                .and_then(|s| s.rsplit('/').next())
                .is_some_and(|s| suffix(s, "cc")))
}
fn priority(c: &Change) -> u8 {
    match c.category.as_str() {
        "Exit status" => 0,
        "Capture completeness" => 1,
        "Initial working directory" | "Command arguments" | "Selected startup environment" => 2,
        _ if c.left == ["success"]
            && !c.right.is_empty()
            && !c.right.iter().any(|s| s == "success") =>
        {
            3
        }
        "Execution attempt changes"
            if !c.right.is_empty() && !c.right.iter().any(|s| s == "success") =>
        {
            4
        }
        "Executable changes" if !c.right.is_empty() => 5,
        _ if c.detail_group.is_some() => 10,
        "New file access" => 6,
        "Executable changes" => 7,
        "File access outcome changes" | "Execution attempt changes" => 8,
        _ => 9,
    }
}
pub fn compare(l: &Run, r: &Run) -> Report {
    let mut report = Report {
        schema_version: 4,
        observed_differences: None,
        reliability: Reliability {
            left: Quality::of(l),
            right: Quality::of(r),
        },
        exit_code: 3,
        errors: vec![],
        left: l.metadata.name.clone(),
        right: r.metadata.name.clone(),
        left_id: l.metadata.id.clone(),
        right_id: r.metadata.id.clone(),
        changes: vec![],
        warnings: vec![],
    };
    let mut simple = |category: &str, left: String, right: String| {
        if left != right {
            report.changes.push(Change {
                category: category.into(),
                subject: String::new(),
                detail_group: None,
                left: vec![left],
                right: vec![right],
                left_evidence: vec![],
                right_evidence: vec![],
            });
        }
    };
    simple(
        "Exit status",
        format!(
            "code={:?}, signal={:?}",
            l.metadata.exit_code, l.metadata.signal
        ),
        format!(
            "code={:?}, signal={:?}",
            r.metadata.exit_code, r.metadata.signal
        ),
    );
    simple(
        "Initial working directory",
        l.metadata.cwd.clone(),
        r.metadata.cwd.clone(),
    );
    simple(
        "Command arguments",
        format!("{:?}", l.metadata.command),
        format!("{:?}", r.metadata.command),
    );
    simple(
        "Capture completeness",
        format!(
            "{}; {}; {} warnings",
            l.metadata.state,
            l.metadata
                .completeness
                .as_deref()
                .unwrap_or("legacy/unknown"),
            l.metadata.warnings.len()
        ),
        format!(
            "{}; {}; {} warnings",
            r.metadata.state,
            r.metadata
                .completeness
                .as_deref()
                .unwrap_or("legacy/unknown"),
            r.metadata.warnings.len()
        ),
    );
    let left_env = l.metadata.environment.clone().unwrap_or_default();
    let right_env = r.metadata.environment.clone().unwrap_or_default();
    for key in left_env
        .keys()
        .chain(right_env.keys())
        .collect::<BTreeSet<_>>()
    {
        let left = left_env.get(key);
        let right = right_env.get(key);
        if left != right {
            let display = |value: Option<&Option<String>>| match value {
                None => "(not selected/unknown)".into(),
                Some(None) => "(unset)".into(),
                Some(Some(s)) => format!("value={s:?}"),
            };
            report.changes.push(Change {
                category: "Selected startup environment".into(),
                subject: key.clone(),
                detail_group: None,
                left: vec![display(left)],
                right: vec![display(right)],
                left_evidence: vec![],
                right_evidence: vec![],
            });
        }
    }
    let a = aggregate(l);
    let b = aggregate(r);
    for key in a.keys().chain(b.keys()).collect::<BTreeSet<_>>() {
        let left = a.get(key).cloned().unwrap_or_default();
        let right = b.get(key).cloned().unwrap_or_default();
        if left.keys().eq(right.keys()) {
            continue;
        }
        let category = if key.0 == "exec" {
            if left.keys().chain(right.keys()).any(|s| s != "success") {
                "Execution attempt changes"
            } else {
                "Executable changes"
            }
        } else if left.is_empty() {
            "New file access"
        } else if right.is_empty() {
            "Removed file access"
        } else {
            "File access outcome changes"
        };
        report.changes.push(Change {
            category: category.into(),
            subject: format!("{} ({})", key.2, key.1),
            detail_group: if key.0 == "file" && (left.is_empty() || right.is_empty()) {
                if left.keys().chain(right.keys()).all(|s| s != "success") {
                    Some("One-sided unsuccessful file accesses (not necessarily faults)".into())
                } else if temporary_candidate(&key.2) {
                    Some("Temporary-looking file paths (heuristic, not ignored)".into())
                } else {
                    None
                }
            } else {
                None
            },
            left: left.keys().cloned().collect(),
            right: right.keys().cloned().collect(),
            left_evidence: left.values().flatten().take(3).cloned().collect(),
            right_evidence: right.values().flatten().take(3).cloned().collect(),
        });
    }
    for run in [l, r] {
        if run.storage_integrity.status != "verified" {
            report.warnings.push(format!(
                "{}: {}",
                run.metadata.name, run.storage_integrity.detail
            ));
        }
        if run.metadata.environment.is_none() {
            report.warnings.push(format!(
                "{}: legacy record has no environment selection data",
                run.metadata.name
            ));
        }
        if run.metadata.state != "completed"
            || !run.metadata.warnings.is_empty()
            || run.metadata.completeness.as_deref() != Some("complete_for_supported_events")
        {
            report.warnings.push(format!(
                "{}: state={}, {} parser warnings; inspect metadata.json",
                run.metadata.name,
                run.metadata.state,
                run.metadata.warnings.len()
            ));
        }
    }
    // Stable ordering within a tier retains deterministic path order. No event is suppressed.
    report.changes.sort_by_key(priority);
    report.observed_differences = Some(!report.changes.is_empty());
    report.exit_code = if report.reliability.left.trusted() && report.reliability.right.trusted() {
        i32::from(!report.changes.is_empty())
    } else {
        3
    };
    report
}

#[derive(Debug, Serialize)]
pub struct Quality {
    pub capture: String,
    pub storage: String,
}
impl Quality {
    pub fn of(run: &Run) -> Self {
        Self {
            capture: if run.storage_integrity.status == "unfinalized" {
                "unknown"
            } else if run.metadata.state == "completed"
                && run.metadata.completeness.as_deref() == Some("complete_for_supported_events")
            {
                "complete"
            } else if run.metadata.completeness.is_none() {
                "unknown"
            } else {
                "incomplete"
            }
            .into(),
            storage: run.storage_integrity.status.clone(),
        }
    }
    fn trusted(&self) -> bool {
        self.capture == "complete" && self.storage == "verified"
    }
}
#[derive(Debug, Serialize)]
pub struct Reliability {
    pub left: Quality,
    pub right: Quality,
}
pub fn evaluate(left: &str, right: &str, l: anyhow::Result<Run>, r: anyhow::Result<Run>) -> Report {
    match (l, r) {
        (Ok(l), Ok(r)) => compare(&l, &r),
        (l, r) => {
            let mut errors = vec![];
            let mut quality = |side: &str, result: &anyhow::Result<Run>| match result {
                Ok(run) => Quality::of(run),
                Err(error) => {
                    errors.push(format!("{side}: {error:#}"));
                    Quality {
                        capture: "unknown".into(),
                        storage: if crate::store::is_corrupt(error) {
                            "corrupt"
                        } else {
                            "unavailable"
                        }
                        .into(),
                    }
                }
            };
            let reliability = Reliability {
                left: quality("left", &l),
                right: quality("right", &r),
            };
            Report {
                schema_version: 4,
                observed_differences: None,
                reliability,
                exit_code: 2,
                errors,
                left: left.into(),
                right: right.into(),
                left_id: String::new(),
                right_id: String::new(),
                changes: vec![],
                warnings: vec![],
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod field_regressions {
    use super::*;
    pub(crate) fn run(raw: &str) -> Run {
        let parsed = crate::parse::parse_with_cwd(raw, "raw/trace.log", Some("/work"));
        assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
        Run {
            metadata: serde_json::from_value(serde_json::json!({
                "schema_version":3,"tool_version":"test","id":"test","name":"test",
                "command":["tool"],"cwd":"/work","started":0,"ended":1,
                "backend":"fixture","state":"completed","exit_code":0,"signal":null,
                "warnings":[],"parsed_events":parsed.events.len(),"raw_lines":parsed.lines,
                "environment":{},"completeness":"complete_for_supported_events"
            }))
            .unwrap(),
            events: parsed.events,
            storage_integrity: crate::model::StorageIntegrity {
                status: "verified".into(),
                detail: String::new(),
            },
        }
    }
    #[test]
    fn failed_exec_is_visible_and_never_called_successful_execution() {
        let l = run("");
        let r = run("2 execve(\"/tools/unavailable\", [], 0x0) = -1 ENOENT (No such file)\n");
        let report = compare(&l, &r);
        assert_eq!(report.exit_code, 1);
        let c = report
            .changes
            .iter()
            .find(|c| c.subject.contains("/tools/unavailable"))
            .unwrap();
        assert_eq!(c.category, "Execution attempt changes");
        assert_eq!(c.right, ["ENOENT"]);
        assert_eq!(c.right_evidence[0].line, 1);
    }
    #[test]
    fn stable_input_failure_precedes_probes_and_downstream_exec() {
        let l = run(
            "1 open(\"/z/settings.ini\", O_RDONLY) = 3\n1 access(\"/a/probe\", F_OK) = -1 ENOENT (No file)\n1 execve(\"/a/child\", [], 0) = 0\n",
        );
        let r = run("2 open(\"/z/settings.ini\", O_RDONLY) = -1 EACCES (Denied)\n");
        let report = compare(&l, &r);
        assert!(report.changes[0].subject.contains("/z/settings.ini"));
    }
    #[test]
    fn mixed_probe_outcomes_do_not_outrank_a_single_success_to_failure() {
        let l = run(
            "1 open(\"/a/cache\", O_RDONLY) = -1 ENOENT (No file)\n1 open(\"/a/cache\", O_RDONLY) = 3\n1 open(\"/z/input\", O_RDONLY) = 4\n",
        );
        let r = run(
            "2 open(\"/a/cache\", O_RDONLY) = -1 ENOENT (No file)\n2 open(\"/z/input\", O_RDONLY) = -1 ENOENT (No file)\n",
        );
        let report = compare(&l, &r);
        assert_eq!(report.changes.len(), 2);
        assert!(report.changes[0].subject.contains("/z/input"));
        assert_eq!(report.changes[1].left, ["ENOENT", "success"]);
        assert!(report.changes.iter().all(|c| c.detail_group.is_none()));
    }
    #[test]
    fn failed_then_successful_exec_is_an_attempt_change_not_disappearance() {
        let l = run("1 execve(\"/tool\", [], 0) = 0\n");
        let r = run(
            "2 execve(\"/tool\", [], 0) = -1 EACCES (Denied)\n2 execve(\"/tool\", [], 0) = 0\n",
        );
        let report = compare(&l, &r);
        assert_eq!(report.changes[0].category, "Execution attempt changes");
        assert_eq!(report.changes[0].right, ["EACCES", "success"]);
    }
    #[test]
    fn resumed_and_unsplit_execution_have_no_semantic_difference() {
        let l = run(
            "1 execve(\"/tool\", [], 0 <unfinished ...>\n1 <... execve resumed>)             = 0\n",
        );
        let r = run("99 execve(\"/tool\", [], 0) = 0\n");
        assert_eq!(compare(&l, &r).observed_differences, Some(false));
    }
}

#[cfg(test)]
mod presentation_tests {
    use super::*;
    #[test]
    fn temporary_candidates_are_narrow_not_path_equivalence() {
        for path in [
            "/work/rustcAb12Z9/symbols.o",
            "/work/.tmp123abc",
            "/tmp/ccAb1234.s",
            "/repo/.git/objects/ab/tmp_obj_a12345",
        ] {
            assert!(temporary_candidate(path), "{path}");
        }
        for path in [
            "/tmp/important.conf",
            "/usr/bin/rustc",
            "/work/rustc-source/a",
            "/work/tmp_obj_a12345",
            "/tmp/ccX.s",
        ] {
            assert!(!temporary_candidate(path), "{path}");
        }
    }
}
