//! Synthetic stored-contract regressions through the compiled process boundary.
//!
//! Checksums make fixture bytes internally consistent, not authentic capture
//! evidence. The CLI must still reject contradictory completeness claims.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.0) {
            eprintln!("could not remove synthetic fixture {:?}: {error}", self.0);
        }
    }
}

fn check(bytes: &[u8]) -> Value {
    json!({"bytes": bytes.len(), "sha256": format!("{:x}", Sha256::digest(bytes))})
}

fn observation(
    pid: &str,
    kind: &str,
    operation: &str,
    path: Value,
    outcome: &str,
    line: usize,
) -> Value {
    json!({
        "schema_version": 3, "pid": pid, "kind": kind, "operation": operation,
        "path": path, "outcome": outcome,
        "evidence": {"file": "raw/trace.log", "line": line, "end_line": line},
        "argv": if kind == "exec" { json!(["tool"]) } else { Value::Null },
        "child_pid": null, "cwd_before": null, "context_uncertain": false,
        "task_generation": 1
    })
}

fn fixture(child_exit: Option<&str>, partial: bool) -> Fixture {
    let root = std::env::temp_dir().join(format!(
        "rundelta-shared-model-{}-{}",
        std::process::id(),
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).unwrap();
    let fixture = Fixture(root);
    let run = fixture.0.join("runs/abc001-101");
    fs::create_dir_all(run.join("raw")).unwrap();
    let mut spawn = observation("7", "spawn", "fork", json!("8"), "success", 2);
    spawn["child_pid"] = json!("8");
    spawn["child_generation"] = json!(1);
    let mut events = vec![
        observation("7", "exec", "execve", json!("/tool"), "success", 1),
        spawn,
    ];
    let mut raw = "7 execve(\"/tool\", [\"tool\"], []) = 0\n7 fork() = 8\n".to_owned();
    if let Some(outcome) = child_exit {
        events.push(observation("8", "exit", "exit", Value::Null, outcome, 3));
        raw.push_str(&format!("8 {outcome}\n"));
    }
    let line = events.len() + 1;
    events.push(observation(
        "7",
        "exit",
        "exit",
        Value::Null,
        "+++ exited with 0 +++",
        line,
    ));
    raw.push_str("7 +++ exited with 0 +++\n");
    let mut event_bytes = Vec::new();
    for event in &events {
        serde_json::to_writer(&mut event_bytes, event).unwrap();
        event_bytes.push(b'\n');
    }
    let metadata = json!({
        "schema_version": 4, "tool_version": "synthetic", "id": "abc001-101",
        "name": "synthetic", "command": ["tool"], "cwd": "/", "started": 1,
        "ended": 2, "backend": "synthetic fixture", "state": "completed",
        "exit_code": 0, "signal": null, "warnings": if partial {json!(["child completion not observed"])} else {json!([])},
        "parsed_events": events.len(), "raw_lines": line, "environment": {},
        "completeness": if partial {"partial"} else {"complete_for_supported_events"},
        "collector_exit_code": 0, "collector_signal": null,
        "target_exit_raw": "+++ exited with 0 +++", "evidence_contract": "trace-v1",
        "target_provenance": {
            "schema_version": 1, "selection_rule": "first_successful_exec_evidence_order_v1",
            "identity_contract": "task_generations_evidence_order_v1",
            "root": {"pid": "7", "generation": 1},
            "root_exec": {"index": 0, "evidence": events[0]["evidence"]},
            "target_exit": {"index": events.len()-1, "evidence": events.last().unwrap()["evidence"]}
        }
    });
    let raw_check = check(raw.as_bytes());
    let manifest = json!({
        "schema_version": 1, "run_id": "abc001-101", "file": "raw/trace.log",
        "receipt": {"status": "parsed", "bytes": raw_check["bytes"],
                    "logical_lines": line, "sha256": raw_check["sha256"]}
    });
    let metadata_bytes = serde_json::to_vec(&metadata).unwrap();
    let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
    fs::write(run.join("raw/trace.log"), raw).unwrap();
    fs::write(run.join("metadata.json"), &metadata_bytes).unwrap();
    fs::write(run.join("events.jsonl"), &event_bytes).unwrap();
    fs::write(run.join("raw-integrity.json"), &manifest_bytes).unwrap();
    fs::write(
        run.join("finalized.json"),
        serde_json::to_vec(&json!({
            "schema_version": 2, "run_id": "abc001-101", "event_count": events.len(),
            "metadata": check(&metadata_bytes), "events": check(&event_bytes),
            "raw_manifest": check(&manifest_bytes)
        }))
        .unwrap(),
    )
    .unwrap();
    fixture
}

#[test]
fn saved_child_completion_controls_show_and_text_json_diff_status() {
    for (child_exit, partial, expected) in [
        (Some("+++ exited with 0 +++"), false, 0),
        (Some("+++ killed by SIGTERM +++"), false, 0),
        (None, false, 2),
        (Some("success"), false, 2),
        (None, true, 3),
    ] {
        let fixture = fixture(child_exit, partial);
        let run_cli = |arguments: &[&str]| {
            Command::new(env!("CARGO_BIN_EXE_rundelta"))
                .arg("--storage")
                .arg(&fixture.0)
                .args(arguments)
                .output()
                .unwrap()
        };
        let show = run_cli(&["show", "synthetic", "--json"]);
        assert_eq!(
            show.status.code(),
            Some(if expected == 2 { 2 } else { 0 }),
            "{child_exit:?}, partial={partial}: {}",
            String::from_utf8_lossy(&show.stderr)
        );
        let text = run_cli(&["diff", "synthetic", "synthetic"]);
        let json_output = run_cli(&["diff", "synthetic", "synthetic", "--json"]);
        assert_eq!(text.status.code(), Some(expected));
        assert_eq!(json_output.status.code(), Some(expected));
        let report: Value = serde_json::from_slice(&json_output.stdout).unwrap();
        assert_eq!(report["exit_code"], json!(expected));
        assert_eq!(
            report["observed_differences"],
            if expected == 2 {
                Value::Null
            } else {
                json!(false)
            }
        );
    }
}

#[test]
fn markerless_provenance_shapes_fail_closed_without_loading_events() {
    type Mutation = fn(&mut Value);
    let mutations: [(&str, Mutation); 14] = [
        ("accepted", |_| {}),
        ("accepted resumed order", |p| {
            p["root_exec"]["index"] = json!(3);
            p["target_exit"]["index"] = json!(0);
        }),
        ("same role reference", |p| {
            p["target_exit"] = p["root_exec"].clone()
        }),
        ("exit before exec", |p| {
            p["root_exec"]["evidence"]["line"] = json!(4);
            p["root_exec"]["evidence"]["end_line"] = json!(4);
            p["target_exit"]["evidence"]["line"] = json!(1);
            p["target_exit"]["evidence"]["end_line"] = json!(1);
        }),
        ("unknown schema", |p| p["schema_version"] = json!(999)),
        ("unknown selection", |p| {
            p["selection_rule"] = json!("unknown")
        }),
        ("unknown identity", |p| {
            p["identity_contract"] = json!("unknown")
        }),
        ("invalid PID", |p| p["root"]["pid"] = json!("not-a-pid")),
        ("zero generation", |p| p["root"]["generation"] = json!(0)),
        ("zero line", |p| {
            p["target_exit"]["evidence"]["line"] = json!(0)
        }),
        ("outside file", |p| {
            p["root_exec"]["evidence"]["file"] = json!("../outside")
        }),
        ("past raw lines", |p| {
            p["target_exit"]["evidence"]["end_line"] = json!(999)
        }),
        ("reversed lines", |p| {
            p["target_exit"]["evidence"]["line"] = json!(999)
        }),
        ("past saved count", |p| p["root_exec"]["index"] = json!(999)),
    ];
    for (name, mutate) in mutations {
        let fixture = fixture(Some("+++ exited with 0 +++"), false);
        let run = fixture.0.join("runs/abc001-101");
        let mut metadata: Value =
            serde_json::from_slice(&fs::read(run.join("metadata.json")).unwrap()).unwrap();
        mutate(&mut metadata["target_provenance"]);
        fs::write(
            run.join("metadata.json"),
            serde_json::to_vec(&metadata).unwrap(),
        )
        .unwrap();
        fs::remove_file(run.join("finalized.json")).unwrap();
        let expected = if name.starts_with("accepted") { 3 } else { 2 };
        for version in [None, Some("5"), Some("6")] {
            for json_output in [false, true] {
                let mut command = Command::new(env!("CARGO_BIN_EXE_rundelta"));
                command
                    .arg("--storage")
                    .arg(&fixture.0)
                    .args(["diff", "synthetic", "synthetic"]);
                if let Some(version) = version {
                    command.args(["--report-version", version]);
                }
                if json_output {
                    command.arg("--json");
                }
                let output = command.output().unwrap();
                assert_eq!(
                    output.status.code(),
                    Some(expected),
                    "{name}, {version:?}, json={json_output}"
                );
                if json_output {
                    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
                    assert_eq!(report["exit_code"], json!(expected));
                    assert_eq!(
                        report["observed_differences"],
                        if expected == 2 {
                            Value::Null
                        } else {
                            json!(false)
                        }
                    );
                    assert_eq!(report["changes"], json!([]));
                    assert_eq!(
                        report["errors"].as_array().unwrap().is_empty(),
                        expected == 3
                    );
                }
            }
        }
    }
}
