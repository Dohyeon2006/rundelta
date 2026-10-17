//! Frozen black-box contracts for RF-000.
//!
//! The test owns only synthetic storage fixtures and invokes the compiled CLI.
//! It deliberately does not import product modules or reinterpret stored data.

use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

const FIXTURE_ROOT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/contract_baseline"
);
const REVIEW_BASELINE: &str = "966fe1432d3053e8e66f24d423cc8c338ca7e543";
const EXPECTED_SOURCE_HASHES: [(&str, &str); 9] = [
    (
        "src/capture/mod.rs",
        "f4e802d5b629f764d02f5505ce857a6ba15f17b42d545d7d5fe933fcd2a3472a",
    ),
    (
        "src/cli.rs",
        "3d9f4ac1d35af9e587a54ad73c9cb1a20eb872dbdc9c8ad4e4ef5da6e15d5ba0",
    ),
    (
        "src/diff/mod.rs",
        "588a9b3efb71f2992bccb61e1b8a4bb67bd2f2ddcc61d5a7be6afd7889d1d33c",
    ),
    (
        "src/main.rs",
        "51f9013edb1386d57588d2b580c8ab317406f57027b3751e14b3749d0d794a1d",
    ),
    (
        "src/model/mod.rs",
        "abcdc1943dde65eb5156b7088f2c443fb2aaf4a3e25c41c6424aaa3ffdb4cc2e",
    ),
    (
        "src/parse/fs_tests.rs",
        "936b8638bc9ba58fc92d386f52f339a8c5ab0dd71f646a91f1f5d47bc1a34e34",
    ),
    (
        "src/parse/mod.rs",
        "23df1cd30b4c443f6471be3a88bd1afe4aedb53ac6d67cfc76fe1c62bc1ed75f",
    ),
    (
        "src/report/mod.rs",
        "3872fc24d6762a3a62e2c5c6584b1a4bc698f520b7bac3f73ce1f9314b36ae38",
    ),
    (
        "src/store/mod.rs",
        "8b2f203c0c36f672d478c98a5edc27826c88f2247bf57b95835ed110e786daf4",
    ),
];

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TempStorage(PathBuf);

impl TempStorage {
    fn from_fixture() -> Self {
        let path = std::env::temp_dir().join(format!(
            "rundelta-contract-baseline-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path)
            .unwrap_or_else(|error| panic!("create isolated fixture root {path:?}: {error}"));
        copy_tree(&Path::new(FIXTURE_ROOT).join("storage"), &path);
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempStorage {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.0) {
            eprintln!(
                "warning: could not remove contract-baseline temp storage {:?}: {error}",
                self.0
            );
        }
    }
}

fn copy_tree(source: &Path, destination: &Path) {
    let mut entries = fs::read_dir(source)
        .unwrap_or_else(|error| panic!("read fixture directory {source:?}: {error}"))
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|error| panic!("enumerate fixture directory {source:?}: {error}"));
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let kind = entry
            .file_type()
            .unwrap_or_else(|error| panic!("inspect fixture entry {source_path:?}: {error}"));
        if kind.is_dir() {
            fs::create_dir(&destination_path).unwrap_or_else(|error| {
                panic!("create fixture directory {destination_path:?}: {error}")
            });
            copy_tree(&source_path, &destination_path);
        } else if kind.is_file() {
            fs::copy(&source_path, &destination_path).unwrap_or_else(|error| {
                panic!("copy fixture {source_path:?} to {destination_path:?}: {error}")
            });
        } else {
            panic!("fixture entry must be a regular file or directory: {source_path:?}");
        }
    }
}

fn run_cli(storage: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rundelta"))
        .arg("--storage")
        .arg(storage)
        .args(arguments)
        .output()
        .unwrap_or_else(|error| panic!("run rundelta with arguments {arguments:?}: {error}"))
}

fn assert_bytes(label: &str, actual: &[u8], expected: &[u8]) {
    if actual == expected {
        return;
    }
    let offset = actual
        .iter()
        .zip(expected)
        .position(|(actual, expected)| actual != expected)
        .unwrap_or(actual.len().min(expected.len()));
    panic!(
        "{label} changed at byte {offset} (expected {} bytes, got {} bytes)\n--- expected ---\n{}\n--- actual ---\n{}",
        expected.len(),
        actual.len(),
        String::from_utf8_lossy(expected),
        String::from_utf8_lossy(actual)
    );
}

fn assert_no_storage_leak(label: &str, storage: &Path, output: &Output) {
    let spelling = storage
        .to_str()
        .unwrap_or_else(|| panic!("temporary storage path is not UTF-8: {storage:?}"));
    for (stream, bytes) in [("stdout", &output.stdout), ("stderr", &output.stderr)] {
        assert!(
            !String::from_utf8_lossy(bytes).contains(spelling),
            "{label} leaked the volatile storage root into {stream}; do not hide it with broad normalization"
        );
    }
}

