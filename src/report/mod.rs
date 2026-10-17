//! Pure presentation adapters for already-computed domain results.
//!
//! This module performs no storage access and makes no reliability decisions.
//! Its diff JSON structs preserve the v4/v5 views and explicitly include the
//! computed v6 evidence mapping when requested. Internal rank keys and structured
//! path helpers are not wire fields. Grouping influences presentation order and
//! text folding only; it does not remove JSON observations, decide reliability,
//! or change the computed status.
//!
//! Renderers borrow facts, never repair or reinterpret them. Diff, show and list
//! JSON write to a caller-owned byte sink without retaining the complete encoded
//! document. List retains only one flat compatibility row during serialization.
//! Text rendering still owns a `String`; allocation failure is not converted
//! into a presentation error here. Stream selection, buffering, trailing newline
//! and write/flush error policy belong to the process boundary.

use crate::diff::{
    Change, ChangeEvidenceMapping, EvidenceSource, RankClass, Reliability, Report,
    TargetProvenanceSides,
};
use crate::model::{RecordResult, RecordSummary};
use serde::Serialize;

const UNSUCCESSFUL_FILE_GROUP: &str =
    "One-sided unsuccessful file accesses (not necessarily faults)";
const TEMPORARY_PATH_GROUP: &str = "Temporary-looking file paths (heuristic, not ignored)";

/// A borrowed change with a presentation hint, leaving the semantic result intact.
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
    // 只折叠单侧 file 观察。双侧成功变失败必须可见，不能因临时路径的外观
    // 被降为噪声；失败的单侧探测也不等于故障，只给出低优先级展示分组。
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

/// Preserve frozen display tiers, using semantic rank except for detail groups.
///
/// This priority belongs to the compatibility view. It must not be fed back into
/// `diff::Report` as evidence quality, a new comparison conclusion, or an exit code.
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

/// Build one deterministic borrowed view shared by JSON and text rendering.
///
/// The original comparison index breaks ties after display grouping changes a
/// tier. Sharing this view keeps both formats aligned while leaving the complete
/// report's semantic ordering and number of changes untouched.
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

/// Explicit versioned field selection, avoiding accidental export of internal data.
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
    #[serde(skip_serializing_if = "Option::is_none")]
    target_provenance: Option<&'a TargetProvenanceSides>,
    #[serde(skip_serializing_if = "Option::is_none")]
    evidence_contract: Option<&'static str>,
}

/// Frozen change fields; `detail_group` is a hint, not a suppressed JSON change.
#[derive(Serialize)]
struct ChangeCompatibility<'a> {
    category: &'a str,
    subject: &'a str,
    detail_group: Option<&'static str>,
    left: &'a [String],
    right: &'a [String],
    left_evidence: &'a [crate::model::Evidence],
    right_evidence: &'a [crate::model::Evidence],
    #[serde(skip_serializing_if = "Option::is_none")]
    evidence_mapping: Option<&'a ChangeEvidenceMapping>,
}

/// Select borrowed wire fields and the established presentation order once.
fn compatibility_view(report: &Report) -> ReportCompatibility<'_> {
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
            evidence_mapping: presented.change.evidence_mapping.as_ref(),
        })
        .collect();
    ReportCompatibility {
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
        target_provenance: report.target_provenance.as_ref(),
        evidence_contract: report.evidence_contract,
    }
}

/// Encode every selected diff field without retaining the full JSON output.
///
/// The caller owns the sink and its buffering/error policy. This renderer adds
/// no trailing newline and does not flush. Samples, full references, reliability
/// and status come from the computed report; serialization errors propagate.
pub(crate) fn write_json(report: &Report, out: &mut impl std::io::Write) -> serde_json::Result<()> {
    serde_json::to_writer_pretty(out, &compatibility_view(report))
}

