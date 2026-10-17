use super::{
    AtomicControl, AtomicStaging, AtomicStep, CommitFailure, CommitStage, EventFacts,
    EventFileSummary, EventReadMode, FileCheck, Finalized, PublishedFile, StorageError, all,
    atomic_replace_with_control, check, copy_anonymous_to_named, create_named_staging,
    create_staging, decode_events, decode_finalized, decode_metadata, delete, file_stat,
    finalized_integrity, finish, finish_at_with_control, load, load_outcome, lock, lock_error,
    metadata, neutral_load_failure, open, open_directory, read_back_published_events,
    read_record_at, read_run, records, required_record_file_error, storage_load_failure_kind,
    stream_event_readback, tmpfile_is_unsupported, tmpfile_link_is_unsupported, unlink_at,
    verify_staging_for_publish,
};
use crate::model::contract_tests::{completed_metadata_fixture, event_fixture, metadata_fixture};
use crate::model::{Event, LoadOutcome, Metadata, RecordSummary, StorageLoadFailureKind};
use std::{
    ffi::CString,
    fs,
    io::{Read as _, Seek as _, SeekFrom, Write as _},
    os::fd::AsRawFd,
    os::unix::{
        ffi::OsStrExt,
        fs::{PermissionsExt, symlink},
    },
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "rundelta-{label}-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn storage_error(error: &anyhow::Error) -> &StorageError {
    error
        .chain()
        .find_map(|source| source.downcast_ref::<StorageError>())
        .expect("typed storage error")
}

fn commit_failure(error: &anyhow::Error) -> &CommitFailure {
    error
        .chain()
        .find_map(|source| source.downcast_ref::<CommitFailure>())
        .expect("typed commit failure")
}

fn staging_entries(directory: &Path) -> Vec<String> {
    let mut entries = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".rundelta-tmp-"))
        .collect::<Vec<_>>();
    entries.sort();
    entries
}

/// Bypass the writer so matching-checksum reader counterexamples stay independent.
fn forge_finalized_record(directory: &Path, metadata: &Metadata, event: &Event) {
    let metadata_bytes = serde_json::to_vec(metadata).unwrap();
    let mut event_bytes = serde_json::to_vec(event).unwrap();
    event_bytes.push(b'\n');
    let marker = Finalized {
        schema_version: 1,
        raw_manifest: None,
        run_id: metadata.id.clone(),
        event_count: 1,
        events: check(&event_bytes),
        metadata: check(&metadata_bytes),
    };
    fs::write(directory.join("metadata.json"), metadata_bytes).unwrap();
    fs::write(directory.join("events.jsonl"), event_bytes).unwrap();
    fs::write(
        directory.join("finalized.json"),
        serde_json::to_vec(&marker).unwrap(),
    )
    .unwrap();
}

fn create_current_record(root: &Path, id: &str, name: &str) {
    let run_dir = root.join("runs").join(id);
    fs::create_dir(&run_dir).unwrap();
    let event = event_fixture(2);
    let mut metadata = metadata_fixture(id);
    metadata.name = name.into();
    metadata.state = "completed".into();
    metadata.ended = Some(2);
    metadata.completeness = Some("complete_for_supported_events".into());
    metadata.exit_code = Some(0);
    metadata.collector_exit_code = Some(0);
    metadata.target_exit_raw = Some("+++ exited with 0 +++".into());
    metadata.parsed_events = 1;
    metadata.raw_lines = 1;
    finish(&run_dir, &metadata, &[event]).unwrap();
}

fn read_finalized_fixture(
    metadata_schema: u32,
    event_schemas: &[u32],
) -> super::Result<crate::model::Run> {
    let temp = TestDir::new("schema-tuple");
    let run_dir = temp.path().join("abc-1");
    fs::create_dir(&run_dir).unwrap();
    let mut metadata = metadata_fixture("abc-1");
    metadata.schema_version = metadata_schema;
    metadata.state = "completed".into();
    metadata.ended = Some(2);
    metadata.completeness = Some("complete_for_supported_events".into());
    metadata.exit_code = Some(0);
    metadata.collector_exit_code = Some(0);
    metadata.target_exit_raw = Some("+++ exited with 0 +++".into());
    metadata.parsed_events = event_schemas.len();
    metadata.raw_lines = usize::from(!event_schemas.is_empty());
    let metadata_bytes = serde_json::to_vec_pretty(&metadata).unwrap();
    let mut event_bytes = Vec::new();
    for schema in event_schemas {
        serde_json::to_writer(&mut event_bytes, &event_fixture(*schema)).unwrap();
        event_bytes.push(b'\n');
    }
    fs::write(run_dir.join("metadata.json"), &metadata_bytes).unwrap();
    fs::write(run_dir.join("events.jsonl"), &event_bytes).unwrap();
    let marker = Finalized {
        schema_version: 1,
        raw_manifest: None,
        run_id: metadata.id.clone(),
        event_count: event_schemas.len(),
        events: check(&event_bytes),
        metadata: check(&metadata_bytes),
    };
    fs::write(
        run_dir.join("finalized.json"),
        serde_json::to_vec_pretty(&marker).unwrap(),
    )
    .unwrap();
    read_run(run_dir, metadata)
}

#[test]
fn storage_resource_buffers_reject_excess_and_report_reservation_failure() {
    use std::io::Cursor;
    let temp = TestDir::new("bounded-read");
    let path = temp.path().join("input");
    fs::write(&path, b"12345").unwrap();
    assert_eq!(
        super::read_bounded(&mut fs::File::open(&path).unwrap(), 5, "input").unwrap(),
        b"12345"
    );
    let error = super::read_bounded(&mut fs::File::open(&path).unwrap(), 4, "input").unwrap_err();
    assert!(error.to_string().contains("resource limit exceeded"));
    let mut reader = Cursor::new(b"1234\nnext\n");
    let mut line = Vec::new();
    assert_eq!(
        super::read_bounded_line(&mut reader, &mut line, 5).unwrap(),
        5
    );
    assert_eq!(line, b"1234\n");
    assert_eq!(
        super::read_bounded_line(&mut reader, &mut line, 5).unwrap(),
        5
    );
    assert_eq!(line, b"next\n");
    let error = super::read_bounded_line(&mut Cursor::new(b"12345\n"), &mut line, 5).unwrap_err();
    assert!(error.to_string().contains("resource limit exceeded"));
    assert!(line.len() <= 5);
    let error =
        super::reserve_bounded(&mut Vec::<u8>::new(), usize::MAX, usize::MAX, "test").unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::OutOfMemory);
}

#[test]
fn json_value_budget_counts_containers_keys_and_scalars_without_string_punctuation() {
    let values = super::MAX_JSON_VALUES;
    let accepted = format!("[{}0]", "0,".repeat(values - 2));
    super::check_json_budget(accepted.as_bytes(), "test").unwrap();
    let rejected = format!("[{}0]", "0,".repeat(values - 1));
    assert!(
        super::check_json_budget(rejected.as_bytes(), "test")
            .unwrap_err()
            .to_string()
            .contains("JSON values")
    );
    let escaped = serde_json::to_vec(&vec!["\" [ {}, : true false 4 \\".repeat(values)]).unwrap();
    super::check_json_budget(&escaped, "test").unwrap();
    assert!(
        super::serialize_bounded(&rejected, 4, "test", false)
            .unwrap_err()
            .to_string()
            .contains("resource limit exceeded")
    );

    let temp = TestDir::new("stream-resource-priority");
    let path = temp.path().join("events");
    // A previous malformed line must not hide a later resource ceiling as
    // corruption or unsupported schema. No JSON tree is allocated here.
    fs::write(&path, format!("{{\"schema_version\":999}}\n{rejected}\n")).unwrap();
    let error = stream_event_readback(&mut fs::File::open(&path).unwrap())
        .err()
        .expect("JSON budget must be an immediate resource failure");
    assert!(error.to_string().contains("JSON values"));
}

#[test]
fn current_shape_accepts_supported_events_through_writer_and_reader() {
    let temp = TestDir::new("current-shape-roundtrip");
    for (index, (kind, operation, path, outcome, child)) in [
        ("file", "openat", Some("/observed"), "success", None),
        ("exec", "execve", Some("/usr/bin/true"), "ENOENT", None),
        ("cwd", "fchdir", Some("/observed"), "success", None),
        ("spawn", "clone", Some("2"), "success", Some("2")),
        ("spawn", "fork", None, "EAGAIN", None),
        ("context", "unshare", None, "success", None),
        ("exit", "exit", None, "+++ exited with 0 +++", None),
    ]
    .into_iter()
    .enumerate()
    {
        let mut event = event_fixture(2);
        event.kind = kind.into();
        event.operation = operation.into();
        event.path = path.map(str::to_owned);
        event.outcome = outcome.into();
        event.child_pid = child.map(str::to_owned);
        if kind == "exec" {
            event.argv = Some(vec!["true".into()]);
        }
        let id = format!("a{index:x}-1");
        let dir = temp.path().join(&id);
        fs::create_dir(&dir).unwrap();
        let metadata = completed_metadata_fixture(&id);
        finish(&dir, &metadata, std::slice::from_ref(&event)).unwrap();
        let loaded = read_run(dir, metadata).unwrap();
        assert_eq!(loaded.events[0].kind, kind);
        assert_eq!(loaded.storage_integrity.status, "verified");
    }
}