fn assert_cli_golden(
    storage: &Path,
    label: &str,
    arguments: &[&str],
    expected_exit: i32,
    expected_stdout: &[u8],
) -> Vec<u8> {
    let output = run_cli(storage, arguments);
    assert_eq!(
        output.status.code(),
        Some(expected_exit),
        "{label} exit changed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_bytes(&format!("{label} stderr"), &output.stderr, b"");
    assert_no_storage_leak(label, storage, &output);
    assert_bytes(&format!("{label} stdout"), &output.stdout, expected_stdout);
    output.stdout
}

fn read_json(path: &Path) -> serde_json::Value {
    let bytes =
        fs::read(path).unwrap_or_else(|error| panic!("read JSON fixture {path:?}: {error}"));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("parse JSON fixture {path:?}: {error}"))
}

fn read_single_json_line(path: &Path) -> serde_json::Value {
    let bytes =
        fs::read(path).unwrap_or_else(|error| panic!("read JSONL fixture {path:?}: {error}"));
    assert!(
        bytes.ends_with(b"\n"),
        "JSONL fixture must end in exactly one newline: {path:?}"
    );
    let body = &bytes[..bytes.len() - 1];
    assert!(
        !body.contains(&b'\n'),
        "minimal JSONL fixture must contain exactly one event: {path:?}"
    );
    serde_json::from_slice(body)
        .unwrap_or_else(|error| panic!("parse JSONL fixture {path:?}: {error}"))
}

#[test]
fn normal_list_and_show_text_and_json_match_exact_goldens() {
    let storage = TempStorage::from_fixture();
    let list_json = assert_cli_golden(
        storage.path(),
        "list --json",
        &["list", "--json"],
        0,
        include_bytes!("fixtures/contract_baseline/golden/list.json"),
    );
    serde_json::from_slice::<serde_json::Value>(&list_json)
        .expect("list JSON golden must remain valid JSON");
    assert_cli_golden(
        storage.path(),
        "list text",
        &["list"],
        0,
        include_bytes!("fixtures/contract_baseline/golden/list.txt"),
    );

    let show_json = assert_cli_golden(
        storage.path(),
        "show --json",
        &["show", "baseline-left", "--json"],
        0,
        include_bytes!("fixtures/contract_baseline/golden/show.json"),
    );
    serde_json::from_slice::<serde_json::Value>(&show_json)
        .expect("show JSON golden must remain valid JSON");
    assert_cli_golden(
        storage.path(),
        "show text",
        &["show", "baseline-left"],
        0,
        include_bytes!("fixtures/contract_baseline/golden/show.txt"),
    );
}

#[test]
fn diff_four_way_status_text_json_and_process_exit_are_identical() {
    struct Case {
        status: i32,
        left: &'static str,
        right: &'static str,
        text: &'static [u8],
        json: &'static [u8],
    }
    let cases = [
        Case {
            status: 0,
            left: "baseline-left",
            right: "baseline-left",
            text: include_bytes!("fixtures/contract_baseline/golden/diff_0.txt"),
            json: include_bytes!("fixtures/contract_baseline/golden/diff_0.json"),
        },
        Case {
            status: 1,
            left: "baseline-left",
            right: "baseline-right",
            text: include_bytes!("fixtures/contract_baseline/golden/diff_1.txt"),
            json: include_bytes!("fixtures/contract_baseline/golden/diff_1.json"),
        },
        Case {
            status: 2,
            left: "missing-record",
            right: "baseline-left",
            text: include_bytes!("fixtures/contract_baseline/golden/diff_2.txt"),
            json: include_bytes!("fixtures/contract_baseline/golden/diff_2.json"),
        },
        Case {
            status: 3,
            left: "baseline-left",
            right: "legacy-v2",
            text: include_bytes!("fixtures/contract_baseline/golden/diff_3.txt"),
            json: include_bytes!("fixtures/contract_baseline/golden/diff_3.json"),
        },
    ];

    let storage = TempStorage::from_fixture();
    for case in cases {
        let text = assert_cli_golden(
            storage.path(),
            &format!("diff status {} text", case.status),
            &["diff", case.left, case.right],
            case.status,
            case.text,
        );
        let text = String::from_utf8(text).expect("diff text must be UTF-8");
        let expected_line = format!("Exit code: {}", case.status);
        assert_eq!(
            text.lines()
                .filter(|line| line.starts_with("Exit code: "))
                .collect::<Vec<_>>(),
            [expected_line.as_str()],
            "diff text must state exactly the process status"
        );

        let json = assert_cli_golden(
            storage.path(),
            &format!("diff status {} JSON", case.status),
            &["diff", case.left, case.right, "--json"],
            case.status,
            case.json,
        );
        let report: serde_json::Value =
            serde_json::from_slice(&json).expect("diff JSON golden must remain valid JSON");
        assert_eq!(report["schema_version"], 4, "diff schema must stay at v4");
        assert_eq!(
            report["exit_code"], case.status,
            "diff JSON and process status diverged"
        );
    }
}