/// Encode the original loaded Run view without retaining a full JSON document.
///
/// The caller owns the sink and buffering/error policy. This pure renderer keeps
/// the Run's serialized fields, adds no trailing newline, and does not flush.
/// Storage integrity and capture facts are presented as already loaded.
pub(crate) fn write_show_json(
    run: &crate::model::Run,
    out: &mut impl std::io::Write,
) -> serde_json::Result<()> {
    serde_json::to_writer_pretty(out, run)
}

/// Retain the owned compatibility oracle only for colocated wire assertions.
#[cfg(test)]
pub(crate) fn json(report: &Report) -> serde_json::Result<String> {
    serde_json::to_string_pretty(&compatibility_view(report))
}

/// Present already-classified deletion slots without deciding recovery safety.
///
/// Storage supplies the deterministic order and state. This adapter neither
/// opens a ticket nor turns `no_record` into a successful deletion assertion.
pub(crate) fn deleted_text(records: &[crate::store::deletion::DeletedRecord]) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    for (index, row) in records.iter().enumerate() {
        if index != 0 {
            out.push('\n');
        }
        write!(
            &mut out,
            "{} {} {}",
            row.ticket,
            row.id.as_deref().unwrap_or("?"),
            row.state
        )
        .expect("formatting strings into a String cannot return an error");
        if let Some(error) = &row.error {
            out.push(' ');
            out.push_str(error);
        }
    }
    out
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
                ..
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
        write!(
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
        if let RecordSummary::Readable {
            raw_integrity: Some(raw),
            ..
        } = record
        {
            // New component facts are additive; legacy index rows stay byte-stable.
            write!(&mut out, "\traw={}\t{}", raw.status, raw.detail).unwrap();
        }
        writeln!(&mut out).unwrap();
    }
    out
}

/// Encode the flat list contract in stored order, keeping one owned row at a time.
///
/// A row still uses the established Value conversion: its lexical field order,
/// flattened metadata and omission rules must stay byte-compatible. The adapter
/// borrows the complete index and drops each row after `serialize_element`; it
/// does not retain a second whole-list container or an encoded document. The
/// caller owns buffering, final newline, flush and output-error classification.
pub(crate) fn write_list_json(
    records: &[RecordSummary],
    out: &mut impl std::io::Write,
) -> serde_json::Result<()> {
    serde_json::to_writer_pretty(out, &ListCompatibility { records })
}

/// Preserve the original flat-row conversion while bounding temporary ownership.
struct ListCompatibility<'a> {
    records: &'a [RecordSummary],
}

impl Serialize for ListCompatibility<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{Error, SerializeSeq};
        let mut sequence = serializer.serialize_seq(Some(self.records.len()))?;
        for record in self.records {
            let row = list_compatibility_value(record).map_err(S::Error::custom)?;
            sequence.serialize_element(&row)?;
        }
        sequence.end()
    }
}

/// Keep the executed whole-list compatibility oracle only for wire regressions.
#[cfg(test)]
fn list_json(records: &[RecordSummary]) -> serde_json::Result<String> {
    let compatible = records
        .iter()
        .map(list_compatibility_value)
        .collect::<serde_json::Result<Vec<_>>>()?;
    serde_json::to_string_pretty(&compatible)
}