#[test]
fn complete_information_gaps_fail_both_boundaries_but_limited_evidence_is_preserved() {
    let temp = TestDir::new("information-gap-contract");
    for (index, (kind, operation, path, argv)) in [
        ("exec", "execve", Some("/usr/bin/true"), None),
        ("cwd", "fchdir", None, None),
        (
            "file",
            "openat",
            Some("relative:input [cwd unresolved]"),
            None,
        ),
        (
            "exec",
            "execve",
            Some("relative:input [cwd unresolved]"),
            Some(vec!["input"]),
        ),
        (
            "cwd",
            "chdir",
            Some("relative:input [cwd unresolved]"),
            None,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let mut event = event_fixture(2);
        event.kind = kind.into();
        event.operation = operation.into();
        event.path = path.map(str::to_owned);
        event.outcome = "success".into();
        event.argv = argv.map(|args| args.into_iter().map(str::to_owned).collect());
        assert!(
            !event.context_uncertain,
            "counterexample must not rely on the flag"
        );
        let id = format!("b{index:x}-1");
        let dir = temp.path().join(&id);
        fs::create_dir(&dir).unwrap();
        let complete = completed_metadata_fixture(&id);
        assert!(finish(&dir, &complete, std::slice::from_ref(&event)).is_err());
        assert!(!dir.join("finalized.json").exists());
        forge_finalized_record(&dir, &complete, &event);
        let error = read_run(dir.clone(), complete.clone()).unwrap_err();
        assert_eq!(
            storage_load_failure_kind(&error),
            StorageLoadFailureKind::Corrupt
        );
        let needs_uncertainty = path.is_none_or(|path| !path.starts_with('/'));
        let mut legal_event = event.clone();
        legal_event.context_uncertain = needs_uncertainty;
        assert!(finish(&dir, &complete, std::slice::from_ref(&legal_event)).is_err());
        forge_finalized_record(&dir, &complete, &legal_event);
        assert!(read_run(dir.clone(), complete.clone()).is_err());

        for incomplete in [false, true] {
            let mut limited = complete.clone();
            limited.warnings = vec!["synthetic information gap".into()];
            limited.completeness = Some(if incomplete { "incomplete" } else { "partial" }.into());
            if incomplete {
                limited.state = "capture_failed".into();
                limited.exit_code = None;
                limited.collector_exit_code = None;
                limited.target_exit_raw = None;
            }
            if needs_uncertainty {
                // A broad partial warning cannot excuse a false per-event
                // path claim. Reject it rather than rewriting saved flags.
                assert!(finish(&dir, &limited, std::slice::from_ref(&event)).is_err());
                forge_finalized_record(&dir, &limited, &event);
                let error = read_run(dir.clone(), limited.clone()).unwrap_err();
                assert_eq!(
                    storage_load_failure_kind(&error),
                    StorageLoadFailureKind::Corrupt
                );
            }
            finish(&dir, &limited, std::slice::from_ref(&legal_event)).unwrap();
            let loaded = read_run(dir.clone(), limited).unwrap();
            assert_eq!(loaded.storage_integrity.status, "verified");
            assert_eq!(loaded.events[0].path, event.path);
            assert_eq!(loaded.events[0].argv, event.argv);
            assert_eq!(
                loaded.events[0].context_uncertain, needs_uncertainty,
                "do not rewrite saved facts"
            );
        }

        let legacy = Metadata {
            schema_version: 2,
            ..complete
        };
        forge_finalized_record(&dir, &legacy, &event);
        assert_eq!(
            read_run(dir, legacy).unwrap().storage_integrity.status,
            "unverified"
        );
    }
}

#[test]
fn pathless_current_file_and_exec_cannot_be_finalized_or_loaded_as_verified() {
    let temp = TestDir::new("current-shape-invalid");
    for kind in ["file", "exec"] {
        let dir = temp.path().join(kind);
        fs::create_dir(&dir).unwrap();
        let metadata = completed_metadata_fixture("abc-1");
        let mut event = event_fixture(2);
        event.kind = kind.into();
        event.operation = if kind == "file" { "openat" } else { "execve" }.into();
        let error = finish(&dir, &metadata, std::slice::from_ref(&event)).unwrap_err();
        assert!(error.to_string().contains("nonempty observed path"));
        assert!(!dir.join("finalized.json").exists());
        assert!(!dir.join("events.jsonl").exists());

        // Reader coverage uses independently forged bytes and a matching
        // marker; it does not rely on the writer refusing the same input.
        forge_finalized_record(&dir, &metadata, &event);
        let error = read_run(dir.clone(), metadata.clone()).unwrap_err();
        assert_eq!(
            storage_load_failure_kind(&error),
            StorageLoadFailureKind::Corrupt
        );
        assert!(error.to_string().contains("current event shape"));

        fs::remove_file(dir.join("finalized.json")).unwrap();
        let legacy = Metadata {
            schema_version: 2,
            ..metadata
        };
        fs::write(
            dir.join("metadata.json"),
            serde_json::to_vec(&legacy).unwrap(),
        )
        .unwrap();
        let loaded = read_run(dir, legacy).unwrap();
        assert_eq!(loaded.storage_integrity.status, "unverified");
        assert!(loaded.events[0].path.is_none());
    }
}

#[test]
fn writer_resource_failure_never_publishes_unloadable_structured_output() {
    let temp = TestDir::new("writer-resource-cap");
    let dir = temp.path().join("run");
    fs::create_dir(&dir).unwrap();
    let mut metadata = completed_metadata_fixture("abc-1");
    metadata.name = "x".repeat(super::MAX_METADATA_BYTES);
    let event = event_fixture(2);
    let error = finish(&dir, &metadata, std::slice::from_ref(&event)).unwrap_err();
    assert_eq!(
        storage_load_failure_kind(&error),
        StorageLoadFailureKind::Unavailable
    );
    assert!(!dir.join("events.jsonl").exists());
    assert!(!dir.join("finalized.json").exists());

    let metadata = completed_metadata_fixture("abc-1");
    let mut large = event;
    large.outcome = "x".repeat(super::MAX_EVENT_LINE_BYTES);
    let error = finish(&dir, &metadata, &[large]).unwrap_err();
    assert_eq!(
        storage_load_failure_kind(&error),
        StorageLoadFailureKind::Unavailable
    );
    assert!(!dir.join("events.jsonl").exists());
    assert!(!dir.join("finalized.json").exists());
    assert!(staging_entries(&dir).is_empty());

    let path = dir.join("events.jsonl");
    let file = fs::File::create(&path).unwrap();
    file.set_len(super::MAX_EVENT_FILE_BYTES + 1).unwrap();
    let error = stream_event_readback(&mut fs::File::open(&path).unwrap())
        .err()
        .expect("file budget must reject before streaming");
    assert!(error.to_string().contains("resource limit exceeded"));
}

#[test]
fn fixed_metadata_temp_hardlink_is_ignored_without_touching_external_inode() {
    let temp = TestDir::new("metadata-hardlink");
    let run = temp.path().join("run");
    fs::create_dir(&run).unwrap();
    let sentinel = temp.path().join("sentinel");
    fs::write(&sentinel, b"outside stays unchanged").unwrap();
    fs::hard_link(&sentinel, run.join("metadata.tmp")).unwrap();

    metadata(&run, &metadata_fixture("abc-1")).unwrap();

    assert_eq!(fs::read(&sentinel).unwrap(), b"outside stays unchanged");
    assert_eq!(
        fs::read(run.join("metadata.tmp")).unwrap(),
        b"outside stays unchanged"
    );
    assert!(run.join("metadata.json").is_file());
}

#[test]
fn lock_rejects_runs_symlink_before_any_record_operation() {
    let temp = TestDir::new("runs-symlink");
    let root = temp.path().join("storage");
    let outside = temp.path().join("outside");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&outside).unwrap();
    symlink(&outside, root.join("runs")).unwrap();

    let error = lock(&root).err().expect("runs symlink must be rejected");

    assert!(error.to_string().contains("unsafe storage root"));
    assert!(outside.exists());
}

#[test]
fn lock_rejects_symlink_and_fifo_leaves_without_touching_their_targets() {
    let temp = TestDir::new("special-lock-leaf");
    let root = temp.path().join("storage");
    let storage = open(&root).unwrap();
    let sentinel = temp.path().join("sentinel");
    fs::write(&sentinel, b"keep").unwrap();
    let lock_path = root.join(".record.lock");

    symlink(&sentinel, &lock_path).unwrap();
    assert!(storage.lock().is_err());
    assert_eq!(fs::read(&sentinel).unwrap(), b"keep");
    fs::remove_file(&lock_path).unwrap();

    let fifo = CString::new(lock_path.as_os_str().as_bytes()).unwrap();
    // SAFETY: fifo is a live NUL-terminated path; mkfifo retains no pointer.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let error = storage
        .lock()
        .err()
        .expect("a FIFO lock leaf must be rejected without blocking");
    assert!(error.to_string().contains("unsafe storage lock"));
    assert_eq!(fs::read(&sentinel).unwrap(), b"keep");
}

#[test]
fn storage_root_holds_cloexec_directory_descriptors() {
    let temp = TestDir::new("storage-root-fds");
    let storage = open(&temp.path().join("storage")).unwrap();

    for fd in [storage.root_dir.as_raw_fd(), storage.runs_dir.as_raw_fd()] {
        // SAFETY: storage owns both live fds; F_GETFD uses no third argument
        // and does not transfer descriptor ownership.
        let descriptor_flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert!(descriptor_flags >= 0);
        assert_ne!(descriptor_flags & libc::FD_CLOEXEC, 0);
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: the live fd is borrowed and status is writable storage
        // for exactly one libc stat structure.
        assert_eq!(unsafe { libc::fstat(fd, status.as_mut_ptr()) }, 0);
        // SAFETY: the preceding assertion guarantees fstat initialized it.
        let status = unsafe { status.assume_init() };
        assert_eq!(status.st_mode & libc::S_IFMT, libc::S_IFDIR);
    }

    let real_root = temp.path().join("real-root");
    fs::create_dir(&real_root).unwrap();
    let linked_root = temp.path().join("linked-root");
    symlink(&real_root, &linked_root).unwrap();
    let error = open(&linked_root)
        .err()
        .expect("the storage root itself must not be followed");
    assert!(error.to_string().contains("unsafe storage root"));
}

#[test]
fn facade_and_compatibility_wrappers_are_equivalent() {
    let temp = TestDir::new("storage-root-wrappers");
    let root = temp.path().join("storage");
    let absent = temp.path().join("absent");
    assert!(records(&absent).unwrap().is_empty());
    assert!(all(&absent).unwrap().is_empty());
    assert!(load(&absent, "missing").is_err());
    assert!(!absent.exists());

    let storage = open(&root).unwrap();
    create_current_record(&root, "abc-1", "first");
    create_current_record(&root, "def-2", "second");

    assert_eq!(
        serde_json::to_value(storage.load("first").unwrap()).unwrap(),
        serde_json::to_value(load(&root, "first").unwrap()).unwrap()
    );
    let facade_records = storage.records().unwrap();
    let wrapper_records = records(&root).unwrap();
    assert_eq!(facade_records.len(), wrapper_records.len());
    for (facade, wrapper) in facade_records.iter().zip(&wrapper_records) {
        match (facade, wrapper) {
            (
                RecordSummary::Readable {
                    metadata: left,
                    directory: left_directory,
                    storage_integrity: left_integrity,
                    raw_integrity: left_raw,
                },
                RecordSummary::Readable {
                    metadata: right,
                    directory: right_directory,
                    storage_integrity: right_integrity,
                    raw_integrity: right_raw,
                },
            ) => {
                assert_eq!(
                    serde_json::to_value(left).unwrap(),
                    serde_json::to_value(right).unwrap()
                );
                assert_eq!(left_directory, right_directory);
                assert_eq!(left_integrity.status, right_integrity.status);
                assert_eq!(left_integrity.detail, right_integrity.detail);
                assert_eq!(
                    serde_json::to_value(left_raw).unwrap(),
                    serde_json::to_value(right_raw).unwrap()
                );
            }
            _ => panic!("facade and compatibility wrapper returned different row shapes"),
        }
    }
    let identities = storage.identities().unwrap();
    let compatibility = all(&root).unwrap();
    assert_eq!(identities.len(), compatibility.len());
    for identity in identities {
        let (path, old_identity) = compatibility
            .iter()
            .find(|(_, candidate)| candidate.id == identity.id)
            .unwrap();
        assert_eq!(old_identity.name, identity.name);
        assert_eq!(path, &root.join("runs").join(&identity.id));
    }

    let held = storage.lock().unwrap();
    let error = lock(&root).err().expect("second lock must be rejected");
    assert!(error.to_string().contains("storage busy"));
    drop(held);
    drop(lock(&root).unwrap());
    assert_eq!(storage.delete("first").unwrap(), "abc-1");
    assert_eq!(delete(&root, "second").unwrap(), "def-2");
}

#[test]
fn one_facade_stays_anchored_when_the_declared_path_is_swapped() {
    let temp = TestDir::new("storage-root-anchor");
    let root = temp.path().join("storage");
    let moved = temp.path().join("opened-storage");
    let moved_runs = moved.join("opened-runs");
    let outside = temp.path().join("outside");
    let storage = open(&root).unwrap();
    create_current_record(&root, "abc-1", "anchored");

    fs::rename(&root, &moved).unwrap();
    fs::rename(moved.join("runs"), &moved_runs).unwrap();
    fs::create_dir(&root).unwrap();
    let outside_record = outside.join("abc-1");
    fs::create_dir_all(&outside_record).unwrap();
    let sentinel = outside_record.join("sentinel");
    fs::write(&sentinel, b"outside remains").unwrap();
    symlink(&outside, moved.join("runs")).unwrap();
    symlink(&outside, root.join("runs")).unwrap();

    assert_eq!(storage.load("anchored").unwrap().metadata.id, "abc-1");
    assert_eq!(storage.records().unwrap().len(), 1);
    assert_eq!(storage.delete("abc-1").unwrap(), "abc-1");
    assert!(!moved_runs.join("abc-1").exists());
    assert_eq!(fs::read(&sentinel).unwrap(), b"outside remains");
    assert!(moved.join("runs").is_symlink());
    assert!(root.join("runs").is_symlink());
}

