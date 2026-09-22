//! Pure presentation adapters for already-computed domain results.
//!
//! This module performs no storage access and makes no reliability decisions.
//! Its diff JSON structs deliberately reproduce the frozen v4 wire view while
//! keeping internal ranking and display-only grouping out of that schema.

use crate::diff::{Change, RankClass, Reliability, Report};
use crate::model::{RecordResult, RecordSummary};
use serde::Serialize;

const UNSUCCESSFUL_FILE_GROUP: &str =
    "One-sided unsuccessful file accesses (not necessarily faults)";
const TEMPORARY_PATH_GROUP: &str = "Temporary-looking file paths (heuristic, not ignored)";

struct PresentedChange<'a> {
    change: &'a Change,
    detail_group: Option<&'static str>,
}

// Narrow spelling heuristic for display grouping only. It never changes
// comparison facts, reliability, the ordinary conclusion, or the exit code.
fn temporary_candidate(path: &str) -> bool {
    fn suffix(s: &str, prefix: &str) -> bool {
        s.strip_prefix(prefix)
            .is_some_and(|s| s.len() == 6 && s.bytes().all(|b| b.is_ascii_alphanumeric()))
    }
    let mut leading_components = path.split('/');
    let is_temporary_root = matches!(
        (leading_components.next(), leading_components.next()),
        (Some(""), Some("tmp"))
    );
    path.split('/')
        .any(|part| suffix(part, "rustc") || suffix(part, "rmeta") || suffix(part, ".tmp"))
        || (path.contains("/objects/")
            && path
                .rsplit('/')
                .next()
                .is_some_and(|p| suffix(p, "tmp_obj_")))
        || (is_temporary_root
            && path
                .strip_suffix(".s")
                .and_then(|s| s.rsplit('/').next())
                .is_some_and(|s| suffix(s, "cc")))
}

fn fold_group(change: &Change) -> Option<&'static str> {
    let path = change.file_path.as_deref()?;
    if !change.left.is_empty() && !change.right.is_empty() {
        return None;
    }
    if change
        .left
        .iter()
        .chain(&change.right)
        .all(|outcome| outcome != "success")
    {
        Some(UNSUCCESSFUL_FILE_GROUP)
    } else if temporary_candidate(path) {
        Some(TEMPORARY_PATH_GROUP)
    } else {
        None
    }
}

fn compatibility_priority(change: &Change, detail_group: Option<&str>) -> u8 {
    if detail_group.is_some() {
        return 10;
    }
    match change.rank_key.class() {
        RankClass::ExitStatus => 0,
        RankClass::CaptureCompleteness => 1,
        RankClass::InvocationContext => 2,
        RankClass::SuccessToFailure => 3,
        RankClass::FailedExecutionAttempt => 4,
        RankClass::NewExecutable => 5,
        RankClass::NewFileAccess => 6,
        RankClass::RemovedExecutable => 7,
        RankClass::OutcomeSetChange => 8,
        RankClass::OtherObservation => 9,
    }
}

fn presented_changes(report: &Report) -> Vec<PresentedChange<'_>> {
    let mut changes = report
        .changes
        .iter()
        .map(|change| PresentedChange {
            change,
            detail_group: fold_group(change),
        })
        .collect::<Vec<_>>();
    changes.sort_by_key(|presented| {
        (
            compatibility_priority(presented.change, presented.detail_group),
            presented.change.rank_key.comparison_order(),
        )
    });
    changes
}

#[derive(Serialize)]
struct ReportCompatibility<'a> {
    schema_version: u32,
    observed_differences: Option<bool>,
    reliability: &'a Reliability,
    exit_code: i32,
    errors: &'a [String],
    left: &'a str,
    right: &'a str,
    left_id: &'a str,
    right_id: &'a str,
    changes: Vec<ChangeCompatibility<'a>>,
    warnings: &'a [String],
}

#[derive(Serialize)]
struct ChangeCompatibility<'a> {
    category: &'a str,
    subject: &'a str,
    detail_group: Option<&'static str>,
    left: &'a [String],
    right: &'a [String],
    left_evidence: &'a [crate::model::Evidence],
    right_evidence: &'a [crate::model::Evidence],
}