fn list_compatibility_value(record: &RecordSummary) -> serde_json::Result<serde_json::Value> {
    // The flat compatibility shape is deliberate. A damaged row has only safe
    // hints, not fabricated metadata defaults that could look like a healthy run.
    let (mut value, directory, integrity) = match record {
        RecordSummary::Readable {
            metadata,
            directory,
            storage_integrity,
            ..
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
    if let RecordSummary::Readable {
        raw_integrity: Some(raw),
        ..
    } = record
    {
        value["raw_integrity"] = serde_json::to_value(raw)?;
    }
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
    if let (Some(left), Some(right)) = (&r.reliability.left.raw, &r.reliability.right.raw) {
        println!("Raw evidence integrity: left={left}, right={right}");
    }
    if let (Some(left), Some(right)) = (
        &r.reliability.left.provenance,
        &r.reliability.right.provenance,
    ) {
        println!("Target provenance: left={left}, right={right}");
    }
    if let Some(contract) = r.evidence_contract {
        println!("Evidence mapping: {contract}; full applicable references in --json");
    }
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
        // Always show the first five presented entries, including detail hints.
        // Only later candidates fold; sample 1-based ranks and the full total keep
        // the reduction visible, and JSON continues to contain every change.
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
        if let Some(mapping) = &c.evidence_mapping {
            render_evidence_mapping(&mut out, mapping);
        }
    }
    for (group, indices) in folded {
        println!("\nDetail group: {group}");
        println!(
            "  {} changes folded; all changes, paths and outcome sets remain in --json, with evidence samples capped per side (first example ranks, 1-based): {:?}",
            indices.len(),
            &indices[..indices.len().min(3)]
        );
        if r.evidence_contract.is_some() {
            println!("  Folded evidence mappings and all applicable references remain in --json.");
        }
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

/// Display the comparison's counts without deriving them from flattened samples.
///
/// Metadata and withheld observations have no applicable event count. Preserving
/// `None` as N/A avoids turning lack of coordinates into an asserted zero-event
/// observation. Omitted counts describe only the small compatibility sample;
/// the JSON outcome mapping retains the complete applicable reference vectors.
fn render_evidence_mapping(out: &mut String, mapping: &ChangeEvidenceMapping) {
    use std::fmt::Write;
    writeln!(
        out,
        "  Evidence sample limit per side: {}",
        mapping.sample_limit_per_side
    )
    .unwrap();
    for (name, side) in [("left", &mapping.left), ("right", &mapping.right)] {
        let source = match side.source {
            EvidenceSource::NormalizedEvents => "normalized_events",
            EvidenceSource::TargetProvenance => "target_provenance",
            EvidenceSource::Metadata => "metadata",
            EvidenceSource::Withheld => "withheld",
        };
        let optional_count = |count: Option<usize>| {
            count.map_or_else(|| "N/A".to_owned(), |count| count.to_string())
        };
        writeln!(
            out,
            "  {name} evidence: source={source}, events={}, sampled={}, omitted_from_sample={}",
            optional_count(side.event_count),
            side.sample_count,
            optional_count(side.omitted_sample_count)
        )
        .unwrap();
        if !side.metadata_fields.is_empty() {
            writeln!(
                out,
                "    metadata fields: {}",
                side.metadata_fields.join(", ")
            )
            .unwrap();
        }
        if !side.sample_event_indices.is_empty() {
            writeln!(
                out,
                "    sampled event indices (0-based): {:?}",
                side.sample_event_indices
            )
            .unwrap();
        }
        for outcome in &side.outcomes {
            writeln!(
                out,
                "    outcome {}: {} events (all references in --json)",
                outcome.outcome, outcome.event_count
            )
            .unwrap();
        }
    }
}

/// Render one already-loaded run without reinterpreting its stored evidence.
/// Saved warnings, outcomes and coordinates are displayed as recorded, including
/// legacy path spellings. No post-run path lookup can strengthen their identity.
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
    if let Some(raw) = &r.raw_integrity {
        println!("Raw evidence integrity: {} — {}", raw.status, raw.detail);
    }
    if let Some(provenance) = &m.target_provenance {
        println!(
            "Target provenance: PID {} generation {}; exec event {}, exit event {}; rule {}",
            provenance.root.pid,
            provenance.root.generation,
            provenance.root_exec.index,
            provenance.target_exit.index,
            provenance.selection_rule
        );
    }
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
        if let Some(generation) = e.task_generation {
            println!(
                "  Trace-local generation: {generation}; spawned child generation: {:?}",
                e.child_generation
            );
        }
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

    /// Exercise every flat-row variant without changing the frozen CLI fixtures.
    fn mixed_list_contract_rows() -> Vec<RecordSummary> {
        let mut legacy = crate::diff::field_regressions::run("");
        legacy.metadata.schema_version = 1;
        legacy.metadata.name = "legacy \"label\"\nsecond line".into();
        legacy.metadata.warnings = vec!["first\\warning".into(), "second\nwarning".into()];
        legacy.storage_integrity = crate::model::StorageIntegrity {
            status: "unverified".into(),
            detail: "legacy integrity was not established".into(),
        };
        let mut pending = crate::diff::field_regressions::run("");
        pending.metadata.state = "running".into();
        pending.storage_integrity = crate::model::StorageIntegrity {
            status: "unfinalized".into(),
            detail: "finalization marker absent".into(),
        };
        let traced = crate::model::trace_contract_tests::run_fixture();
        vec![
            RecordSummary::Readable {
                metadata: Box::new(legacy.metadata),
                directory: "runs/legacy".into(),
                storage_integrity: legacy.storage_integrity,
                raw_integrity: None,
            },
            RecordSummary::Readable {
                metadata: Box::new(pending.metadata),
                directory: "runs/pending".into(),
                storage_integrity: pending.storage_integrity,
                raw_integrity: None,
            },
            RecordSummary::Readable {
                metadata: Box::new(traced.metadata),
                directory: "runs/traced".into(),
                storage_integrity: traced.storage_integrity,
                raw_integrity: traced.raw_integrity,
            },
            RecordSummary::Unreadable {
                id: Some("abc-101".into()),
                name: Some("safe \"hint\"\tvalue".into()),
                directory: "runs/damaged".into(),
                storage_integrity: crate::model::StorageIntegrity {
                    status: "corrupt".into(),
                    detail: "metadata cannot be loaded".into(),
                },
            },
            RecordSummary::Unreadable {
                id: None,
                name: None,
                directory: "runs/unavailable".into(),
                storage_integrity: crate::model::StorageIntegrity {
                    status: "unavailable".into(),
                    detail: "unsafe leaf".into(),
                },
            },
        ]
    }

    #[test]
    fn list_json_wire_contract_before_streaming() {
        use sha2::Digest;
        let json = list_json(&mixed_list_contract_rows()).unwrap();
        // Frozen from the executed whole-list renderer before its replacement.
        // This covers every field and its byte order, not merely parsed equality.
        assert_eq!(
            format!("{:x}", sha2::Sha256::digest(json.as_bytes())),
            "3feb5adaa02d453b236985a5b7f1aab28ca22c0f4e5542faf7607667ad705dc0"
        );
        assert_eq!(list_json(&[]).unwrap(), "[]");
        let rows: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(rows.as_array().unwrap().len(), 5);
        assert!(rows[0].get("raw_integrity").is_none());
        assert!(rows[1].get("raw_integrity").is_none());
        assert_eq!(rows[1]["storage_integrity"]["status"], "unfinalized");
        assert_eq!(rows[2]["raw_integrity"]["status"], "verified");
        assert!(rows[3].get("metadata").is_none());
        assert!(rows[4]["id"].is_null());
    }

    #[test]
    fn list_stream_preserves_complete_wire_rows_and_never_flushes() {
        use std::io::{self, Write};
        #[derive(Default)]
        struct ShortWrites(Vec<u8>);
        impl Write for ShortWrites {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                let count = bytes.len().min(3);
                self.0.extend_from_slice(&bytes[..count]);
                Ok(count)
            }
            fn flush(&mut self) -> io::Result<()> {
                panic!("the process boundary, not the renderer, owns flush")
            }
        }
        let rows = mixed_list_contract_rows();
        for slice in [&rows[..0], &rows[..1], &rows[..3], &rows[..]] {
            let mut out = ShortWrites::default();
            write_list_json(slice, &mut out).unwrap();
            assert_eq!(out.0, list_json(slice).unwrap().as_bytes());
            assert!(!out.0.ends_with(b"\n"));
        }
    }

    #[test]
    fn list_stream_propagates_write_failures_without_retry_or_flush() {
        use std::io::{self, Write};
        struct FailingSink {
            bytes: Vec<u8>,
            limit: usize,
            kind: io::ErrorKind,
            failed: bool,
        }
        impl Write for FailingSink {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                assert!(!self.failed, "renderer retried after the first failure");
                if self.bytes.len() == self.limit {
                    self.failed = true;
                    return Err(self.kind.into());
                }
                let count = bytes.len().min(self.limit - self.bytes.len()).min(3);
                self.bytes.extend_from_slice(&bytes[..count]);
                Ok(count)
            }
            fn flush(&mut self) -> io::Result<()> {
                panic!("failed rendering must not flush")
            }
        }
        let rows = mixed_list_contract_rows();
        let original = list_json(&rows).unwrap();
        for kind in [io::ErrorKind::BrokenPipe, io::ErrorKind::Other] {
            for limit in [0, 1, 17, original.len() / 2] {
                let mut out = FailingSink {
                    bytes: Vec::new(),
                    limit,
                    kind,
                    failed: false,
                };
                let error = write_list_json(&rows, &mut out).unwrap_err();
                assert_eq!(error.io_error_kind(), Some(kind));
                assert!(out.failed);
                assert_eq!(out.bytes, &original.as_bytes()[..limit]);
            }
        }
    }

    #[test]
    fn list_stream_propagates_flat_row_conversion_error_after_a_prefix() {
        let mut rows = mixed_list_contract_rows();
        let RecordSummary::Readable { metadata, .. } = &mut rows[1] else {
            panic!("fixture requires a readable second row");
        };
        // Value conversion cannot represent this u128 with the existing JSON
        // features. Both implementations must fail; streaming may have already
        // delivered the first row and must never claim a complete document.
        metadata.started = u128::MAX;
        assert!(list_json(&rows).is_err());
        let mut out = Vec::new();
        let error = write_list_json(&rows, &mut out).unwrap_err();
        assert!(!error.is_io());
        assert!(out.starts_with(b"[\n  {"));
        assert!(!out.ends_with(b"]"));
        let RecordSummary::Readable { metadata, .. } = &rows[1] else {
            unreachable!();
        };
        assert_eq!(metadata.started, u128::MAX);
    }

    #[test]
    fn deleted_text_preserves_all_states_order_missing_id_and_diagnostic() {
        use crate::store::deletion::DeletedRecord;
        let rows = [
            DeletedRecord {
                ticket: "aa-1".into(),
                id: Some("abc-1".into()),
                name: Some("label".into()),
                state: "isolated",
                error: None,
            },
            DeletedRecord {
                ticket: "aa-2".into(),
                id: Some("abc-2".into()),
                name: None,
                state: "purge_started",
                error: None,
            },
            DeletedRecord {
                ticket: "aa-3".into(),
                id: Some("abc-3".into()),
                name: None,
                state: "no_record",
                error: None,
            },
            DeletedRecord {
                ticket: "unsafe".into(),
                id: None,
                name: None,
                state: "damaged",
                error: Some("identity changed".into()),
            },
        ];
        assert_eq!(
            deleted_text(&rows),
            "aa-1 abc-1 isolated\naa-2 abc-2 purge_started\naa-3 abc-3 no_record\nunsafe ? damaged identity changed"
        );
        assert_eq!(deleted_text(&[]), "");
        assert_eq!(rows[2].state, "no_record");
    }

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
                raw_integrity: None,
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
            "abc001-101\ttest\truns/test\tcapture=complete_for_supported_events\tstorage=verified\t\n(no safe ID)\t(unknown label)\truns/unsafe\tcapture=unknown\tstorage=unavailable\tunsafe leaf\n"
        );
        let json = list_json(&rows).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value[0]["id"], "abc001-101");
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