#[test]
fn current_and_legacy_storage_schema_fixtures_are_valid_and_minimal() {
    let runs = Path::new(FIXTURE_ROOT).join("storage/runs");
    for (id, expected_event_outcome) in [("abc001-101", "success"), ("abc002-102", "ENOENT")] {
        let run = runs.join(id);
        let metadata_bytes =
            fs::read(run.join("metadata.json")).expect("read current metadata fixture");
        let metadata: serde_json::Value =
            serde_json::from_slice(&metadata_bytes).expect("parse current metadata fixture");
        let event_bytes = fs::read(run.join("events.jsonl")).expect("read current event fixture");
        let event = read_single_json_line(&run.join("events.jsonl"));
        let marker = read_json(&run.join("finalized.json"));

        assert_eq!(metadata["schema_version"], 3, "{id}: metadata schema");
        assert_eq!(event["schema_version"], 2, "{id}: event schema");
        assert_eq!(event["outcome"], expected_event_outcome, "{id}: outcome");
        assert_eq!(marker["schema_version"], 1, "{id}: marker schema");
        assert_eq!(marker["run_id"], id, "{id}: marker identity");
        assert_eq!(marker["event_count"], 1, "{id}: marker event count");
        assert_eq!(
            marker["events"]["bytes"],
            event_bytes.len() as u64,
            "{id}: finalized event byte count"
        );
        assert_eq!(
            marker["events"]["sha256"],
            format!("{:x}", Sha256::digest(&event_bytes)),
            "{id}: finalized event digest"
        );
        assert_eq!(
            marker["metadata"]["bytes"],
            metadata_bytes.len() as u64,
            "{id}: finalized metadata byte count"
        );
        assert_eq!(
            marker["metadata"]["sha256"],
            format!("{:x}", Sha256::digest(&metadata_bytes)),
            "{id}: finalized metadata digest"
        );
        assert!(
            run.join("raw/trace.log").is_file(),
            "{id}: normalized evidence must retain its raw file"
        );
    }

    for (id, schema) in [("abc101-201", 1), ("abc102-202", 2)] {
        let run = runs.join(id);
        let metadata = read_json(&run.join("metadata.json"));
        let event = read_single_json_line(&run.join("events.jsonl"));
        assert_eq!(metadata["schema_version"], schema, "{id}: metadata schema");
        assert_eq!(event["schema_version"], schema, "{id}: event schema");
        assert!(
            !run.join("finalized.json").exists(),
            "{id}: legacy fixture must remain markerless"
        );
        assert!(
            run.join("raw/trace.log").is_file(),
            "{id}: legacy evidence must retain its raw file"
        );
    }

    let storage = TempStorage::from_fixture();
    for (name, schema) in [("legacy-v1", 1), ("legacy-v2", 2)] {
        let output = run_cli(storage.path(), &["show", name, "--json"]);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{name} must remain readable: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_no_storage_leak(name, storage.path(), &output);
        let shown: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("legacy show must emit JSON");
        assert_eq!(shown["metadata"]["schema_version"], schema);
        assert_eq!(shown["storage_integrity"]["status"], "unverified");
    }
}

#[test]
fn review_commit_manifest_and_fixture_privacy_are_frozen() {
    let manifest = read_json(&Path::new(FIXTURE_ROOT).join("baseline_manifest.json"));
    assert_eq!(manifest["review_baseline_commit"], REVIEW_BASELINE);
    assert_eq!(manifest["algorithm"], "sha256");
    let actual = manifest["source_hashes"]
        .as_object()
        .expect("source_hashes must be an object")
        .iter()
        .map(|(path, digest)| {
            (
                path.as_str(),
                digest.as_str().expect("source digest must be a string"),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let expected = EXPECTED_SOURCE_HASHES
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        actual, expected,
        "review-baseline facts changed; recompute only from git show {REVIEW_BASELINE}:<path>"
    );
    for (path, digest) in actual {
        assert_eq!(digest.len(), 64, "{path}: SHA-256 length");
        assert!(
            digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "{path}: SHA-256 must be lowercase hexadecimal"
        );
    }

    let mut pending = vec![Path::new(FIXTURE_ROOT).to_owned()];
    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(&path)
            .unwrap_or_else(|error| panic!("scan fixture privacy at {path:?}: {error}"))
        {
            let entry = entry.expect("enumerate fixture privacy entry");
            let kind = entry.file_type().expect("inspect fixture privacy entry");
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                let bytes = fs::read(entry.path()).expect("read fixture for privacy scan");
                let text = String::from_utf8_lossy(&bytes);
                for forbidden in ["/home/", "/Users/", "C:\\Users\\"] {
                    assert!(
                        !text.contains(forbidden),
                        "fixture {:?} contains a private absolute-path prefix {forbidden:?}",
                        entry.path()
                    );
                }
            } else {
                panic!(
                    "fixture tree contains a link or special file: {:?}",
                    entry.path()
                );
            }
        }
    }
}