/// Render the frozen diff v4 JSON compatibility view from one computed report.
pub(crate) fn json(report: &Report) -> serde_json::Result<String> {
    let changes = presented_changes(report)
        .into_iter()
        .map(|presented| ChangeCompatibility {
            category: &presented.change.category,
            subject: &presented.change.subject,
            detail_group: presented.detail_group,
            left: &presented.change.left,
            right: &presented.change.right,
            left_evidence: &presented.change.left_evidence,
            right_evidence: &presented.change.right_evidence,
        })
        .collect();
    serde_json::to_string_pretty(&ReportCompatibility {
        schema_version: report.schema_version,
        observed_differences: report.observed_differences,
        reliability: &report.reliability,
        exit_code: report.exit_code,
        errors: &report.errors,
        left: &report.left,
        right: &report.right,
        left_id: &report.left_id,
        right_id: &report.right_id,
        changes,
        warnings: &report.warnings,
    })
}

/// Render the established tab-separated record-index view from typed facts.
pub(crate) fn list_text(records: &[RecordSummary]) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    for record in records {
        let (id, name, directory, completeness, integrity) = match record {
            RecordSummary::Readable {
                metadata,
                directory,
                storage_integrity,
            } => (
                Some(metadata.id.as_str()),
                Some(metadata.name.as_str()),
                directory.as_str(),
                metadata.completeness.as_deref(),
                storage_integrity,
            ),
            RecordSummary::Unreadable {
                id,
                name,
                directory,
                storage_integrity,
            } => (
                id.as_deref(),
                name.as_deref(),
                directory.as_str(),
                None,
                storage_integrity,
            ),
        };
        writeln!(
            &mut out,
            "{}\t{}\t{}\tcapture={}\tstorage={}\t{}",
            id.unwrap_or("(no safe ID)"),
            name.unwrap_or("(unknown label)"),
            directory,
            completeness.unwrap_or("unknown"),
            integrity.status,
            integrity.detail,
        )
        .unwrap();
    }
    out
}

/// Render the established flat list JSON without making it a storage schema.
pub(crate) fn list_json(records: &[RecordSummary]) -> serde_json::Result<String> {
    let compatible = records
        .iter()
        .map(list_compatibility_value)
        .collect::<serde_json::Result<Vec<_>>>()?;
    serde_json::to_string_pretty(&compatible)
}

fn list_compatibility_value(record: &RecordSummary) -> serde_json::Result<serde_json::Value> {
    let (mut value, directory, integrity) = match record {
        RecordSummary::Readable {
            metadata,
            directory,
            storage_integrity,
        } => (
            serde_json::to_value(metadata.as_ref())?,
            directory,
            storage_integrity,
        ),
        RecordSummary::Unreadable {
            id,
            name,
            directory,
            storage_integrity,
        } => (
            serde_json::json!({"id": id, "name": name}),
            directory,
            storage_integrity,
        ),
    };
    value["directory"] = directory.as_str().into();
    value["storage_integrity"] = serde_json::to_value(integrity)?;
    Ok(value)
}

/// Render the established record summary for the process stderr boundary.
pub(crate) fn record(result: &RecordResult) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    writeln!(
        &mut out,
        "Recorded {:?}: {} ({}, {} events, {} warnings)\nEvidence: {}",
        result.name,
        result.id,
        result.state,
        result.event_count,
        result.warnings.len(),
        result.evidence_dir
    )
    .unwrap();
    for warning in &result.warnings {
        writeln!(&mut out, "warning: {warning}").unwrap();
    }
    out
}

/// Render the frozen human-readable diff view without recomputing its status.
pub(crate) fn text(r: &Report) -> String {
    render(r)
}
fn render(r: &Report) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    macro_rules! println {
        ($($arg:tt)*) => { writeln!(&mut out, $($arg)*).unwrap() };
    }

    println!("Run comparison: {} → {}", r.left, r.right);
    println!(
        "Observed differences: {}",
        match r.observed_differences {
            Some(true) => "yes",
            Some(false) => "no",
            None => "not evaluated",
        }
    );
    println!(
        "Capture completeness: left={}, right={}",
        r.reliability.left.capture, r.reliability.right.capture
    );
    println!(
        "Storage integrity: left={}, right={}",
        r.reliability.left.storage, r.reliability.right.storage
    );
    println!("Exit code: {}", r.exit_code);
    if r.exit_code == 3 {
        println!(
            "RELIABILITY NOT ESTABLISHED: comparison cannot establish a complete, verified result."
        );
    }
    for error in &r.errors {
        println!("ERROR: {error}");
    }
    if r.observed_differences == Some(false) {
        println!("No differences in supported, aggregated observations.");
    }
    let mut folded = std::collections::BTreeMap::<&str, Vec<usize>>::new();
    for (index, presented) in presented_changes(r).into_iter().enumerate() {
        let c = presented.change;
        // Always show the first five ranked entries. Fold only subsequent candidates.
        if index >= 5
            && let Some(group) = presented.detail_group
        {
            folded.entry(group).or_default().push(index + 1);
            continue;
        }
        println!("\n{}", c.category);
        if !c.subject.is_empty() {
            println!("  {}", c.subject);
        }
        println!(
            "  {} → {}",
            if c.left.is_empty() {
                "(not observed)".into()
            } else {
                c.left.join(", ")
            },
            if c.right.is_empty() {
                "(not observed)".into()
            } else {
                c.right.join(", ")
            }
        );
        for (id, evs) in [
            (&r.left_id, &c.left_evidence),
            (&r.right_id, &c.right_evidence),
        ] {
            for e in evs {
                println!("  evidence: runs/{id}/{}:{}-{}", e.file, e.line, e.end_line);
            }
        }
    }
    for (group, indices) in folded {
        println!("\nDetail group: {group}");
        println!(
            "  {} changes folded; all changes, paths and outcome sets remain in --json, with evidence samples capped per side (first example ranks, 1-based): {:?}",
            indices.len(),
            &indices[..indices.len().min(3)]
        );
    }
    println!(
        "\nTotal observed changes: {} (including folded details)",
        r.changes.len()
    );
    for w in &r.warnings {
        println!("warning: {w}");
    }
    println!("\nObserved changes are investigation clues, not proof of a root cause.");
    out
}