#[cfg(test)]
mod trace_presentation_tests {
    use super::*;
    use crate::model::trace_contract_tests::run_fixture;

    #[test]
    fn v5_renderings_preserve_component_facts_and_saved_coordinates() {
        let run = run_fixture();
        let report = crate::diff::compare(&run, &run);
        let value: serde_json::Value = serde_json::from_str(&json(&report).unwrap()).unwrap();
        assert_eq!(value["schema_version"], 5);
        assert_eq!(value["exit_code"], report.exit_code);
        assert_eq!(
            value["target_provenance"]["left"],
            serde_json::to_value(&run.metadata.target_provenance).unwrap()
        );
        assert_eq!(value["reliability"]["left"]["raw"], "verified");
        let rendered = text(&report);
        assert!(rendered.contains("Exit code: 0"));
        assert!(rendered.contains("Raw evidence integrity: left=verified, right=verified"));
        assert!(rendered.contains("Target provenance: left=verified, right=verified"));
        assert!(show(&run).contains("generation 1; exec event"));

        let mut damaged = run.clone();
        damaged.raw_integrity.as_mut().unwrap().status = "corrupt".into();
        let report = crate::diff::compare(&damaged, &run);
        let value: serde_json::Value = serde_json::from_str(&json(&report).unwrap()).unwrap();
        assert_eq!(value["exit_code"], 2);
        assert_eq!(value["observed_differences"], serde_json::Value::Null);
        assert_eq!(value["reliability"]["left"]["storage"], "verified");
        assert_eq!(value["reliability"]["left"]["raw"], "corrupt");
        assert!(text(&report).contains("Observed differences: not evaluated"));
    }