#[test]
fn run_writer_owns_the_normal_layout_and_trace_inode() {
    let temp = TestDir::new("run-writer-layout");
    let root = temp.path().join("storage");
    let storage = open(&root).unwrap();
    let mut writer = storage.begin_run_with_id("fresh", "abc-1".into()).unwrap();
    let run = root.join("runs/abc-1");

    assert_eq!(writer.id(), "abc-1");
    assert_eq!(writer.evidence_dir(), run);
    assert_eq!(
        fs::metadata(&run).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert!(run.join("raw").is_dir());
    assert!(run.join("raw/trace.log").is_file());
    assert_eq!(
        fs::metadata(run.join("raw/trace.log"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    for fd in [
        writer.storage.root_dir.as_raw_fd(),
        writer.storage.runs_dir.as_raw_fd(),
        writer.run_dir.as_raw_fd(),
        writer.raw_dir.as_raw_fd(),
        writer.trace_file.as_raw_fd(),
    ] {
        // SAFETY: writer owns each live fd; F_GETFD uses no third argument
        // and does not transfer descriptor ownership.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert!(flags >= 0);
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
    }

    let mut metadata = metadata_fixture(writer.id());
    metadata.name = "fresh".into();
    writer.write_metadata(&metadata).unwrap();
    let target = writer.trace_target().unwrap();
    let status = Command::new("/bin/sh")
        .args(["-c", "printf 'synthetic trace\\n' > \"$1\"", "writer"])
        .arg(&target)
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(writer.read_trace().unwrap(), "synthetic trace\n");

    let event = event_fixture(2);
    metadata.state = "completed".into();
    metadata.ended = Some(2);
    metadata.exit_code = Some(0);
    metadata.collector_exit_code = Some(0);
    metadata.target_exit_raw = Some("+++ exited with 0 +++".into());
    metadata.parsed_events = 1;
    metadata.raw_lines = 1;
    metadata.completeness = Some("complete_for_supported_events".into());
    writer.finish(&metadata, &[event]).unwrap();
    drop(lock(&root).expect("finalization releases its lock"));
    assert_eq!(
        load(&root, "fresh").unwrap().storage_integrity.status,
        "verified"
    );
}

#[test]
fn trace_streaming_facade_bounds_memory_and_preserves_line_termination() {
    let temp = TestDir::new("trace-streaming-facade");
    let root = temp.path().join("storage");
    let mut writer = open(&root)
        .unwrap()
        .begin_run_with_id("stream", "abc-1".into())
        .unwrap();
    let large = "x".repeat(192 * 1024);
    let contents = format!("{large}\r\nunterminated");
    writer.trace_file.write_all(contents.as_bytes()).unwrap();
    assert_eq!(writer.trace_len().unwrap(), contents.len() as u64);

    let mut lines = Vec::new();
    writer
        .for_each_trace_line(|line| {
            lines.push(line.to_owned());
            Ok(())
        })
        .unwrap();
    assert_eq!(lines, [format!("{large}\r\n"), "unterminated".into()]);

    writer.trace_file.set_len(0).unwrap();
    writer.trace_file.seek(SeekFrom::Start(0)).unwrap();
    writer
        .trace_file
        .write_all(b"valid\ninvalid:\xff\n")
        .unwrap();
    let mut accepted = Vec::new();
    let error = writer
        .for_each_trace_line(|line| {
            accepted.push(line.to_owned());
            Ok(())
        })
        .expect_err("each logical line must be valid UTF-8");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(accepted, ["valid\n"]);
}

#[test]
fn initial_metadata_cannot_be_replaced_or_retried_after_an_io_error() {
    let temp = TestDir::new("writer-initial-once");
    let root = temp.path().join("storage");
    for fail_first in [false, true] {
        let id = if fail_first { "abc-1" } else { "def-2" };
        let mut writer = open(&root)
            .unwrap()
            .begin_run_with_id("writer", id.into())
            .unwrap();
        let mut metadata = metadata_fixture(id);
        metadata.name = "writer".into();
        let path = writer.evidence_dir().join("metadata.json");
        if fail_first {
            fs::create_dir(&path).unwrap();
            assert!(writer.write_metadata(&metadata).is_err());
            fs::remove_dir(&path).unwrap();
        } else {
            writer.write_metadata(&metadata).unwrap();
        }
        let before = fs::read(&path).ok();
        metadata.backend = "must not replace initial metadata".into();
        assert!(writer.write_metadata(&metadata).is_err());
        assert_eq!(fs::read(&path).ok(), before);
        assert!(!writer.evidence_dir().join("finalized.json").exists());
        drop(writer);
        drop(lock(&root).expect("failed initialization must release the lock"));
        // Each iteration reserves the same label only after its old record
        // is removed by this test's explicit directory cleanup.
        fs::remove_dir_all(root.join("runs").join(id)).unwrap();
    }
}

#[test]
fn writer_requires_initial_metadata_before_finalization() {
    let temp = TestDir::new("writer-needs-initial");
    let root = temp.path().join("storage");
    let writer = open(&root)
        .unwrap()
        .begin_run_with_id("test", "abc-1".into())
        .unwrap();
    assert!(
        writer
            .finish(&completed_metadata_fixture("abc-1"), &[event_fixture(2)])
            .is_err()
    );
    assert!(!root.join("runs/abc-1/events.jsonl").exists());
    assert!(!root.join("runs/abc-1/finalized.json").exists());
    drop(lock(&root).expect("rejected finalization releases its lock"));
}

#[test]
fn writer_authority_is_exclusive_and_finish_consumes_it() {
    // These assignments are compile-time regressions against weakening the
    // receivers back to shared borrows, without adding a compile-test crate.
    let _: fn(&mut super::RunWriter, &Metadata) -> anyhow::Result<()> =
        super::RunWriter::write_metadata;
    let _: fn(super::RunWriter, &Metadata, &[Event]) -> anyhow::Result<()> =
        super::RunWriter::finish;

    let temp = TestDir::new("writer-finalization-error");
    let root = temp.path().join("storage");
    let mut writer = open(&root)
        .unwrap()
        .begin_run_with_id("test", "abc-1".into())
        .unwrap();
    assert!(
        writer
            .write_metadata(&completed_metadata_fixture("abc-1"))
            .is_err()
    );
    let initial = metadata_fixture("abc-1");
    writer.write_metadata(&initial).unwrap();
    // A finalized record cannot contain running metadata; rejection still
    // consumes the writer and releases its resources without a marker.
    assert!(writer.finish(&initial, &[]).is_err());
    drop(lock(&root).expect("failed finalization releases its lock"));
    assert!(!root.join("runs/abc-1/finalized.json").exists());
}

#[test]
fn begin_run_rejects_all_name_and_id_collision_directions() {
    let temp = TestDir::new("run-writer-collisions");
    let root = temp.path().join("storage");
    open(&root).unwrap();
    create_current_record(&root, "abc-1", "def-2");

    for (name, id, expected) in [
        ("def-2", "fed-3", "record name already exists"),
        ("abc-1", "fed-3", "record name already exists"),
        ("fresh", "def-2", "record ID already exists"),
        ("fresh", "abc-1", "record ID already exists"),
    ] {
        let error = open(&root)
            .unwrap()
            .begin_run_with_id(name, id.into())
            .err()
            .expect("all four collision directions must be rejected");
        assert!(
            error.to_string().contains(expected),
            "name={name:?} id={id:?}: {error:#}"
        );
    }
    assert!(!root.join("runs/fed-3").exists());
}

#[test]
fn damaged_index_hints_reserve_all_name_and_id_collision_directions() {
    let temp = TestDir::new("damaged-run-writer-collisions");
    let root = temp.path().join("storage");
    open(&root).unwrap();
    open(&root).unwrap().check_name_available("feed-5").unwrap();
    let damaged = root.join("runs/dead-4");
    fs::create_dir(&damaged).unwrap();
    fs::write(
        damaged.join("metadata.json"),
        br#"{"schema_version":999,"name":"feed-5"}"#,
    )
    .unwrap();

    for (name, id, expected) in [
        ("feed-5", "cafe-6", "record name already exists"),
        ("dead-4", "cafe-6", "record name already exists"),
        ("fresh", "feed-5", "record ID already exists"),
        ("fresh", "dead-4", "record ID already exists"),
    ] {
        let error = open(&root)
            .unwrap()
            .begin_run_with_id(name, id.into())
            .err()
            .expect("damaged index hints must remain reserved");
        assert!(
            error.to_string().contains(expected),
            "name={name:?} id={id:?}: {error:#}"
        );
    }
    assert!(!root.join("runs/cafe-6").exists());
}

#[test]
fn typed_record_summaries_isolate_every_damage_class() {
    let temp = TestDir::new("typed-record-summaries");
    let root = temp.path().join("storage");
    open(&root).unwrap();
    create_current_record(&root, "a00-1", "healthy");

    let runs = root.join("runs");
    let corrupt = runs.join("b00-2");
    fs::create_dir(&corrupt).unwrap();
    fs::write(
        corrupt.join("metadata.json"),
        br#"{"schema_version":3,"name":"corrupt-label"}"#,
    )
    .unwrap();

    let future = runs.join("c00-3");
    fs::create_dir(&future).unwrap();
    fs::write(
        future.join("metadata.json"),
        br#"{"schema_version":999,"name":"future-label"}"#,
    )
    .unwrap();

    let unavailable = runs.join("d00-4");
    fs::create_dir(&unavailable).unwrap();
    let sentinel = temp.path().join("outside-metadata");
    fs::write(&sentinel, b"outside sentinel").unwrap();
    symlink(&sentinel, unavailable.join("metadata.json")).unwrap();

    let missing_label = runs.join("e00-5");
    fs::create_dir(&missing_label).unwrap();
    fs::write(missing_label.join("metadata.json"), b"{broken").unwrap();

    fs::create_dir(runs.join("not-a-run-id")).unwrap();

    let summaries = records(&root).unwrap();
    assert_eq!(summaries.len(), 6);
    let summary = |directory: &str| {
        summaries
            .iter()
            .find(|summary| match summary {
                RecordSummary::Readable {
                    directory: candidate,
                    ..
                }
                | RecordSummary::Unreadable {
                    directory: candidate,
                    ..
                } => candidate == directory,
            })
            .unwrap()
    };

    match summary("runs/a00-1") {
        RecordSummary::Readable {
            metadata,
            storage_integrity,
            ..
        } => {
            assert_eq!(metadata.name, "healthy");
            assert_eq!(storage_integrity.status, "verified");
        }
        _ => panic!("healthy row must be readable"),
    }
    for (directory, id, name, status) in [
        (
            "runs/b00-2",
            Some("b00-2"),
            Some("corrupt-label"),
            "corrupt",
        ),
        (
            "runs/c00-3",
            Some("c00-3"),
            Some("future-label"),
            "unsupported",
        ),
        ("runs/d00-4", Some("d00-4"), None, "unavailable"),
        ("runs/e00-5", Some("e00-5"), None, "corrupt"),
        ("runs/not-a-run-id", None, None, "unavailable"),
    ] {
        match summary(directory) {
            RecordSummary::Unreadable {
                id: actual_id,
                name: actual_name,
                storage_integrity,
                ..
            } => {
                assert_eq!(actual_id.as_deref(), id);
                assert_eq!(actual_name.as_deref(), name);
                assert_eq!(storage_integrity.status, status);
            }
            _ => panic!("damaged row {directory} must remain isolated"),
        }
    }
    assert_eq!(fs::read(sentinel).unwrap(), b"outside sentinel");
}

#[test]
fn fd_relative_delete_unlinks_internal_special_files_without_following_them() {
    let temp = TestDir::new("delete-special-leaves");
    let root = temp.path().join("storage");
    open(&root).unwrap();
    create_current_record(&root, "abc-1", "victim");
    let run = root.join("runs/abc-1");
    let outside = temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let sentinel = outside.join("sentinel");
    fs::write(&sentinel, b"keep").unwrap();
    fs::create_dir(run.join("nested")).unwrap();
    symlink(&outside, run.join("nested/link")).unwrap();
    let fifo = run.join("nested/pipe");
    let fifo_name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: fifo_name is a live NUL-terminated path; mkfifo retains no pointer.
    assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);

    assert_eq!(delete(&root, "victim").unwrap(), "abc-1");
    assert!(!run.exists());
    assert_eq!(fs::read(&sentinel).unwrap(), b"keep");

    symlink(&outside, root.join("runs/def-2")).unwrap();
    let error = delete(&root, "def-2").expect_err("a run leaf symlink must not be selected");
    assert!(
        error
            .to_string()
            .contains("expected one safe ID/label match")
    );
    assert_eq!(fs::read(&sentinel).unwrap(), b"keep");
}

#[test]
fn storage_root_trailing_separators_do_not_bypass_leaf_symlink_rejection() {
    let temp = TestDir::new("root-trailing-separators");
    let root = temp.path().join("storage");
    open(&root).unwrap();
    create_current_record(&root, "abc-1", "healthy");
    let link = temp.path().join("linked");
    symlink(&root, &link).unwrap();

    for suffix in ["", "/", "///"] {
        let linked = PathBuf::from(format!("{}{suffix}", link.display()));
        assert!(open(&linked).is_err(), "root symlink spelling: {linked:?}");
        assert!(records(&linked).is_err());
        assert!(load(&linked, "healthy").is_err());
        assert!(delete(&linked, "abc-1").is_err());
        assert!(root.join("runs/abc-1/metadata.json").is_file());

        let real = PathBuf::from(format!("{}{suffix}", root.display()));
        assert_eq!(records(&real).unwrap().len(), 1);
        assert_eq!(load(&real, "healthy").unwrap().metadata.id, "abc-1");
    }
    // Ancestors are still resolved once by the OS at the configured boundary.
    assert_eq!(records(&link.join("runs/..")).unwrap().len(), 1);
    assert!(open_directory(Path::new("////")).is_ok());
    assert!(open_directory(Path::new("")).is_err());
}

#[test]
fn recursive_delete_depth_has_an_accepted_boundary_and_a_fail_closed_limit() {
    let temp = TestDir::new("delete-depth");
    let root = temp.path().join("storage");
    open(&root).unwrap();
    for (id, depth) in [("abc-1", 128), ("def-2", 129)] {
        let run = root.join("runs").join(id);
        fs::create_dir(&run).unwrap();
        fs::write(run.join("first-evidence"), b"preserve before deep child").unwrap();
        let mut leaf = run.clone();
        for _ in 0..depth {
            leaf.push("n");
            fs::create_dir(&leaf).unwrap();
        }
        fs::write(leaf.join("sentinel"), b"deep evidence").unwrap();
        if depth == 128 {
            assert_eq!(delete(&root, id).unwrap(), id);
            assert!(!run.exists());
        } else {
            let error = delete(&root, id).unwrap_err();
            assert_eq!(
                storage_load_failure_kind(&error),
                StorageLoadFailureKind::Unavailable
            );
            assert!(format!("{error:#}").contains("directory depth: 128"));
            assert_eq!(fs::read(leaf.join("sentinel")).unwrap(), b"deep evidence");
            assert_eq!(
                fs::read(run.join("first-evidence")).unwrap(),
                b"preserve before deep child"
            );
        }
    }
}

#[test]
fn run_leaf_swap_fails_closed_before_delete_or_trace_output() {
    let temp = TestDir::new("run-leaf-swap");
    let root = temp.path().join("storage");
    let storage = open(&root).unwrap();
    create_current_record(&root, "abc-1", "victim");
    let entry = storage.resolve("victim").unwrap();
    let original = root.join("runs/original");
    fs::rename(root.join("runs/abc-1"), &original).unwrap();
    let outside = temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let sentinel = outside.join("sentinel");
    fs::write(&sentinel, b"keep").unwrap();
    symlink(&outside, root.join("runs/abc-1")).unwrap();

    let error = storage
        .delete_resolved(entry)
        .expect_err("the resolved leaf identity changed");
    assert!(error.to_string().contains("unsafe record directory"));
    assert!(original.join("metadata.json").is_file());
    assert_eq!(fs::read(&sentinel).unwrap(), b"keep");

    fs::remove_file(root.join("runs/abc-1")).unwrap();
    fs::rename(&original, root.join("runs/abc-1")).unwrap();
    assert_eq!(storage.delete("victim").unwrap(), "abc-1");

    let writer = open(&root)
        .unwrap()
        .begin_run_with_id("writer", "def-2".into())
        .unwrap();
    let moved = root.join("runs/moved-writer");
    fs::rename(root.join("runs/def-2"), &moved).unwrap();
    symlink(&outside, root.join("runs/def-2")).unwrap();
    let error = writer
        .trace_target()
        .expect_err("trace output must fail closed after a leaf swap");
    assert!(error.to_string().contains("record directory changed"));
    assert_eq!(fs::read(&sentinel).unwrap(), b"keep");
}

#[test]
fn trace_handle_stays_inside_the_opened_root_after_ancestor_swap() {
    let temp = TestDir::new("trace-ancestor-swap");
    let root = temp.path().join("storage");
    let moved = temp.path().join("opened-storage");
    let outside = temp.path().join("outside");
    let mut writer = open(&root)
        .unwrap()
        .begin_run_with_id("writer", "abc-1".into())
        .unwrap();
    let mut metadata = metadata_fixture(writer.id());
    metadata.name = "writer".into();
    writer.write_metadata(&metadata).unwrap();

    fs::rename(&root, &moved).unwrap();
    fs::create_dir(&root).unwrap();
    fs::create_dir(&outside).unwrap();
    let sentinel = outside.join("sentinel");
    fs::write(&sentinel, b"outside").unwrap();
    symlink(&outside, root.join("runs")).unwrap();
    let target = writer.trace_target().unwrap();
    fs::write(target, b"anchored\n").unwrap();

    assert_eq!(writer.read_trace().unwrap(), "anchored\n");
    assert_eq!(
        fs::read(moved.join("runs/abc-1/raw/trace.log")).unwrap(),
        b"anchored\n"
    );
    assert_eq!(fs::read(&sentinel).unwrap(), b"outside");
}

#[test]
fn only_the_current_finalized_schema_tuple_is_verified() {
    let event_v1 = event_fixture(1);
    let event_v2 = event_fixture(2);
    for metadata_schema in [1, 2] {
        for events in [
            Vec::new(),
            vec![event_v1.clone()],
            vec![event_v2.clone()],
            vec![event_v1.clone(), event_v2.clone()],
        ] {
            assert_eq!(
                finalized_integrity(metadata_schema, &events)
                    .unwrap()
                    .status,
                "unverified"
            );
        }
    }
    assert_eq!(finalized_integrity(3, &[]).unwrap().status, "verified");
    assert_eq!(
        finalized_integrity(3, std::slice::from_ref(&event_v2))
            .unwrap()
            .status,
        "verified"
    );
    assert!(finalized_integrity(3, std::slice::from_ref(&event_v1)).is_err());
    assert!(finalized_integrity(3, &[event_v1, event_v2]).is_err());
}

#[test]
fn finalized_reader_does_not_promote_legacy_or_accept_mixed_current_events() {
    for metadata_schema in [1, 2] {
        for event_schemas in [&[1][..], &[2][..], &[1, 2][..]] {
            assert_eq!(
                read_finalized_fixture(metadata_schema, event_schemas)
                    .unwrap()
                    .storage_integrity
                    .status,
                "unverified"
            );
        }
    }
    assert_eq!(
        read_finalized_fixture(3, &[2])
            .unwrap()
            .storage_integrity
            .status,
        "verified"
    );
    assert!(read_finalized_fixture(3, &[1]).is_err());
    assert!(read_finalized_fixture(3, &[1, 2]).is_err());
}

#[test]
fn streaming_load_collects_large_events_while_list_validates_without_retaining_them() {
    let temp = TestDir::new("streaming-load-modes");
    let root = temp.path().join("storage");
    let storage = open(&root).unwrap();
    let run_dir = root.join("runs/abc-1");
    fs::create_dir(&run_dir).unwrap();
    let mut event = event_fixture(2);
    event.outcome = "x".repeat(512 * 1024 + 37);
    let metadata = completed_metadata_fixture("abc-1");
    finish(&run_dir, &metadata, std::slice::from_ref(&event)).unwrap();

    let loaded = storage.load("abc-1").unwrap();
    assert_eq!(loaded.events.len(), 1);
    assert_eq!(loaded.events[0].outcome, event.outcome);
    assert_eq!(loaded.storage_integrity.status, "verified");

    let directory = open_directory(&run_dir).unwrap();
    let validated = read_record_at(&directory, "abc-1", EventReadMode::ValidateOnly).unwrap();
    assert!(validated.events.is_none());
    assert_eq!(validated.storage_integrity.status, "verified");
    assert!(matches!(
        storage.records().unwrap().as_slice(),
        [RecordSummary::Readable { .. }]
    ));
}

#[test]
fn markerless_legacy_events_still_stream_and_remain_unverified() {
    let temp = TestDir::new("streaming-legacy");
    let run_dir = temp.path().join("abc-1");
    fs::create_dir(&run_dir).unwrap();
    let mut metadata = completed_metadata_fixture("abc-1");
    metadata.schema_version = 1;
    metadata.environment = None;
    metadata.completeness = None;
    let mut event = event_fixture(1);
    event.outcome = "legacy".repeat(48 * 1024);
    let mut event_bytes = serde_json::to_vec(&event).unwrap();
    event_bytes.push(b'\n');
    fs::write(
        run_dir.join("metadata.json"),
        serde_json::to_vec_pretty(&metadata).unwrap(),
    )
    .unwrap();
    fs::write(run_dir.join("events.jsonl"), event_bytes).unwrap();

    let loaded = read_run(run_dir.clone(), metadata.clone()).unwrap();
    assert_eq!(loaded.events.len(), 1);
    assert_eq!(
        serde_json::to_value(&loaded.events[0]).unwrap(),
        serde_json::to_value(&event).unwrap()
    );
    assert_eq!(loaded.storage_integrity.status, "unverified");
    let directory = open_directory(&run_dir).unwrap();
    let validated = read_record_at(&directory, "abc-1", EventReadMode::ValidateOnly).unwrap();
    assert!(validated.events.is_none());
    assert_eq!(validated.storage_integrity.status, "unverified");
}

#[test]
fn current_record_without_marker_never_reads_speculative_events() {
    let temp = TestDir::new("unfinalized-no-events-read");
    let root = temp.path().join("storage");
    let storage = open(&root).unwrap();
    let run_dir = root.join("runs/abc-1");
    fs::create_dir(&run_dir).unwrap();
    let metadata = completed_metadata_fixture("abc-1");
    fs::write(
        run_dir.join("metadata.json"),
        serde_json::to_vec_pretty(&metadata).unwrap(),
    )
    .unwrap();
    let events_fifo = run_dir.join("events.jsonl");
    let events_fifo = CString::new(events_fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: events_fifo is a live NUL-terminated path; mkfifo retains no pointer.
    assert_eq!(unsafe { libc::mkfifo(events_fifo.as_ptr(), 0o600) }, 0);

    let loaded = storage.load("abc-1").unwrap();
    assert!(loaded.events.is_empty());
    assert_eq!(loaded.storage_integrity.status, "unfinalized");
    match storage.records().unwrap().as_slice() {
        [
            RecordSummary::Readable {
                storage_integrity, ..
            },
        ] => assert_eq!(storage_integrity.status, "unfinalized"),
        _ => panic!("an unfinalized current record remains listable"),
    }
}

#[test]
fn events_jsonl_bytes_and_lexical_error_priority_are_frozen() {
    let event = event_fixture(2);
    let mut bytes = serde_json::to_vec(&event).unwrap();
    bytes.push(b'\n');
    assert_eq!(
            bytes,
            br#"{"schema_version":2,"pid":"1","kind":"exit","operation":"exit","path":null,"outcome":"+++ exited with 0 +++","evidence":{"file":"raw/trace.log","line":1,"end_line":1},"argv":null,"child_pid":null,"cwd_before":null,"context_uncertain":false}
"#
        );
    assert_eq!(decode_events(&bytes).unwrap().len(), 1);

    let temp = TestDir::new("events-wire-bytes");
    let run_dir = temp.path().join("abc-1");
    fs::create_dir(&run_dir).unwrap();
    let metadata = completed_metadata_fixture("abc-1");
    finish(&run_dir, &metadata, std::slice::from_ref(&event)).unwrap();
    assert_eq!(fs::read(run_dir.join("events.jsonl")).unwrap(), bytes);
    let metadata_bytes = serde_json::to_vec_pretty(&metadata).unwrap();
    let expected_marker = Finalized {
        schema_version: 1,
        raw_manifest: None,
        run_id: "abc-1".into(),
        event_count: 1,
        events: check(&bytes),
        metadata: check(&metadata_bytes),
    };
    assert_eq!(
        fs::read(run_dir.join("finalized.json")).unwrap(),
        serde_json::to_vec_pretty(&expected_marker).unwrap()
    );

    let mut crlf = bytes.clone();
    crlf.insert(crlf.len() - 1, b'\r');
    assert_eq!(decode_events(&crlf).unwrap().len(), 1);

    let invalid_utf8 = decode_events(b"{\"schema_version\":2}\xff")
        .expect_err("whole-file UTF-8 validation precedes final-line validation");
    assert!(
        invalid_utf8
            .to_string()
            .contains("events.jsonl: invalid utf-8")
    );

    let incomplete = decode_events(br#"{"schema_version":2}"#)
        .expect_err("a nonempty JSONL file must end in LF");
    assert!(
        incomplete
            .to_string()
            .contains("events.jsonl: incomplete final line")
    );

    let future = decode_events(b"{\"schema_version\":999}\n")
        .expect_err("a future header precedes full DTO decoding");
    assert_eq!(
        storage_load_failure_kind(&future),
        StorageLoadFailureKind::Unsupported
    );
}

#[test]
fn streaming_event_readback_keeps_bytes_and_error_priority() {
    let temp = TestDir::new("streaming-event-readback");
    let path = temp.path().join("events.jsonl");
    let read = |bytes: &[u8]| {
        fs::write(&path, bytes).unwrap();
        stream_event_readback(&mut fs::File::open(&path).unwrap()).unwrap()
    };

    let event = event_fixture(2);
    let mut valid = serde_json::to_vec(&event).unwrap();
    valid.push(b'\n');
    let valid_readback = read(&valid);
    assert_eq!(valid_readback.check, check(&valid));
    assert_eq!(valid_readback.decoded.unwrap().facts.count(), 1);

    let invalid_utf8 = read(b"{\"schema_version\":999}\n\xff")
        .decoded
        .expect_err("whole-file UTF-8 failure precedes schema and final-line errors");
    assert!(
        invalid_utf8
            .to_string()
            .contains("events.jsonl: invalid utf-8")
    );

    let incomplete = read(b"not-json")
        .decoded
        .expect_err("an incomplete final line precedes DTO decoding");
    assert!(incomplete.to_string().contains("incomplete final line"));

    let future = read(b"{\"schema_version\":999}\n")
        .decoded
        .expect_err("a verified future header remains unsupported");
    assert_eq!(
        storage_load_failure_kind(&future),
        StorageLoadFailureKind::Unsupported
    );
}

#[test]
fn streaming_readback_rejects_a_replaced_published_path() {
    let temp = TestDir::new("streaming-readback-swap");
    let run = temp.path().join("run");
    fs::create_dir(&run).unwrap();
    let directory = open_directory(&run).unwrap();
    let event = event_fixture(2);
    let mut bytes = serde_json::to_vec(&event).unwrap();
    bytes.push(b'\n');
    let destination = run.join("events.jsonl");
    fs::write(&destination, &bytes).unwrap();
    let retained = fs::File::open(&destination).unwrap();
    let published = PublishedFile {
        file: retained,
        stage: CommitStage::Durable,
    };
    let expected = EventFileSummary {
        check: check(&bytes),
        facts: EventFacts::from_events(std::slice::from_ref(&event)),
    };

    fs::rename(&destination, run.join("detached-events")).unwrap();
    let sentinel = temp.path().join("outside-sentinel");
    fs::write(&sentinel, b"outside remains").unwrap();
    fs::hard_link(&sentinel, &destination).unwrap();

    let error =
        read_back_published_events(&directory, &published, &expected, AtomicControl::default())
            .expect_err("the published component must still name the staged inode");
    let failure = error
        .downcast_ref::<CommitFailure>()
        .expect("identity failures retain their atomic commit stage");
    assert_eq!(failure.stage, CommitStage::Durable);
    assert_eq!(fs::read(&sentinel).unwrap(), b"outside remains");
    assert_eq!(fs::read(&destination).unwrap(), b"outside remains");
    assert!(!run.join("finalized.json").exists());
}

#[test]
fn finalized_checksums_precede_metadata_and_event_decoding() {
    let temp = TestDir::new("checksum-priority");
    let run_dir = temp.path().join("abc-1");
    fs::create_dir(&run_dir).unwrap();
    let metadata = completed_metadata_fixture("abc-1");
    let metadata_bytes = serde_json::to_vec_pretty(&metadata).unwrap();
    let future_events = b"{\"schema_version\":999}\n";
    fs::write(run_dir.join("metadata.json"), &metadata_bytes).unwrap();
    fs::write(run_dir.join("events.jsonl"), future_events).unwrap();

    let marker = |metadata_check, events_check| Finalized {
        schema_version: 1,
        raw_manifest: None,
        run_id: metadata.id.clone(),
        event_count: 1,
        events: events_check,
        metadata: metadata_check,
    };
    let write_marker = |marker: &Finalized| {
        fs::write(
            run_dir.join("finalized.json"),
            serde_json::to_vec_pretty(marker).unwrap(),
        )
        .unwrap();
    };

    write_marker(&marker(check(&metadata_bytes), check(b"different events")));
    let stale_events = read_run(run_dir.clone(), metadata.clone())
        .expect_err("checksum mismatch must precede a future event header");
    assert!(
        stale_events
            .to_string()
            .contains("byte count or SHA-256 mismatch")
    );
    assert_eq!(
        storage_load_failure_kind(&stale_events),
        StorageLoadFailureKind::Corrupt
    );

    write_marker(&marker(check(&metadata_bytes), check(future_events)));
    let verified_future = read_run(run_dir.clone(), metadata.clone())
        .expect_err("the future event header is visible after checksum verification");
    assert_eq!(
        storage_load_failure_kind(&verified_future),
        StorageLoadFailureKind::Unsupported
    );

    let future_metadata = br#"{"schema_version":999}"#;
    fs::write(run_dir.join("metadata.json"), future_metadata).unwrap();
    write_marker(&marker(check(b"different metadata"), check(future_events)));
    let stale_metadata = read_run(run_dir.clone(), metadata.clone())
        .expect_err("metadata checksum mismatch must precede its future header");
    assert!(
        stale_metadata
            .to_string()
            .contains("byte count or SHA-256 mismatch")
    );
    assert_eq!(
        storage_load_failure_kind(&stale_metadata),
        StorageLoadFailureKind::Corrupt
    );

    write_marker(&marker(check(future_metadata), check(b"different events")));
    let stale_events_before_future_metadata = read_run(run_dir.clone(), metadata.clone())
        .expect_err("events checksum verification precedes future metadata decoding");
    assert!(
        stale_events_before_future_metadata
            .to_string()
            .contains("events.jsonl: byte count or SHA-256 mismatch")
    );

    let invalid_metadata = b"{\"schema_version\":3,\xff}";
    let invalid_events = b"\xff\n";
    fs::write(run_dir.join("metadata.json"), invalid_metadata).unwrap();
    fs::write(run_dir.join("events.jsonl"), invalid_events).unwrap();
    write_marker(&marker(check(invalid_metadata), check(b"different events")));
    let stale_events_before_metadata = read_run(run_dir.clone(), metadata.clone())
        .expect_err("all finalized checksums precede metadata decoding");
    assert!(
        stale_events_before_metadata
            .to_string()
            .contains("events.jsonl: byte count or SHA-256 mismatch")
    );

    write_marker(&marker(check(invalid_metadata), check(invalid_events)));
    let metadata_before_events = read_run(run_dir, metadata)
        .expect_err("metadata decoding precedes event UTF-8 decoding after verification");
    assert!(metadata_before_events.to_string().contains("metadata.json"));
    assert_eq!(
        storage_load_failure_kind(&metadata_before_events),
        StorageLoadFailureKind::Corrupt
    );
}

#[test]
fn finalized_metadata_errors_precede_event_decode_but_follow_all_checksums() {
    let temp = TestDir::new("finalized-error-order");
    let run_dir = temp.path().join("abc-1");
    fs::create_dir(&run_dir).unwrap();
    let metadata = metadata_fixture("abc-1");
    let future_metadata = br#"{"schema_version":999}"#;
    let invalid_events = b"\xff\n";
    fs::write(run_dir.join("metadata.json"), future_metadata).unwrap();
    fs::write(run_dir.join("events.jsonl"), invalid_events).unwrap();
    let marker = Finalized {
        schema_version: 1,
        raw_manifest: None,
        run_id: "abc-1".into(),
        event_count: 1,
        events: check(invalid_events),
        metadata: check(future_metadata),
    };
    fs::write(
        run_dir.join("finalized.json"),
        serde_json::to_vec_pretty(&marker).unwrap(),
    )
    .unwrap();

    let error = read_run(run_dir, metadata)
        .expect_err("metadata schema is interpreted before event UTF-8 after verification");
    assert_eq!(
        storage_load_failure_kind(&error),
        StorageLoadFailureKind::Unsupported
    );
    assert!(
        error
            .to_string()
            .contains("unsupported metadata schema 999")
    );
}

#[test]
fn marker_header_and_dto_precede_run_files_and_identity_checks() {
    let temp = TestDir::new("marker-priority");
    let run_dir = temp.path().join("abc-1");
    fs::create_dir(&run_dir).unwrap();
    let metadata = metadata_fixture("abc-1");

    fs::write(run_dir.join("finalized.json"), br#"{"schema_version":999}"#).unwrap();
    let future = read_run(run_dir.clone(), metadata.clone())
        .expect_err("future marker header precedes missing run files");
    assert_eq!(
        storage_load_failure_kind(&future),
        StorageLoadFailureKind::Unsupported
    );

    fs::write(run_dir.join("finalized.json"), br#"{"schema_version":1}"#).unwrap();
    let incomplete = read_run(run_dir, metadata)
        .expect_err("marker DTO completeness precedes missing run files");
    assert_eq!(
        storage_load_failure_kind(&incomplete),
        StorageLoadFailureKind::Corrupt
    );
    assert!(incomplete.to_string().contains("finalized.json"));
}

#[test]
fn streaming_reader_checks_count_then_cross_file_contract() {
    let temp = TestDir::new("streaming-count-contract");
    let run_dir = temp.path().join("abc-1");
    fs::create_dir(&run_dir).unwrap();
    let metadata = completed_metadata_fixture("abc-1");
    let metadata_bytes = serde_json::to_vec_pretty(&metadata).unwrap();
    let event = event_fixture(2);
    let mut event_bytes = serde_json::to_vec(&event).unwrap();
    event_bytes.push(b'\n');
    fs::write(run_dir.join("metadata.json"), &metadata_bytes).unwrap();
    fs::write(run_dir.join("events.jsonl"), &event_bytes).unwrap();
    let write_marker = |event_count, events: &FileCheck| {
        let marker = Finalized {
            schema_version: 1,
            raw_manifest: None,
            run_id: metadata.id.clone(),
            event_count,
            events: events.clone(),
            metadata: check(&metadata_bytes),
        };
        fs::write(
            run_dir.join("finalized.json"),
            serde_json::to_vec_pretty(&marker).unwrap(),
        )
        .unwrap();
    };

    write_marker(2, &check(&event_bytes));
    let count = read_run(run_dir.clone(), metadata.clone())
        .expect_err("marker count must match the streamed DTO count");
    assert!(count.to_string().contains("event count mismatch"));

    let mut invalid_event = event;
    invalid_event.evidence.end_line = 2;
    let mut invalid_bytes = serde_json::to_vec(&invalid_event).unwrap();
    invalid_bytes.push(b'\n');
    fs::write(run_dir.join("events.jsonl"), &invalid_bytes).unwrap();
    write_marker(1, &check(&invalid_bytes));
    let contract = read_run(run_dir, metadata)
        .expect_err("verified DTOs must still satisfy the cross-file evidence contract");
    assert!(contract.to_string().contains("run contract"));
    assert!(contract.to_string().contains("evidence lines"));
}

#[test]
fn index_hints_never_override_finalized_checksum_priority() {
    let temp = TestDir::new("index-checksum-priority");
    let root = temp.path().join("storage");
    let storage = open(&root).unwrap();
    let run_dir = root.join("runs/abc-1");
    fs::create_dir(&run_dir).unwrap();
    let speculative = br#"{"schema_version":999,"name":"priority-label"}"#;
    fs::write(run_dir.join("metadata.json"), speculative).unwrap();
    fs::write(run_dir.join("events.jsonl"), b"").unwrap();
    let marker = Finalized {
        schema_version: 1,
        raw_manifest: None,
        run_id: "abc-1".into(),
        event_count: 0,
        events: check(b""),
        metadata: check(b"different metadata"),
    };
    fs::write(
        run_dir.join("finalized.json"),
        serde_json::to_vec_pretty(&marker).unwrap(),
    )
    .unwrap();

    let error = storage
        .load("priority-label")
        .expect_err("the label hint may select, but cannot validate, a record");
    assert_eq!(
        storage_load_failure_kind(&error),
        StorageLoadFailureKind::Corrupt
    );
    assert!(format!("{error:#}").contains("byte count or SHA-256 mismatch"));
    match storage.records().unwrap().as_slice() {
        [
            RecordSummary::Unreadable {
                name,
                storage_integrity,
                ..
            },
        ] => {
            assert_eq!(name.as_deref(), Some("priority-label"));
            assert_eq!(storage_integrity.status, "corrupt");
            assert!(
                storage_integrity
                    .detail
                    .contains("byte count or SHA-256 mismatch")
            );
        }
        _ => panic!("the damaged row must remain isolated and retain its index hint"),
    }
}

#[test]
fn atomic_replace_faults_report_the_exact_publish_stage_and_clean_staging() {
    let temp = TestDir::new("atomic-faults");
    let path = temp.path().join("run");
    fs::create_dir(&path).unwrap();
    let directory = open_directory(&path).unwrap();
    let target = path.join("payload");
    let unrelated = path.join(".rundelta-tmp-preexisting");
    fs::write(&unrelated, b"do not touch").unwrap();
    let expected_staging = vec![".rundelta-tmp-preexisting".to_owned()];

    for step in [AtomicStep::Write, AtomicStep::FileSync, AtomicStep::Rename] {
        fs::write(&target, b"old").unwrap();
        let error = atomic_replace_with_control(
            &directory,
            "payload",
            b"new",
            AtomicControl {
                fail: Some(("payload", step)),
                force_named_fallback: true,
                force_link_unsupported: false,
            },
        )
        .unwrap_err();
        assert_eq!(error.stage, CommitStage::BeforePublish, "{step:?}");
        assert!(error.cleanup.is_none(), "{step:?}");
        assert_eq!(fs::read(&target).unwrap(), b"old", "{step:?}");
        assert_eq!(staging_entries(&path), expected_staging, "{step:?}");
        assert_eq!(fs::read(&unrelated).unwrap(), b"do not touch");
    }

    let error = atomic_replace_with_control(
        &directory,
        "payload",
        b"published",
        AtomicControl {
            fail: Some(("payload", AtomicStep::DirectorySync)),
            force_named_fallback: true,
            force_link_unsupported: false,
        },
    )
    .unwrap_err();
    assert_eq!(error.stage, CommitStage::PublishedDurabilityUnknown);
    assert!(error.cleanup.is_none());
    assert_eq!(fs::read(&target).unwrap(), b"published");
    assert!(error.to_string().contains("was published"));
    assert_eq!(staging_entries(&path), expected_staging);

    assert_eq!(
        atomic_replace_with_control(
            &directory,
            "payload",
            b"durable",
            AtomicControl {
                fail: None,
                force_named_fallback: true,
                force_link_unsupported: false,
            },
        )
        .unwrap(),
        CommitStage::Durable
    );
    assert_eq!(fs::read(&target).unwrap(), b"durable");
    assert_eq!(staging_entries(&path), expected_staging);

    // The production path attempts O_TMPFILE first and safely falls back on
    // filesystems whose anonymous files cannot be linked.
    assert_eq!(
        atomic_replace_with_control(
            &directory,
            "anonymous-or-fallback",
            b"bytes",
            AtomicControl::default(),
        )
        .unwrap(),
        CommitStage::Durable
    );
    assert_eq!(
        fs::read(path.join("anonymous-or-fallback")).unwrap(),
        b"bytes"
    );
    assert_eq!(staging_entries(&path), expected_staging);
}

#[test]
fn anonymous_link_fallback_copies_in_bounded_chunks_and_cleans_exactly() {
    let temp = TestDir::new("atomic-anonymous-copy");
    let directory = open_directory(temp.path()).unwrap();
    let mut source = create_staging(&directory, false).unwrap();
    if let Some(name) = source.name.take() {
        // Filesystems without O_TMPFILE first return the production named
        // fallback. Unlink exactly that entry while retaining its fd so
        // the rest of this test still starts from a true anonymous inode.
        unlink_at(&directory, &name, 0).unwrap();
    }
    assert_eq!(file_stat(&source.file).unwrap().st_nlink, 0);
    let contents = (0..(192 * 1024 + 17))
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    source.file.write_all(&contents).unwrap();

    for step in [AtomicStep::FallbackCopy, AtomicStep::FallbackFileSync] {
        let error = copy_anonymous_to_named(
            &mut source,
            "payload",
            AtomicControl {
                fail: Some(("payload", step)),
                force_named_fallback: false,
                force_link_unsupported: false,
            },
        )
        .err()
        .expect("the injected fallback fault must be reported");
        assert_eq!(error.stage, CommitStage::BeforePublish);
        assert!(error.cleanup.is_none(), "{step:?}");
        assert!(staging_entries(temp.path()).is_empty(), "{step:?}");
        assert_eq!(file_stat(&source.file).unwrap().st_nlink, 0, "{step:?}");
    }

    let mut copied =
        copy_anonymous_to_named(&mut source, "payload", AtomicControl::default()).unwrap();
    copied.file.seek(SeekFrom::Start(0)).unwrap();
    let mut actual = Vec::new();
    copied.file.read_to_end(&mut actual).unwrap();
    assert_eq!(actual, contents);
    assert_eq!(staging_entries(temp.path()).len(), 1);
    drop(copied);
    assert!(staging_entries(temp.path()).is_empty());
    assert_eq!(file_stat(&source.file).unwrap().st_nlink, 0);

    let published = AtomicStaging {
        staged: source,
        published: CString::new("payload").unwrap(),
        display: "payload",
        control: AtomicControl {
            fail: None,
            force_named_fallback: false,
            force_link_unsupported: true,
        },
    }
    .publish()
    .unwrap();
    assert_eq!(published.stage, CommitStage::Durable);
    assert_eq!(fs::read(temp.path().join("payload")).unwrap(), contents);
    assert!(staging_entries(temp.path()).is_empty());
}

#[test]
fn staging_cleanup_is_idempotent_and_only_unlinks_its_exact_name() {
    let temp = TestDir::new("atomic-cleanup");
    let directory = open_directory(temp.path()).unwrap();
    let sentinel = temp.path().join(".rundelta-tmp-unrelated");
    fs::write(&sentinel, b"keep").unwrap();
    let mut staged = create_named_staging(&directory).unwrap();
    let staged_name = staged.name.as_ref().unwrap().to_bytes().to_vec();
    assert!(temp.path().join(crate::model::bytes(&staged_name)).exists());

    staged.cleanup().unwrap();
    staged.cleanup().unwrap();

    assert!(!temp.path().join(crate::model::bytes(&staged_name)).exists());
    assert_eq!(fs::read(&sentinel).unwrap(), b"keep");
}

#[test]
fn staging_identity_check_rejects_a_replaced_name_before_publish() {
    let temp = TestDir::new("atomic-staging-swap");
    let directory = open_directory(temp.path()).unwrap();
    let destination = temp.path().join("payload");
    let sentinel = temp.path().join("outside-sentinel");
    fs::write(&destination, b"old destination").unwrap();
    fs::write(&sentinel, b"outside stays unchanged").unwrap();

    let mut staged = create_named_staging(&directory).unwrap();
    staged.file.write_all(b"new staged bytes").unwrap();
    staged.file.sync_all().unwrap();
    let staging_name = staged.name.as_ref().unwrap().to_bytes().to_vec();
    let staging_path = temp.path().join(crate::model::bytes(&staging_name));
    fs::remove_file(&staging_path).unwrap();
    fs::hard_link(&sentinel, &staging_path).unwrap();

    let error = verify_staging_for_publish(&staged, "payload")
        .expect_err("a replaced staging entry must not be published");
    assert_eq!(error.stage, CommitStage::BeforePublish);
    assert_eq!(error.source.kind(), std::io::ErrorKind::InvalidData);
    drop(staged);

    assert_eq!(fs::read(&destination).unwrap(), b"old destination");
    assert_eq!(fs::read(&sentinel).unwrap(), b"outside stays unchanged");
    assert_eq!(fs::read(&staging_path).unwrap(), b"outside stays unchanged");
}

#[test]
fn explicit_staging_cleanup_rejects_replacement_and_drop_leaves_it_untouched() {
    for replacement_is_symlink in [false, true] {
        let temp = TestDir::new("atomic-cleanup-replacement");
        let directory = open_directory(temp.path()).unwrap();
        let sentinel = temp.path().join("sentinel");
        fs::write(&sentinel, b"keep replacement").unwrap();
        let mut staged = create_named_staging(&directory).unwrap();
        let staging_path = temp.path().join(crate::model::bytes(
            staged.name.as_ref().unwrap().to_bytes(),
        ));
        fs::remove_file(&staging_path).unwrap();
        if replacement_is_symlink {
            symlink(&sentinel, &staging_path).unwrap();
        } else {
            fs::hard_link(&sentinel, &staging_path).unwrap();
        }

        for _ in 0..2 {
            let error = staged.cleanup().expect_err("replacement is not owned");
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
            assert!(staged.name.is_some());
        }
        drop(staged);

        assert_eq!(fs::read(&sentinel).unwrap(), b"keep replacement");
        assert_eq!(fs::read(&staging_path).unwrap(), b"keep replacement");
        assert_eq!(staging_path.is_symlink(), replacement_is_symlink);
    }
}

#[test]
fn staging_cleanup_accepts_an_already_absent_owned_name() {
    let temp = TestDir::new("atomic-cleanup-missing");
    let directory = open_directory(temp.path()).unwrap();
    let mut staged = create_named_staging(&directory).unwrap();
    let staging_path = temp.path().join(crate::model::bytes(
        staged.name.as_ref().unwrap().to_bytes(),
    ));
    fs::remove_file(&staging_path).unwrap();

    staged.cleanup().unwrap();
    staged.cleanup().unwrap();
    assert!(staged.name.is_none());
    assert!(!staging_path.exists());
}

#[test]
fn only_materializing_an_unnamed_tmpfile_treats_enoent_as_fallback() {
    let error = std::io::Error::from_raw_os_error(libc::ENOENT);
    assert!(tmpfile_link_is_unsupported(&error));
    assert!(!tmpfile_is_unsupported(&error));
}

#[test]
fn finalization_faults_distinguish_durable_readback_from_published_marker() {
    let event = event_fixture(2);

    let readback_temp = TestDir::new("readback-fault");
    let readback_run = readback_temp.path().join("abc-1");
    fs::create_dir(&readback_run).unwrap();
    let readback_directory = open_directory(&readback_run).unwrap();
    let metadata = completed_metadata_fixture("abc-1");
    let error = finish_at_with_control(
        &readback_directory,
        &metadata,
        std::slice::from_ref(&event),
        AtomicControl {
            fail: Some(("events.jsonl", AtomicStep::ReadBack)),
            force_named_fallback: true,
            force_link_unsupported: false,
        },
    )
    .unwrap_err();
    assert_eq!(commit_failure(&error).stage, CommitStage::Durable);
    assert!(readback_run.join("events.jsonl").is_file());
    assert!(!readback_run.join("finalized.json").exists());
    assert!(staging_entries(&readback_run).is_empty());

    let marker_temp = TestDir::new("marker-dirsync-fault");
    let marker_run = marker_temp.path().join("abc-1");
    fs::create_dir(&marker_run).unwrap();
    let marker_directory = open_directory(&marker_run).unwrap();
    let error = finish_at_with_control(
        &marker_directory,
        &metadata,
        std::slice::from_ref(&event),
        AtomicControl {
            fail: Some(("finalized.json", AtomicStep::DirectorySync)),
            force_named_fallback: true,
            force_link_unsupported: false,
        },
    )
    .unwrap_err();
    let failure = commit_failure(&error);
    assert_eq!(failure.stage, CommitStage::PublishedDurabilityUnknown);
    assert!(failure.to_string().contains("was published"));
    assert!(marker_run.join("finalized.json").is_file());
    assert_eq!(
        read_run(marker_run.clone(), metadata.clone())
            .unwrap()
            .storage_integrity
            .status,
        "verified"
    );
    assert!(staging_entries(&marker_run).is_empty());

    // Retrying through the same protocol replaces the published files and
    // reaches Durable without depending on rollback of the earlier marker.
    finish_at_with_control(
        &marker_directory,
        &metadata,
        &[event],
        AtomicControl {
            fail: None,
            force_named_fallback: true,
            force_link_unsupported: false,
        },
    )
    .unwrap();
    assert!(staging_entries(&marker_run).is_empty());
}

#[test]
fn future_headers_are_unsupported_before_current_dto_fields_are_decoded() {
    for error in [
        decode_metadata(br#"{"schema_version":999}"#, "abc-1").unwrap_err(),
        decode_events(b"{\"schema_version\":999}\n").unwrap_err(),
        decode_finalized(br#"{"schema_version":999}"#)
            .err()
            .expect("future marker must fail"),
    ] {
        assert_eq!(
            storage_load_failure_kind(&error),
            StorageLoadFailureKind::Unsupported,
            "{error:#}"
        );
    }

    for error in [
        decode_metadata(br#"{"schema_version":3}"#, "abc-1").unwrap_err(),
        decode_events(b"{\"schema_version\":2}\n").unwrap_err(),
        decode_finalized(br#"{"schema_version":1}"#)
            .err()
            .expect("incomplete current marker must fail"),
    ] {
        assert_eq!(
            storage_load_failure_kind(&error),
            StorageLoadFailureKind::Corrupt,
            "{error:#}"
        );
    }
}

#[test]
fn unknown_current_wire_state_values_are_corrupt_after_typed_conversion() {
    for (state, completeness) in [
        ("future_state", "complete_for_supported_events"),
        ("completed", "future_completeness"),
    ] {
        let mut metadata = completed_metadata_fixture("abc-1");
        metadata.state = state.into();
        metadata.completeness = Some(completeness.into());
        let error = decode_metadata(&serde_json::to_vec(&metadata).unwrap(), "abc-1")
            .expect_err("unknown wire enum values must fail closed");
        assert_eq!(
            storage_load_failure_kind(&error),
            StorageLoadFailureKind::Corrupt,
            "{error:#}"
        );
    }
}

#[test]
fn marker_run_id_contradiction_is_corrupt_not_unsupported() {
    let temp = TestDir::new("marker-run-id");
    let directory = temp.path().join("abc-1");
    fs::create_dir(&directory).unwrap();
    let marker = Finalized {
        schema_version: 1,
        raw_manifest: None,
        run_id: "other-2".into(),
        event_count: 0,
        events: check(&[]),
        metadata: check(&[]),
    };
    fs::write(
        directory.join("finalized.json"),
        serde_json::to_vec(&marker).unwrap(),
    )
    .unwrap();

    let error = read_run(directory, metadata_fixture("abc-1")).unwrap_err();
    assert_eq!(
        storage_load_failure_kind(&error),
        StorageLoadFailureKind::Corrupt,
        "{error:#}"
    );
    assert!(error.to_string().contains("run ID"));
}

#[test]
fn errno_mapping_reserves_busy_for_flock_contention() {
    for errno in [libc::EAGAIN, libc::EWOULDBLOCK] {
        let error = lock_error(std::io::Error::from_raw_os_error(errno));
        assert!(matches!(storage_error(&error), StorageError::Busy(_)));
    }
    for errno in [libc::ENOLCK, libc::EPERM, libc::EMFILE, libc::EACCES] {
        let error = lock_error(std::io::Error::from_raw_os_error(errno));
        assert!(matches!(
            storage_error(&error),
            StorageError::Unavailable {
                source: Some(_),
                ..
            }
        ));
    }
    let synthetic = lock_error(std::io::Error::new(
        std::io::ErrorKind::WouldBlock,
        "not an errno",
    ));
    assert!(matches!(
        storage_error(&synthetic),
        StorageError::Unavailable { .. }
    ));
}

#[test]
fn concrete_storage_errors_map_to_neutral_load_outcomes() {
    let failures: Vec<(anyhow::Error, StorageLoadFailureKind, &str)> = vec![
        (
            StorageError::Corrupt("bad bytes".into()).into(),
            StorageLoadFailureKind::Corrupt,
            "corrupt record: bad bytes",
        ),
        (
            StorageError::Unsupported("future schema".into()).into(),
            StorageLoadFailureKind::Unsupported,
            "future schema",
        ),
        (
            StorageError::Unavailable {
                message: "read metadata".into(),
                source: Some(std::io::Error::from_raw_os_error(libc::EMFILE)),
            }
            .into(),
            StorageLoadFailureKind::Unavailable,
            "read metadata",
        ),
        (
            StorageError::Busy("storage busy".into()).into(),
            StorageLoadFailureKind::Busy,
            "storage busy",
        ),
        (
            anyhow::anyhow!("non-storage tool failure"),
            StorageLoadFailureKind::Unavailable,
            "non-storage tool failure",
        ),
    ];
    for (error, kind, message) in failures {
        let failure = neutral_load_failure(error);
        assert_eq!(failure.kind, kind);
        assert!(failure.message.contains(message), "{}", failure.message);
    }

    let temp = TestDir::new("neutral-load-outcome");
    let root = temp.path().join("storage");
    open(&root).unwrap();
    create_current_record(&root, "abc-1", "readable");
    assert!(matches!(
        load_outcome(&root, "readable"),
        LoadOutcome::Loaded(_)
    ));
    match load_outcome(&root, "missing") {
        LoadOutcome::Failed(failure) => {
            assert_eq!(failure.kind, StorageLoadFailureKind::Unavailable);
            assert!(failure.message.contains("expected one safe ID/label match"));
        }
        LoadOutcome::Loaded(_) => panic!("missing record unexpectedly loaded"),
    }
}

#[test]
fn required_file_io_distinguishes_damage_from_resource_unavailability() {
    let missing = required_record_file_error(
        "metadata.json",
        std::io::Error::from(std::io::ErrorKind::NotFound),
    );
    assert_eq!(
        storage_load_failure_kind(&missing),
        StorageLoadFailureKind::Corrupt
    );

    for errno in [libc::EACCES, libc::EMFILE] {
        let error =
            required_record_file_error("metadata.json", std::io::Error::from_raw_os_error(errno));
        assert!(matches!(
            storage_error(&error),
            StorageError::Unavailable {
                source: Some(_),
                ..
            }
        ));
        assert_eq!(
            storage_load_failure_kind(&error),
            StorageLoadFailureKind::Unavailable
        );
    }
}

/// Synthetic current trace tuple; tests retain the exact raw input independently.
fn trace_fixture(id: &str) -> (Metadata, Vec<Event>, Vec<u8>) {
    use crate::model::{
        EventReference, TARGET_SELECTION_RULE_V1, TASK_IDENTITY_CONTRACT_V1, TargetProvenance,
        TaskIdentity,
    };
    let mut metadata = completed_metadata_fixture(id);
    metadata.schema_version = 4;
    metadata.evidence_contract = Some("trace-v1".into());
    metadata.command = vec!["/bin/tool".into()];
    metadata.raw_lines = 2;
    metadata.parsed_events = 2;
    metadata.target_exit_raw = Some("+++ exited with 0 +++".into());
    let mut exec = event_fixture(3);
    exec.kind = "exec".into();
    exec.operation = "execve".into();
    exec.pid = "10".into();
    exec.path = Some("/bin/tool".into());
    exec.argv = Some(vec!["/bin/tool".into()]);
    exec.outcome = "success".into();
    exec.task_generation = Some(1);
    let mut exit = exec.clone();
    exit.kind = "exit".into();
    exit.operation = "exit".into();
    exit.path = None;
    exit.argv = None;
    exit.outcome = "+++ exited with 0 +++".into();
    exit.evidence.line = 2;
    exit.evidence.end_line = 2;
    metadata.target_provenance = Some(TargetProvenance {
        schema_version: 1,
        selection_rule: TARGET_SELECTION_RULE_V1.into(),
        identity_contract: TASK_IDENTITY_CONTRACT_V1.into(),
        root: TaskIdentity {
            pid: "10".into(),
            generation: 1,
        },
        root_exec: EventReference {
            index: 0,
            evidence: exec.evidence.clone(),
        },
        target_exit: EventReference {
            index: 1,
            evidence: exit.evidence.clone(),
        },
    });
    (
        metadata,
        vec![exec, exit],
        b"10 execve(\"/bin/tool\", [\"/bin/tool\"], []) = 0\n10 +++ exited with 0 +++\n".to_vec(),
    )
}

fn initial_trace_metadata(id: &str, name: &str) -> Metadata {
    let mut metadata = metadata_fixture(id);
    metadata.name = name.into();
    metadata.schema_version = 4;
    metadata.evidence_contract = Some("trace-v1".into());
    metadata
}

fn failed_trace_metadata(id: &str, name: &str) -> Metadata {
    let mut metadata = initial_trace_metadata(id, name);
    metadata.state = "capture_failed".into();
    metadata.completeness = Some("incomplete".into());
    metadata.ended = Some(2);
    metadata.warnings = vec!["normalization input was unavailable".into()];
    metadata
}

fn parsed_manifest(metadata: &Metadata, raw: &[u8]) -> crate::model::RawManifest {
    use crate::model::{RawManifest, RawReceipt};
    RawManifest {
        schema_version: 1,
        run_id: metadata.id.clone(),
        file: "raw/trace.log".into(),
        receipt: RawReceipt::Parsed {
            bytes: raw.len() as u64,
            logical_lines: metadata.raw_lines,
            sha256: check(raw).sha256,
        },
    }
}

fn finish_trace_fixture(root: &Path) -> (String, PathBuf) {
    let mut writer = open(root).unwrap().begin_run("trace").unwrap();
    let id = writer.id().to_owned();
    writer
        .write_metadata(&initial_trace_metadata(&id, "trace"))
        .unwrap();
    let (mut metadata, events, raw) = trace_fixture(&id);
    metadata.name = "trace".into();
    fs::write(writer.trace_target().unwrap(), &raw).unwrap();
    writer.for_each_trace_line(|_| Ok(())).unwrap();
    writer.finish(&metadata, &events).unwrap();
    (id.clone(), root.join("runs").join(id))
}

#[test]
fn trace_writer_binds_the_actual_normalization_stream_and_preserves_default_tuple() {
    let temp = TestDir::new("raw-bound-stream");
    let root = temp.path().join("store");
    let (id, directory) = finish_trace_fixture(&root);
    let run = load(&root, &id).unwrap();
    assert_eq!(run.storage_integrity.status, "verified");
    let raw_integrity = run.raw_integrity.unwrap();
    assert_eq!(raw_integrity.status, "verified");
    let (_, _, raw) = trace_fixture(&id);
    assert_eq!(
        raw_integrity.manifest.unwrap(),
        parsed_manifest(&run.metadata, &raw)
    );
    let marker = decode_finalized(&fs::read(directory.join("finalized.json")).unwrap()).unwrap();
    assert_eq!(marker.schema_version, 2);
    assert_eq!(
        marker.raw_manifest.unwrap(),
        check(&fs::read(directory.join("raw-integrity.json")).unwrap())
    );
    let rows = records(&root).unwrap();
    assert!(
        matches!(&rows[0], RecordSummary::Readable {storage_integrity,raw_integrity:Some(raw),..} if storage_integrity.status=="verified" && raw.status=="verified")
    );

    create_current_record(&root, "abc-7", "default");
    let default = load(&root, "default").unwrap();
    assert_eq!(default.metadata.schema_version, 3);
    assert!(default.raw_integrity.is_none());
    assert!(!root.join("runs/abc-7/raw-integrity.json").exists());
    assert!(
        serde_json::to_value(&default)
            .unwrap()
            .get("raw_integrity")
            .is_none()
    );
}

#[test]
fn raw_damage_is_retained_separately_from_verified_structured_facts() {
    for mutation in [
        "missing",
        "truncated",
        "same_length",
        "symlink",
        "fifo",
        "raw_directory",
    ] {
        let temp = TestDir::new("raw-independent-damage");
        let root = temp.path().join("store");
        let (id, directory) = finish_trace_fixture(&root);
        let trace = directory.join("raw/trace.log");
        match mutation {
            "missing" => fs::remove_file(&trace).unwrap(),
            "truncated" => fs::write(&trace, b"10\n").unwrap(),
            "same_length" => {
                let mut bytes = fs::read(&trace).unwrap();
                bytes[0] = b'9';
                fs::write(&trace, bytes).unwrap();
            }
            "symlink" => {
                fs::remove_file(&trace).unwrap();
                symlink(directory.join("metadata.json"), &trace).unwrap();
            }
            "fifo" => {
                fs::remove_file(&trace).unwrap();
                let path = CString::new(trace.as_os_str().as_bytes()).unwrap();
                // SAFETY: CString owns the NUL-terminated pathname through the
                // call; mkfifo retains no pointer and creates only this test leaf.
                assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
            }
            "raw_directory" => {
                fs::remove_file(&trace).unwrap();
                fs::remove_dir(directory.join("raw")).unwrap();
            }
            _ => unreachable!(),
        }
        let run = load(&root, &id).unwrap();
        assert_eq!(run.storage_integrity.status, "verified", "{mutation}");
        assert_eq!(run.events.len(), 2);
        let expected = if matches!(mutation, "symlink" | "fifo") {
            "unavailable"
        } else {
            "corrupt"
        };
        assert_eq!(run.raw_integrity.unwrap().status, expected, "{mutation}");
        let rows = records(&root).unwrap();
        assert!(
            matches!(&rows[0],RecordSummary::Readable {storage_integrity,raw_integrity:Some(raw),..} if storage_integrity.status=="verified" && raw.status==expected)
        );
    }
}

#[test]
fn skipped_and_failed_consumption_never_get_a_complete_raw_receipt() {
    for failure in ["skipped", "utf8", "consumer"] {
        let temp = TestDir::new("raw-unverified");
        let root = temp.path().join("store");
        let mut writer = open(&root).unwrap().begin_run("trace").unwrap();
        let id = writer.id().to_owned();
        writer
            .write_metadata(&initial_trace_metadata(&id, "trace"))
            .unwrap();
        fs::write(
            writer.trace_target().unwrap(),
            if failure == "utf8" {
                b"first\n\xff".as_slice()
            } else {
                b"first\nsecond\n".as_slice()
            },
        )
        .unwrap();
        match failure {
            "utf8" => assert!(writer.for_each_trace_line(|_| Ok(())).is_err()),
            "consumer" => assert!(
                writer
                    .for_each_trace_line(|_| Err(std::io::Error::other("consumer stopped")))
                    .is_err()
            ),
            "skipped" => {}
            _ => unreachable!(),
        }
        writer
            .finish(&failed_trace_metadata(&id, "trace"), &[])
            .unwrap();
        let run = load(&root, &id).unwrap();
        assert_eq!(run.storage_integrity.status, "verified");
        let raw = run.raw_integrity.unwrap();
        assert_eq!(raw.status, "unverified");
        assert!(
            matches!(raw.manifest.unwrap().receipt,crate::model::RawReceipt::Unverified {reason} if !reason.is_empty())
        );
        assert!(run.events.is_empty());
    }
}

#[test]
fn raw_changes_after_consumption_reject_finalization_without_manufacturing_marker() {
    let temp = TestDir::new("raw-consumption-mismatch");
    let root = temp.path().join("store");
    let mut writer = open(&root).unwrap().begin_run("trace").unwrap();
    let id = writer.id().to_owned();
    let directory = writer.evidence_dir().to_path_buf();
    writer
        .write_metadata(&initial_trace_metadata(&id, "trace"))
        .unwrap();
    let (mut metadata, events, mut raw) = trace_fixture(&id);
    metadata.name = "trace".into();
    fs::write(writer.trace_target().unwrap(), &raw).unwrap();
    writer.for_each_trace_line(|_| Ok(())).unwrap();
    raw[0] = b'9';
    fs::write(writer.trace_target().unwrap(), raw).unwrap();
    let error = writer.finish(&metadata, &events).unwrap_err();
    assert_eq!(
        storage_load_failure_kind(&error),
        StorageLoadFailureKind::Corrupt
    );
    assert!(!directory.join("finalized.json").exists());
    assert_eq!(
        load(&root, &id).unwrap().storage_integrity.status,
        "unfinalized"
    );
}

#[test]
fn raw_manifest_and_marker_publication_faults_preserve_the_exact_commit_stage() {
    for (leaf, step) in [
        ("raw-integrity.json", AtomicStep::Write),
        ("raw-integrity.json", AtomicStep::ReadBack),
        ("raw-integrity.json", AtomicStep::DirectorySync),
        ("raw/trace.log", AtomicStep::ReadBack),
        ("finalized.json", AtomicStep::Rename),
        ("finalized.json", AtomicStep::DirectorySync),
    ] {
        let temp = TestDir::new("raw-commit-fault");
        let directory = temp.path().join("abc-1");
        fs::create_dir(&directory).unwrap();
        fs::create_dir(directory.join("raw")).unwrap();
        let (metadata, events, raw) = trace_fixture("abc-1");
        fs::write(directory.join("raw/trace.log"), &raw).unwrap();
        let manifest = parsed_manifest(&metadata, &raw);
        let fd = open_directory(&directory).unwrap();
        let error = super::finish_at_with_manifest_control(
            &fd,
            &metadata,
            &events,
            Some(&manifest),
            AtomicControl {
                fail: Some((leaf, step)),
                ..Default::default()
            },
        )
        .unwrap_err();
        if leaf == "finalized.json" && step == AtomicStep::DirectorySync {
            assert_eq!(
                commit_failure(&error).stage,
                CommitStage::PublishedDurabilityUnknown
            );
            assert_eq!(
                read_record_at(&fd, "abc-1", EventReadMode::CollectEvents)
                    .unwrap()
                    .storage_integrity
                    .status,
                "verified"
            );
        } else {
            assert!(
                !directory.join("finalized.json").exists(),
                "{leaf} {step:?}"
            );
        }
        assert!(staging_entries(&directory).is_empty());
    }
}

#[test]
fn trace_manifest_is_required_and_legacy_sidecars_are_not_reinterpreted() {
    let temp = TestDir::new("raw-required-manifest");
    let root = temp.path().join("store");
    let (id, directory) = finish_trace_fixture(&root);
    fs::remove_file(directory.join("raw-integrity.json")).unwrap();
    assert_eq!(
        storage_load_failure_kind(&load(&root, &id).unwrap_err()),
        StorageLoadFailureKind::Corrupt
    );
    create_current_record(&root, "abc-7", "default");
    fs::write(
        root.join("runs/abc-7/raw-integrity.json"),
        b"not a future proof",
    )
    .unwrap();
    let legacy = load(&root, "default").unwrap();
    assert_eq!(legacy.storage_integrity.status, "verified");
    assert!(legacy.raw_integrity.is_none());
}

#[test]
fn raw_checksum_line_framing_rejects_consistently_forged_counts() {
    for raw in [
        b"".as_slice(),
        b"one\n".as_slice(),
        b"one\nlast".as_slice(),
        b"one\r\n\r".as_slice(),
    ] {
        let temp = TestDir::new("raw-framing-count");
        let file = temp.path().join("trace");
        fs::write(&file, raw).unwrap();
        let expected = if raw.is_empty() {
            0
        } else {
            raw.iter().filter(|&&byte| byte == b'\n').count() + usize::from(!raw.ends_with(b"\n"))
        };
        let (check, lines) =
            super::calculate_file_check_with_lines(&mut fs::File::open(&file).unwrap(), true)
                .unwrap();
        assert_eq!(check, super::check(raw));
        assert_eq!(lines, expected);
    }
    let temp = TestDir::new("raw-forged-framing");
    let directory = temp.path().join("abc-1");
    fs::create_dir(&directory).unwrap();
    fs::create_dir(directory.join("raw")).unwrap();
    let (mut metadata, events, raw) = trace_fixture("abc-1");
    fs::write(directory.join("raw/trace.log"), &raw).unwrap();
    metadata.raw_lines += 1;
    let manifest = parsed_manifest(&metadata, &raw);
    let fd = open_directory(&directory).unwrap();
    let error = super::finish_at_with_manifest_control(
        &fd,
        &metadata,
        &events,
        Some(&manifest),
        AtomicControl::default(),
    )
    .unwrap_err();
    assert_eq!(
        storage_load_failure_kind(&error),
        StorageLoadFailureKind::Corrupt
    );
    assert!(format!("{error:#}").contains("logical line count"));
    assert!(!directory.join("finalized.json").exists());
}

#[test]
fn recording_protocol_tuple_cannot_be_silently_mixed_or_promoted() {
    let mut event3 = event_fixture(3);
    event3.task_generation = Some(1);
    let event2 = event_fixture(2);
    for (metadata, events, marker, expected) in [
        (4, vec![event3.clone()], 2, None),
        (3, vec![event2.clone()], 1, None),
        (
            3,
            vec![event3.clone()],
            1,
            Some(StorageLoadFailureKind::Corrupt),
        ),
        (
            2,
            vec![event2.clone(), event3.clone()],
            1,
            Some(StorageLoadFailureKind::Corrupt),
        ),
        (
            4,
            vec![event2.clone()],
            2,
            Some(StorageLoadFailureKind::Corrupt),
        ),
        (
            4,
            vec![event3.clone()],
            1,
            Some(StorageLoadFailureKind::Corrupt),
        ),
        (
            3,
            vec![event2.clone()],
            2,
            Some(StorageLoadFailureKind::Corrupt),
        ),
        (
            4,
            vec![event3.clone()],
            9,
            Some(StorageLoadFailureKind::Unsupported),
        ),
    ] {
        let result = super::validate_schema_tuple(&super::SchemaTuple::finalized(
            metadata,
            EventFacts::from_events(&events),
            marker,
        ));
        match expected {
            None => result.unwrap(),
            Some(expected) => assert_eq!(storage_load_failure_kind(&result.unwrap_err()), expected),
        }
    }
}

#[test]
fn staging_cleanup_keeps_a_sole_link_regular_replacement() {
    use std::os::unix::fs::MetadataExt as _;

    let temp = TestDir::new("staging-sole-link-replacement");
    let directory = open_directory(temp.path()).unwrap();
    let mut staged = create_named_staging(&directory).unwrap();
    let original = staged.file.metadata().unwrap();
    let staging_path = temp.path().join(crate::model::bytes(
        staged.name.as_ref().unwrap().to_bytes(),
    ));
    fs::remove_file(&staging_path).unwrap();

    // The replacement owns only this link: survival cannot be inferred from an
    // independently hard-linked sentinel after cleanup removes the observed name.
    let payload = b"sole-link replacement must remain";
    let mut replacement = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staging_path)
        .unwrap();
    replacement.write_all(payload).unwrap();
    drop(replacement);
    let replacement_identity = fs::symlink_metadata(&staging_path).unwrap();
    assert!(replacement_identity.file_type().is_file());
    assert_eq!(replacement_identity.nlink(), 1);
    assert_ne!(
        (original.dev(), original.ino()),
        (replacement_identity.dev(), replacement_identity.ino())
    );

    let error = staged
        .cleanup()
        .expect_err("sole-link replacement is not the owned staging inode");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(staged.name.is_some());
    assert_eq!(fs::read(&staging_path).unwrap(), payload);
    drop(staged);

    let preserved = fs::symlink_metadata(&staging_path).unwrap();
    assert!(preserved.file_type().is_file());
    assert_eq!(preserved.nlink(), 1);
    assert_eq!(
        (preserved.dev(), preserved.ino()),
        (replacement_identity.dev(), replacement_identity.ino())
    );
    assert_eq!(fs::read(&staging_path).unwrap(), payload);
}

#[test]
fn publish_failure_retains_primary_io_error_and_secondary_cleanup_failure() {
    let temp = TestDir::new("publish-cleanup-secondary");
    let directory = open_directory(temp.path()).unwrap();
    fs::write(temp.path().join("payload"), b"old destination").unwrap();
    let staged = create_named_staging(&directory).unwrap();
    let temporary = temp.path().join(crate::model::bytes(
        staged.name.as_ref().unwrap().to_bytes(),
    ));
    fs::remove_file(&temporary).unwrap();
    fs::write(&temporary, b"replacement must remain").unwrap();
    let failure = AtomicStaging {
        staged,
        published: CString::new("payload").unwrap(),
        display: "payload",
        control: AtomicControl {
            fail: Some(("payload", AtomicStep::FileSync)),
            force_named_fallback: true,
            force_link_unsupported: false,
        },
    }
    .publish()
    .err()
    .expect("the publication fault must remain an error");
    assert_eq!(failure.stage, CommitStage::BeforePublish);
    assert_eq!(failure.operation, "sync temporary file");
    assert_eq!(failure.source.raw_os_error(), Some(libc::EIO));
    let cleanup = failure.cleanup.as_ref().unwrap();
    assert_eq!(cleanup.source.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(
        cleanup.temporary,
        temporary.file_name().unwrap().to_string_lossy()
    );
    assert_eq!(
        fs::read(temp.path().join("payload")).unwrap(),
        b"old destination"
    );
    assert_eq!(fs::read(&temporary).unwrap(), b"replacement must remain");
    let diagnostic = failure.to_string();
    assert!(diagnostic.contains("Input/output error"), "{diagnostic}");
    assert!(
        diagnostic.contains("temporary cleanup failed"),
        "secondary cleanup error was lost: {diagnostic}"
    );
    assert!(
        diagnostic.contains("directory entry no longer names the opened inode"),
        "{diagnostic}"
    );
}

#[test]
fn failed_staging_write_cleans_owned_name_and_keeps_primary_errno() {
    let temp = TestDir::new("write-abort-owned-staging");
    let directory = open_directory(temp.path()).unwrap();
    let mut staged = create_named_staging(&directory).unwrap();
    let temporary = temp.path().join(crate::model::bytes(
        staged.name.as_ref().unwrap().to_bytes(),
    ));
    // The retained fd still identifies the owned inode; write through a read-only
    // handle fails independently of any test-control checkpoint.
    staged.file = fs::File::open(&temporary).unwrap();
    let mut atomic = AtomicStaging {
        staged,
        published: CString::new("payload").unwrap(),
        display: "payload",
        control: AtomicControl::default(),
    };
    let failure = atomic.write_all(b"unpublishable").unwrap_err();
    assert_eq!(failure.source.raw_os_error(), Some(libc::EBADF));
    assert_eq!(failure.stage, CommitStage::BeforePublish);
    assert!(failure.cleanup.is_none());
    assert!(atomic.staged.name.is_none());
    assert!(!temporary.exists());
    assert!(!temp.path().join("payload").exists());
    drop(atomic);
    assert!(staging_entries(temp.path()).is_empty());
}

#[test]
fn event_serialization_failure_reports_cleanup_without_reclassifying_primary() {
    let temp = TestDir::new("events-cleanup-secondary");
    let directory = open_directory(temp.path()).unwrap();
    let mut events = super::EventJsonlStaging::begin(
        &directory,
        AtomicControl {
            fail: None,
            force_named_fallback: true,
            force_link_unsupported: false,
        },
    )
    .unwrap();
    let temporary = temp.path().join(crate::model::bytes(
        events.atomic.staged.name.as_ref().unwrap().to_bytes(),
    ));
    fs::remove_file(&temporary).unwrap();
    fs::write(&temporary, b"replacement must remain").unwrap();
    let mut event = event_fixture(2);
    event.path = Some("x".repeat(super::MAX_EVENT_LINE_BYTES));
    let failure = events.push(&event).unwrap_err();
    assert_eq!(
        storage_load_failure_kind(&failure),
        StorageLoadFailureKind::Unavailable
    );
    let diagnostic = format!("{failure:#}");
    assert!(diagnostic.contains("bounded serialization"), "{diagnostic}");
    assert!(
        diagnostic.contains("temporary cleanup failed"),
        "secondary cleanup error was lost: {diagnostic}"
    );
    assert!(
        diagnostic.contains("directory entry no longer names the opened inode"),
        "{diagnostic}"
    );
    assert!(!temp.path().join("events.jsonl").exists());
    drop(events);
    assert_eq!(fs::read(&temporary).unwrap(), b"replacement must remain");
}

#[test]
fn staging_cleanup_reports_linked_inode_after_removing_its_owned_name() {
    let temp = TestDir::new("staging-cleanup-linked-inode");
    let directory = open_directory(temp.path()).unwrap();
    let mut staged = create_named_staging(&directory).unwrap();
    let temporary = temp.path().join(crate::model::bytes(
        staged.name.as_ref().unwrap().to_bytes(),
    ));
    staged.file.write_all(b"externally retained inode").unwrap();
    let alias = temp.path().join("external-alias");
    fs::hard_link(&temporary, &alias).unwrap();
    let failure = staged
        .cleanup()
        .expect_err("the owned inode remains linked after name removal");
    assert_eq!(failure.kind(), std::io::ErrorKind::InvalidData);
    assert!(failure.to_string().contains("remains linked after removal"));
    assert!(!temporary.exists(), "the real unlink already occurred");
    assert_eq!(file_stat(&staged.file).unwrap().st_nlink, 1);
    assert_eq!(fs::read(&alias).unwrap(), b"externally retained inode");
    // Absence of the old name makes Drop's later retry a no-op. The reported
    // failed postcondition must not be read as proof that this name survives.
    drop(staged);
    assert_eq!(fs::read(&alias).unwrap(), b"externally retained inode");
}