/// Render one already-loaded run without reinterpreting its stored evidence.
pub(crate) fn show(r: &crate::model::Run) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    macro_rules! println {
        ($($arg:tt)*) => { writeln!(&mut out, $($arg)*).unwrap() };
    }
    let m = &r.metadata;
    println!(
        "Storage integrity: {} — {}",
        r.storage_integrity.status, r.storage_integrity.detail
    );
    println!(
        "{} ({})\nState: {} / {}\nCommand: {:?}\nInitial cwd: {}\nExit: {:?}; signal: {:?}\nEvents: {}",
        m.name,
        m.id,
        m.state,
        m.completeness.as_deref().unwrap_or("legacy/unknown"),
        m.command,
        m.cwd,
        m.exit_code,
        m.signal,
        r.events.len()
    );
    if let Some(env) = &m.environment {
        for (key, value) in env {
            println!("env {key}: {}", value.as_deref().unwrap_or("(unset)"));
        }
    }
    println!(
        "Path semantics: absolute spellings are recorded syscall/strace observations, not physical identity guarantees; relative paths stay uncertain. Older records may contain historical CWD-derived spellings; consult raw evidence."
    );
    for e in &r.events {
        if e.kind == "file" || e.kind == "exec" || e.kind == "cwd" {
            println!(
                "  Last observed CWD (context only, not a current physical path): {}",
                e.cwd_before.as_deref().unwrap_or("unknown")
            );
            println!(
                "  Path context: {}",
                if e.path.is_none() {
                    "unresolved"
                } else if e.context_uncertain
                    || e.path.as_ref().is_some_and(|p| p.starts_with("relative:"))
                {
                    "uncertain / relative observation"
                } else {
                    "recorded absolute spelling; verify syscall evidence"
                }
            );
        }
        println!(
            "{} {} {} => {} [runs/{}/{}:{}-{}]",
            e.pid,
            e.operation,
            e.path.as_deref().unwrap_or(""),
            e.outcome,
            m.id,
            e.evidence.file,
            e.evidence.line,
            e.evidence.end_line
        );
    }
    for e in &r.events {
        if let Some(argv) = &e.argv {
            println!("exec arguments (PID {}): {:?}", e.pid, argv);
        }
    }
    for w in &m.warnings {
        println!("warning: {w}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{compare, field_regressions::run};
    use crate::model::{CaptureCompleteness, CaptureState, StorageIntegrity};

    #[test]
    fn typed_list_renderers_preserve_the_flat_compatibility_view() {
        let loaded = run("");
        let rows = vec![
            RecordSummary::Readable {
                metadata: Box::new(loaded.metadata),
                directory: "runs/test".into(),
                storage_integrity: loaded.storage_integrity,
            },
            RecordSummary::Unreadable {
                id: None,
                name: None,
                directory: "runs/unsafe".into(),
                storage_integrity: StorageIntegrity {
                    status: "unavailable".into(),
                    detail: "unsafe leaf".into(),
                },
            },
        ];

        assert_eq!(
            list_text(&rows),
            "test\ttest\truns/test\tcapture=complete_for_supported_events\tstorage=verified\t\n(no safe ID)\t(unknown label)\truns/unsafe\tcapture=unknown\tstorage=unavailable\tunsafe leaf\n"
        );
        let json = list_json(&rows).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value[0]["id"], "test");
        assert_eq!(value[0]["directory"], "runs/test");
        assert!(value[0].get("metadata").is_none());
        assert_eq!(
            &json[json
                .find("  {\n    \"directory\": \"runs/unsafe\"")
                .unwrap()..],
            "  {\n    \"directory\": \"runs/unsafe\",\n    \"id\": null,\n    \"name\": null,\n    \"storage_integrity\": {\n      \"detail\": \"unsafe leaf\",\n      \"status\": \"unavailable\"\n    }\n  }\n]"
        );
    }

    #[test]
    fn record_summary_matches_the_existing_stderr_bytes() {
        let result = RecordResult {
            id: "abc-123".into(),
            name: "quoted \"record\"".into(),
            state: CaptureState::Completed,
            completeness: CaptureCompleteness::Partial,
            event_count: 7,
            warnings: vec!["first warning".into(), "second warning".into()],
            evidence_dir: "/var/lib/rundelta/runs/abc-123".into(),
            cli_exit: 0,
        };

        assert_eq!(
            record(&result).as_bytes(),
            b"Recorded \"quoted \\\"record\\\"\": abc-123 (completed, 7 events, 2 warnings)\nEvidence: /var/lib/rundelta/runs/abc-123\nwarning: first warning\nwarning: second warning\n"
        );
    }

    #[test]
    fn folded_details_preserve_json_truth_evidence_and_reliability() {
        let l = run("");
        let mut raw = String::new();
        for i in 0..12 {
            raw.push_str(&format!(
                "1 access(\"/search/{i}\", F_OK) = -1 ENOENT (No file)\n"
            ));
        }
        let mut right = run(&raw);
        right.metadata.completeness = Some("partial".into());
        right.metadata.warnings.push("unknown event".into());
        let report = compare(&l, &right);
        assert_eq!(report.exit_code, 3);
        assert_eq!(report.observed_differences, Some(true));
        assert_eq!(report.changes.len(), 13);
        let text = render(&report);
        assert!(text.contains("RELIABILITY NOT ESTABLISHED"));
        assert!(text.contains("8 changes folded"));
        assert!(text.contains("Total observed changes: 13"));
        let json: serde_json::Value = serde_json::from_str(&json(&report).unwrap()).unwrap();
        assert_eq!(json["changes"].as_array().unwrap().len(), 13);
        for c in &report.changes[1..] {
            assert_eq!(c.right_evidence.len(), 1);
            assert_eq!(fold_group(c), Some(UNSUCCESSFUL_FILE_GROUP));
        }
    }
    #[test]
    fn temporary_path_failure_is_not_folded_and_errors_stay_distinct() {
        let l = run("1 open(\"/tmp/rustcABC123/key\", O_RDONLY) = 3\n");
        let r = run("2 open(\"/tmp/rustcABC123/key\", O_RDONLY) = -1 EACCES (Denied)\n");
        let report = compare(&l, &r);
        assert_eq!(fold_group(&report.changes[0]), None);
        assert!(render(&report).contains("success → EACCES"));
    }

    #[test]
    fn temporary_candidates_are_narrow_display_hints_not_path_equivalence() {
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

    #[test]
    fn compatibility_adapter_preserves_legacy_ranking_and_json_detail_groups() {
        let left = run("");
        let right = run(concat!(
            "1 open(\"/tmp/ccAb1234.s\", O_RDONLY) = 3\n",
            "1 open(\"/z/ordinary-input\", O_RDONLY) = 3\n",
        ));
        let report = compare(&left, &right);
        let presented = presented_changes(&report);
        assert!(presented[0].change.subject.contains("/z/ordinary-input"));
        assert_eq!(presented[0].detail_group, None);
        assert!(presented[1].change.subject.contains("/tmp/ccAb1234.s"));
        assert_eq!(presented[1].detail_group, Some(TEMPORARY_PATH_GROUP));

        let text = render(&report);
        assert!(text.find("/z/ordinary-input").unwrap() < text.find("/tmp/ccAb1234.s").unwrap());
        let value: serde_json::Value = serde_json::from_str(&json(&report).unwrap()).unwrap();
        assert!(
            value["changes"][0]["subject"]
                .as_str()
                .unwrap()
                .contains("/z/ordinary-input")
        );
        assert_eq!(value["changes"][1]["detail_group"], TEMPORARY_PATH_GROUP);
        assert!(value["changes"][0].get("rank_key").is_none());
        assert!(value["changes"][0].get("file_path").is_none());
        assert_eq!(report.exit_code, 1);
        assert_eq!(value["exit_code"], report.exit_code);
        assert_eq!(
            value["observed_differences"],
            serde_json::json!(report.observed_differences)
        );
        assert_eq!(report.observed_differences, Some(true));
    }
}