    #[test]
    fn new_list_facts_are_additive_and_legacy_diff_fields_stay_omitted() {
        let run = run_fixture();
        let rows = [RecordSummary::Readable {
            metadata: Box::new(run.metadata.clone()),
            directory: "runs/abc001-101".into(),
            storage_integrity: run.storage_integrity.clone(),
            raw_integrity: run.raw_integrity.clone(),
        }];
        let value: serde_json::Value = serde_json::from_str(&list_json(&rows).unwrap()).unwrap();
        assert_eq!(value[0]["raw_integrity"]["status"], "verified");
        assert!(list_text(&rows).contains("\traw=verified\t"));
        let old = crate::diff::field_regressions::run("");
        let value: serde_json::Value =
            serde_json::from_str(&json(&crate::diff::compare(&old, &old)).unwrap()).unwrap();
        assert!(value.get("target_provenance").is_none());
        assert!(value["reliability"]["left"].get("raw").is_none());
        assert!(value["reliability"]["left"].get("provenance").is_none());
    }
}

#[cfg(test)]
mod outcome_presentation_tests {
    //! Rendering controls cover full mappings despite bounded/folded text views.
    use super::*;
    use crate::diff::{ReportFormat, evaluate_report};
    use crate::model::{LoadOutcome, RawReceipt, Run, trace_contract_tests::run_fixture};

