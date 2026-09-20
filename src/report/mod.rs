use crate::diff::Report;
pub fn text(r: &Report) -> String {
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
    for (index, c) in r.changes.iter().enumerate() {
        // Always show the first five ranked entries. Fold only subsequent candidates.
        if index >= 5
            && let Some(group) = &c.detail_group
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
            "  {} changes folded; all paths, outcomes and evidence remain in --json changes (first example ranks, 1-based): {:?}",
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

pub fn show(r: &crate::model::Run) -> String {
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
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["changes"].as_array().unwrap().len(), 13);
        for c in &report.changes[1..] {
            assert_eq!(c.right_evidence.len(), 1);
            assert!(c.detail_group.is_some());
        }
    }
    #[test]
    fn temporary_path_failure_is_not_folded_and_errors_stay_distinct() {
        let l = run("1 open(\"/tmp/rustcABC123/key\", O_RDONLY) = 3\n");
        let r = run("2 open(\"/tmp/rustcABC123/key\", O_RDONLY) = -1 EACCES (Denied)\n");
        let report = compare(&l, &r);
        assert!(report.changes[0].detail_group.is_none());
        assert!(render(&report).contains("success → EACCES"));
    }
}