    fn compare_v6(left: &Run, right: &Run) -> Report {
        evaluate_report(
            "left",
            "right",
            LoadOutcome::Loaded(left.clone()),
            LoadOutcome::Loaded(right.clone()),
            ReportFormat::OutcomeV6,
        )
    }

    fn with_repeated_failures(mut run: Run, paths: usize, repetitions: usize) -> Run {
        // Build saved DTOs only: this fixture does not claim real raw capture or
        // marker verification. Keep the selected root's exit after all accesses.
        let mut exit = run.events.pop().unwrap();
        let template = run.events[1].clone();
        for path in 0..paths {
            for _ in 0..repetitions {
                let mut event = template.clone();
                event.path = Some(format!("/search/{path}"));
                event.outcome = "ENOENT".into();
                event.evidence.line = run.events.len() + 1;
                event.evidence.end_line = event.evidence.line;
                run.events.push(event);
            }
        }
        exit.evidence.line = run.events.len() + 1;
        exit.evidence.end_line = exit.evidence.line;
        let target = &mut run.metadata.target_provenance.as_mut().unwrap().target_exit;
        target.index = run.events.len();
        target.evidence = exit.evidence.clone();
        run.events.push(exit);
        run.metadata.parsed_events = run.events.len();
        run.metadata.raw_lines = run.events.len();
        if let RawReceipt::Parsed { logical_lines, .. } = &mut run
            .raw_integrity
            .as_mut()
            .unwrap()
            .manifest
            .as_mut()
            .unwrap()
            .receipt
        {
            *logical_lines = run.events.len();
        }
        run
    }

    #[test]
    fn v6_folded_changes_keep_every_outcome_reference_in_json() {
        let left = run_fixture();
        let right = with_repeated_failures(left.clone(), 12, 4);
        let report = compare_v6(&left, &right);
        assert_eq!((report.schema_version, report.exit_code), (6, 1));
        assert_eq!(report.observed_differences, Some(true));
        assert_eq!(report.changes.len(), 12);
        let value: serde_json::Value = serde_json::from_str(&json(&report).unwrap()).unwrap();
        assert_eq!(value["evidence_contract"], "outcome_event_references_v1");
        assert_eq!(value["changes"].as_array().unwrap().len(), 12);
        for change in value["changes"].as_array().unwrap() {
            let mapping = &change["evidence_mapping"];
            assert_eq!(mapping["sample_limit_per_side"], 3);
            assert_eq!(mapping["left"]["source"], "normalized_events");
            assert_eq!(mapping["left"]["event_count"], 0);
            assert_eq!(mapping["right"]["event_count"], 4);
            assert_eq!(mapping["right"]["sample_count"], 3);
            assert_eq!(mapping["right"]["omitted_sample_count"], 1);
            assert_eq!(change["right_evidence"].as_array().unwrap().len(), 3);
            let expected = right
                .events
                .iter()
                .enumerate()
                .filter(|(_, event)| {
                    event.path.as_deref() == mapping["observation"]["path"].as_str()
                })
                .map(|(index, event)| {
                    serde_json::json!({"index": index, "evidence": event.evidence})
                })
                .collect::<Vec<_>>();
            assert_eq!(mapping["right"]["outcomes"][0]["outcome"], "ENOENT");
            assert_eq!(mapping["right"]["outcomes"][0]["event_count"], 4);
            assert_eq!(
                mapping["right"]["outcomes"][0]["events"],
                serde_json::json!(expected)
            );
        }
        let rendered = text(&report);
        assert!(rendered.contains("7 changes folded"));
        assert!(rendered.contains("events=4, sampled=3, omitted_from_sample=1"));
        assert!(
            rendered.contains(
                "Folded evidence mappings and all applicable references remain in --json."
            )
        );
        assert!(rendered.contains("Exit code: 1"));
        assert_eq!(value["exit_code"], report.exit_code);
        assert_eq!(
            report.changes.len(),
            12,
            "rendering must preserve the report"
        );
    }

    #[test]
    fn metadata_and_withheld_counts_remain_inapplicable_in_both_views() {
        let left = run_fixture();
        let mut right = left.clone();
        right.metadata.cwd = "/different".into();
        right
            .metadata
            .environment
            .as_mut()
            .unwrap()
            .insert("LANG".into(), Some("example".into()));
        let metadata = compare_v6(&left, &right);
        assert_eq!(metadata.exit_code, 1);
        let value: serde_json::Value = serde_json::from_str(&json(&metadata).unwrap()).unwrap();
        for change in value["changes"].as_array().unwrap() {
            assert!(change["evidence_mapping"]["observation"].is_null());
            for side in ["left", "right"] {
                let mapping = &change["evidence_mapping"][side];
                assert_eq!(mapping["source"], "metadata");
                assert!(mapping["event_count"].is_null());
                assert!(mapping["omitted_sample_count"].is_null());
                assert_eq!(mapping["sample_count"], 0);
                assert!(mapping["outcomes"].as_array().unwrap().is_empty());
                assert!(!mapping["metadata_fields"].as_array().unwrap().is_empty());
            }
        }
        assert!(
            text(&metadata)
                .contains("source=metadata, events=N/A, sampled=0, omitted_from_sample=N/A")
        );
        assert!(text(&metadata).contains("metadata fields: metadata.cwd"));

        let mut withheld = left.clone();
        withheld.storage_integrity.status = "unfinalized".into();
        withheld.events.clear();
        let raw = withheld.raw_integrity.as_mut().unwrap();
        raw.status = "unverified".into();
        raw.manifest = None;
        let incomplete = compare_v6(&withheld, &right);
        assert_eq!(incomplete.exit_code, 3);
        let value: serde_json::Value = serde_json::from_str(&json(&incomplete).unwrap()).unwrap();
        for change in value["changes"].as_array().unwrap() {
            let mapping = &change["evidence_mapping"]["left"];
            assert_eq!(mapping["source"], "withheld");
            assert!(mapping["event_count"].is_null());
            assert!(mapping["omitted_sample_count"].is_null());
            assert!(mapping["outcomes"].as_array().unwrap().is_empty());
            assert!(
                mapping["sample_event_indices"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        }
        assert!(
            text(&incomplete)
                .contains("source=withheld, events=N/A, sampled=0, omitted_from_sample=N/A")
        );
        assert!(text(&incomplete).contains("Exit code: 3"));
    }

    #[test]
    fn earlier_formats_omit_mapping_fields_and_v6_errors_have_no_partial_changes() {
        let new = run_fixture();
        let mut new_changed = new.clone();
        new_changed.events[1].outcome = "ENOENT".into();
        let old = crate::diff::field_regressions::run("1 open(\"/input\", O_RDONLY) = 3\n");
        let old_changed = crate::diff::field_regressions::run(
            "1 open(\"/input\", O_RDONLY) = -1 ENOENT (No file)\n",
        );
        for (left, right) in [(&old, &old_changed), (&new, &new_changed)] {
            let report = crate::diff::compare(left, right);
            assert!(!report.changes.is_empty(), "exercise change-field omission");
            let value: serde_json::Value = serde_json::from_str(&json(&report).unwrap()).unwrap();
            assert!(value.get("evidence_contract").is_none());
            assert!(!text(&report).contains("Evidence mapping:"));
            for change in value["changes"].as_array().unwrap() {
                assert!(change.get("evidence_mapping").is_none());
            }
        }
        let mut damaged = with_repeated_failures(new.clone(), 1, 4);
        damaged.raw_integrity.as_mut().unwrap().status = "corrupt".into();
        let error = compare_v6(&new, &damaged);
        assert_eq!((error.exit_code, error.observed_differences), (2, None));
        let value: serde_json::Value = serde_json::from_str(&json(&error).unwrap()).unwrap();
        assert_eq!(value["schema_version"], 6);
        assert_eq!(value["evidence_contract"], "outcome_event_references_v1");
        assert!(value["changes"].as_array().unwrap().is_empty());
        let rendered = text(&error);
        assert!(rendered.contains("Observed differences: not evaluated"));
        assert!(rendered.contains("Exit code: 2"));
        assert!(!rendered.contains("omitted_from_sample="));
    }

    #[test]
    fn rendering_uses_computed_counts_without_repairing_or_recounting() {
        let left = run_fixture();
        let right = with_repeated_failures(left.clone(), 1, 4);
        let mut report = compare_v6(&left, &right);
        // Deliberately inconsistent presentation inputs prove this layer does
        // not become a second comparison/validation policy. Production counts
        // are established by diff's contract tests before rendering begins.
        let side = &mut report.changes[0].evidence_mapping.as_mut().unwrap().right;
        side.event_count = Some(97);
        side.sample_count = 2;
        side.omitted_sample_count = Some(95);
        let rendered = text(&report);
        assert!(rendered.contains("events=97, sampled=2, omitted_from_sample=95"));
        let value: serde_json::Value = serde_json::from_str(&json(&report).unwrap()).unwrap();
        assert_eq!(
            value["changes"][0]["evidence_mapping"]["right"]["event_count"],
            97
        );
        assert_eq!(value["exit_code"], 1);
        assert_eq!(
            report.changes[0]
                .evidence_mapping
                .as_ref()
                .unwrap()
                .right
                .event_count,
            Some(97)
        );
    }
}
