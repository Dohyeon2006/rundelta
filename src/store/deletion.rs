//! Explicit deletion isolation and recovery, anchored to storage descriptors.
//!
//! Moving `runs/id` to `deleted/ticket/record` is the logical commit. Evidence
//! is never unlinked before that move and the post-move identity check. Purge
//! writes a durable-in-this-process intent before its first irreversible unlink;
//! restore refuses that intent rather than pretending partially removed bytes
//! can be rolled back. Neither rename nor fsync authenticates another same-UID
//! writer, freezes a directory tree, or guarantees power-loss durability.

use super::*;

/// One restart-visible deletion slot, including unsafe or incomplete slots.
#[derive(Debug, Serialize)]
pub(crate) struct DeletedRecord {
    /// Exact ticket spelling; a label is never accepted by recovery commands.
    pub(crate) ticket: String,
    /// Original record ID when the receipt is readable and supported.
    pub(crate) id: Option<String>,
    /// Original observed label, which is not an authenticated identity.
    pub(crate) name: Option<String>,
    /// `isolated`, `purge_started`, `no_record`, or `damaged`.
    pub(crate) state: &'static str,
    /// Reason a slot cannot be safely classified, if any.
    pub(crate) error: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema_version: u32,
    ticket: String,
    id: String,
    name: Option<String>,
    device: u64,
    inode: u64,
    slot_device: u64,
    slot_inode: u64,
}

struct Slot {
    directory: File,
    component: CString,
    receipt: Receipt,
}

fn component(value: &str) -> Result<CString> {
    if !valid_id(value) {
        bail!("deletion recovery requires one exact safe ticket ID");
    }
    Ok(CString::new(value).expect("safe IDs contain no NUL"))
}

/// Enforce the private layout without changing an existing directory's policy.
///
/// This excludes ordinary other-UID access; it is not an exclusion boundary
/// against the owner UID, privileged administrators or later owner mutations.
fn require_private_directory(directory: &File) -> Result<()> {
    let status = file_stat(directory)
        .map_err(|error| unavailable_io("inspect private deletion directory", error))?;
    // SAFETY: geteuid returns this process's effective credential without
    // dereferencing a pointer or changing credentials/global security policy.
    let owner = unsafe { libc::geteuid() };
    if status.st_uid != owner || status.st_mode & 0o077 != 0 {
        bail!(
            "deletion directory must be private and owned by the effective UID; existing permissions were not changed"
        );
    }
    Ok(())
}

fn deleted_directory(storage: &StorageRoot, create: bool) -> Result<Option<File>> {
    let directory = match openat_beneath(
        &storage.root_dir,
        c"deleted",
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        0,
    ) {
        Ok(directory) => Ok(Some(directory)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !create => Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match create_directory_at(&storage.root_dir, c"deleted", 0o700) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(unavailable_io("create deletion isolation directory", error));
                }
            }
            let directory = openat_beneath(
                &storage.root_dir,
                c"deleted",
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                0,
            )
            .map_err(|error| unavailable_io("open deletion isolation directory", error))?;
            Ok(Some(directory))
        }
        Err(error) => Err(unavailable_io("open deletion isolation directory", error)),
    }?;
    if let Some(directory) = &directory {
        require_private_directory(directory)?;
    }
    Ok(directory)
}

/// NOREPLACE is essential: neither isolation nor restore may overwrite a name.
///
/// An unavailable kernel operation is an error. Falling back to a separate
/// absence check and replacing rename would reopen a destructive race.
fn move_exclusive(old: &File, name: &CStr, new: &File, destination: &CStr) -> std::io::Result<()> {
    // SAFETY: all descriptors and C strings are live; renameat2 retains none.
    let result = unsafe {
        libc::renameat2(
            old.as_raw_fd(),
            name.as_ptr(),
            new.as_raw_fd(),
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn write_new(directory: &File, name: &CStr, bytes: &[u8]) -> Result<()> {
    let mut file = openat_beneath(
        directory,
        name,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC,
        0o600,
    )
    .map_err(|error| unavailable_io("create deletion receipt", error))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .and_then(|()| directory.sync_all())
        .map_err(|error| {
            unavailable_io(
                "write/sync deletion receipt; no evidence cleanup started",
                error,
            )
        })?;
    Ok(())
}

fn read_receipt(directory: &File, ticket: &str) -> Result<Receipt> {
    let bytes = read_file_at(directory, c"receipt.json", MAX_METADATA_BYTES)
        .map_err(|error| unavailable_io("read deletion receipt", error))?;
    let receipt: Receipt = serde_json::from_slice(&bytes)
        .map_err(|error| unavailable(format!("damaged deletion receipt: {error}")))?;
    let actual =
        file_stat(directory).map_err(|error| unavailable_io("inspect deletion slot", error))?;
    if receipt.schema_version != 1
        || receipt.ticket != ticket
        || !valid_id(&receipt.id)
        || receipt.slot_device != actual.st_dev
        || receipt.slot_inode != actual.st_ino
    {
        bail!("unsupported or inconsistent deletion receipt; no automatic recovery");
    }
    Ok(receipt)
}

fn selected_slot(deleted: &File, ticket: &str) -> Result<Slot> {
    let name = component(ticket)?;
    let directory = openat_beneath(
        deleted,
        &name,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        0,
    )
    .map_err(|error| unavailable_io(format!("open deletion ticket {ticket}"), error))?;
    require_private_directory(&directory)?;
    ensure_entry_identity(deleted, &name, &directory)
        .map_err(|error| unavailable_io("deletion slot changed while opening", error))?;
    let receipt = read_receipt(&directory, ticket)?;
    Ok(Slot {
        directory,
        component: name,
        receipt,
    })
}

fn record(slot: &Slot) -> Result<Option<File>> {
    let file = match openat_beneath(
        &slot.directory,
        c"record",
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        0,
    ) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(unavailable_io("open isolated record", error)),
    };
    let actual =
        file_stat(&file).map_err(|error| unavailable_io("inspect isolated record", error))?;
    if actual.st_dev != slot.receipt.device || actual.st_ino != slot.receipt.inode {
        bail!(
            "isolated record identity differs from deletion receipt; result unknown, no cleanup or restore"
        );
    }
    ensure_entry_identity(&slot.directory, c"record", &file)
        .map_err(|error| unavailable_io("isolated record name changed", error))?;
    Ok(Some(file))
}

fn purge_started(directory: &File) -> Result<bool> {
    match read_file_at(directory, c"purge-started", 32) {
        Ok(bytes) if bytes == b"purge-v1\n" => Ok(true),
        Ok(_) => bail!("damaged purge intent; cannot infer whether cleanup started"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(unavailable_io("read purge intent", error)),
    }
}

/// Freeze cleanup intent before removal; even an existing marker is re-synced.
///
/// A write or sync failure stops before unlink. A malformed marker remains
/// enumerable and is not silently replaced by a new claim about earlier work.
fn mark_purge(directory: &File) -> Result<()> {
    if !purge_started(directory)? {
        write_new(directory, c"purge-started", b"purge-v1\n")?;
    }
    let marker = openat_beneath(
        directory,
        c"purge-started",
        libc::O_RDONLY | libc::O_CLOEXEC,
        0,
    )
    .map_err(|error| unavailable_io("open purge intent for sync", error))?;
    marker
        .sync_all()
        .and_then(|()| directory.sync_all())
        .map_err(|error| unavailable_io("sync purge intent; no evidence cleanup started", error))?;
    Ok(())
}

/// Remove protocol bookkeeping only after the isolated record name is absent.
///
/// A failure here can leave a damaged/no-record slot, but cannot bring a partial
/// record back into the active namespace. Unknown slot children are retained.
fn finish_slot(deleted: &File, slot: &Slot) -> Result<()> {
    for name in directory_names(&slot.directory)
        .map_err(|error| unavailable_io("enumerate completed deletion slot", error))?
    {
        if !matches!(name.as_slice(), b"receipt.json" | b"purge-started") {
            bail!("deletion slot contains an unexpected entry; retain it for diagnosis");
        }
    }
    ensure_entry_identity(deleted, &slot.component, &slot.directory)
        .map_err(|error| unavailable_io("completed deletion slot identity changed", error))?;
    match unlink_at(&slot.directory, c"purge-started", 0) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(unavailable_io("remove completed purge intent", error)),
    }
    unlink_at(&slot.directory, c"receipt.json", 0)
        .map_err(|error| unavailable_io("remove completed deletion receipt", error))?;
    unlink_opened_directory(deleted, &slot.component, &slot.directory)
        .and_then(|()| deleted.sync_all())
        .map_err(|error| {
            unavailable_io(
                "remove/sync completed deletion slot; record already absent",
                error,
            )
        })?;
    Ok(())
}

fn purge_slot(deleted: &File, slot: &Slot) -> Result<()> {
    if let Some(run) = record(slot)? {
        walk_delete_tree(&run, 0, DeletePass::Preflight).map_err(|error| {
            unavailable_io("isolated cleanup preflight failed; no new unlink", error)
        })?;
        mark_purge(&slot.directory)?;
        ensure_entry_identity(deleted, &slot.component, &slot.directory)
            .map_err(|error| unavailable_io("deletion slot changed before cleanup", error))?;
        ensure_entry_identity(&slot.directory, c"record", &run)
            .map_err(|error| unavailable_io("isolated record changed before cleanup", error))?;
        walk_delete_tree(&run, 0, DeletePass::Remove)
            .and_then(|()| unlink_opened_directory(&slot.directory, c"record", &run))
            .map_err(|error| {
                unavailable_io(
                    "removal started; record may be partially deleted inside isolation",
                    error,
                )
            })?;
    }
    finish_slot(deleted, slot)
}

/// Move one previously resolved record before granting any physical cleanup.
fn isolate_record(storage: &StorageRoot, entry: Entry) -> Result<(String, File, Slot)> {
    let id = entry
        .id
        .ok_or_else(|| unavailable("unsafe record directory"))?;
    let run = entry
        .run_dir
        .ok_or_else(|| unavailable(format!("unsafe record directory: {}", entry.directory)))?;
    let source = component(&id)?;
    ensure_entry_identity(&storage.runs_dir, &source, &run).map_err(|error| {
        unavailable_io(
            format!("unsafe record directory: {}", entry.directory),
            error,
        )
    })?;
    // Retain the existing fail-before-unlink depth/access boundary for delete.
    walk_delete_tree(&run, 0, DeletePass::Preflight).map_err(|error| {
        unavailable_io(
            format!(
                "delete {} failed before removal during preflight",
                entry.directory
            ),
            error,
        )
    })?;
    let deleted = deleted_directory(storage, true)?.expect("creation requested");
    let ticket = format!(
        "{:x}-{}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
        std::process::id()
    );
    let ticket_name = component(&ticket)?;
    create_directory_at(&deleted, &ticket_name, 0o700)
        .map_err(|error| unavailable_io("prepare deletion slot; active record not moved", error))?;
    let directory = openat_beneath(
        &deleted,
        &ticket_name,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        0,
    )
    .map_err(|error| {
        unavailable_io(
            "open prepared deletion slot; active record not moved",
            error,
        )
    })?;
    let inode =
        file_stat(&run).map_err(|error| unavailable_io("inspect selected record", error))?;
    let slot_inode = file_stat(&directory)
        .map_err(|error| unavailable_io("inspect prepared deletion slot", error))?;
    let receipt = Receipt {
        schema_version: 1,
        ticket: ticket.clone(),
        id: id.clone(),
        name: entry.name,
        device: inode.st_dev,
        inode: inode.st_ino,
        slot_device: slot_inode.st_dev,
        slot_inode: slot_inode.st_ino,
    };
    // Labels already use the metadata budget. Reusing the smaller marker cap
    // would make an existing large-label record impossible to delete by ID/label.
    let bytes = serialize_bounded(&receipt, MAX_METADATA_BYTES, "deletion receipt", true)?;
    write_new(&directory, c"receipt.json", &bytes).with_context(|| {
        format!("delete {id} failed before isolation; prepared ticket {ticket}, no evidence unlink")
    })?;
    let slot = Slot {
        directory,
        component: ticket_name,
        receipt,
    };
    ensure_entry_identity(&storage.root_dir, c"deleted", &deleted)
        .and_then(|()| ensure_entry_identity(&deleted, &slot.component, &slot.directory))
        .and_then(|()| ensure_entry_identity(&storage.runs_dir, &source, &run))
        .and_then(|()| move_exclusive(&storage.runs_dir, &source, &slot.directory, c"record"))
        .map_err(|error| {
            unavailable_io(
                format!("delete {id} isolation failed; ticket {ticket}, no evidence unlink"),
                error,
            )
        })?;
    // Rename is the visibility commit. Failure afterward never triggers an
    // automatic rollback or cleanup of a mismatched replacement inode.
    ensure_entry_identity(&slot.directory, c"record", &run)
        .map_err(|error| unavailable_io(format!("delete {id} isolation identity changed; result unknown; inspect ticket {ticket}, no evidence unlink"), error))?;
    storage.runs_dir.sync_all().and_then(|()| slot.directory.sync_all()).and_then(|()| deleted.sync_all())
        .map_err(|error| unavailable_io(format!("delete {id} logically committed at ticket {ticket}; isolation sync failed; no evidence unlink"), error))?;
    Ok((id, deleted, slot))
}

/// Isolate, then finish the physical deletion requested by the original command.
pub(super) fn isolate(storage: &StorageRoot, entry: Entry) -> Result<String> {
    let (id, deleted, slot) = isolate_record(storage, entry)?;
    purge_slot(&deleted, &slot).with_context(|| format!(
        "delete {id} logically committed; cleanup failed; inspect deleted ticket {}; no automatic restore", slot.receipt.ticket))?;
    Ok(id)
}

/// Enumerate restart-visible deletion state without creating or repairing it.
pub(crate) fn list(root: &Path) -> Result<Vec<DeletedRecord>> {
    let Some(storage) = open_existing(root)? else {
        return Ok(vec![]);
    };
    let Some(deleted) = deleted_directory(&storage, false)? else {
        return Ok(vec![]);
    };
    let mut names = directory_names(&deleted)
        .map_err(|error| unavailable_io("enumerate deletion slots", error))?;
    names.sort();
    let mut rows = Vec::new();
    for name in names {
        let ticket = crate::model::bytes(&name);
        let mut row = DeletedRecord {
            ticket: ticket.clone(),
            id: None,
            name: None,
            state: "damaged",
            error: None,
        };
        let status = (|| -> Result<&'static str> {
            let slot = selected_slot(&deleted, &ticket)?;
            row.id = Some(slot.receipt.id.clone());
            row.name = slot.receipt.name.clone();
            let started = purge_started(&slot.directory)?;
            Ok(if record(&slot)?.is_none() {
                "no_record"
            } else if started {
                "purge_started"
            } else {
                "isolated"
            })
        })();
        match status {
            Ok(state) => row.state = state,
            Err(error) => row.error = Some(format!("{error:#}")),
        }
        rows.push(row);
    }
    Ok(rows)
}

/// Continue physical cleanup only for an explicitly selected valid ticket.
pub(crate) fn purge(root: &Path, ticket: &str) -> Result<String> {
    let storage = open(root)?;
    let _lock = storage.lock()?;
    let deleted = deleted_directory(&storage, false)?
        .ok_or_else(|| unavailable("no deletion isolation directory"))?;
    let slot = selected_slot(&deleted, ticket)?;
    purge_slot(&deleted, &slot).with_context(|| {
        format!("purge ticket {ticket} failed; remaining state retained, cleanup may be partial")
    })?;
    Ok(slot.receipt.id)
}

/// Restore an unpurged inode without overwriting an active ID or known label.
///
/// This returns stored bytes, including original damage, without manufacturing
/// finalization or strengthening integrity. A same-UID writer can forge/remove
/// receipts; this is a cooperative recovery protocol, not origin authentication.
pub(crate) fn restore(root: &Path, ticket: &str) -> Result<String> {
    let storage = open(root)?;
    let _lock = storage.lock()?;
    let deleted = deleted_directory(&storage, false)?
        .ok_or_else(|| unavailable("no deletion isolation directory"))?;
    let slot = selected_slot(&deleted, ticket)?;
    if purge_started(&slot.directory)? {
        bail!(
            "restore refused: purge intent exists; cleanup may have started; saved bytes may be incomplete"
        );
    }
    let run = record(&slot)?.ok_or_else(|| {
        unavailable("restore refused: isolated record is absent; commit history cannot be inferred")
    })?;
    let identities = storage.identities()?;
    reject_name_collision(&identities, &slot.receipt.id)?;
    if let Some(name) = &slot.receipt.name {
        reject_name_collision(&identities, name)?;
    }
    let destination = component(&slot.receipt.id)?;
    ensure_entry_identity(&deleted, &slot.component, &slot.directory)
        .and_then(|()| ensure_entry_identity(&slot.directory, c"record", &run))
        .and_then(|()| move_exclusive(&slot.directory, c"record", &storage.runs_dir, &destination))
        .map_err(|error| {
            unavailable_io(
                format!("restore ticket {ticket} failed before rename; no cleanup"),
                error,
            )
        })?;
    ensure_entry_identity(&storage.runs_dir, &destination, &run).map_err(|error| {
        unavailable_io(
            format!("restore ticket {ticket} identity changed; result unknown"),
            error,
        )
    })?;
    storage.runs_dir.sync_all().and_then(|()| slot.directory.sync_all())
        .map_err(|error| unavailable_io(format!("record {} restored by rename; sync failed; inspect active ID and ticket {ticket}", slot.receipt.id), error))?;
    finish_slot(&deleted, &slot).with_context(|| {
        format!(
            "record {} restored; bookkeeping cleanup failed at ticket {ticket}",
            slot.receipt.id
        )
    })?;
    Ok(slot.receipt.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "rundelta-deletion-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            let storage = open(&path).unwrap();
            fs::create_dir(path.join("runs/abc-1")).unwrap();
            fs::write(path.join("runs/abc-1/evidence"), b"original raw bytes").unwrap();
            drop(storage);
            Self(path)
        }
        fn isolated(&self) -> String {
            let storage = open(&self.0).unwrap();
            let _lock = storage.lock().unwrap();
            let (_, _, slot) = isolate_record(&storage, storage.resolve("abc-1").unwrap()).unwrap();
            slot.receipt.ticket
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn nonprivate_isolation_names_are_rejected_without_chmod_or_unlink() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        let deleted = fixture.0.join("deleted");
        fs::create_dir(&deleted).unwrap();
        fs::set_permissions(&deleted, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(delete(&fixture.0, "abc-1").is_err());
        assert_eq!(
            fs::read(fixture.0.join("runs/abc-1/evidence")).unwrap(),
            b"original raw bytes"
        );
        assert_eq!(
            fs::metadata(&deleted).unwrap().permissions().mode() & 0o777,
            0o777
        );
        fs::set_permissions(&deleted, fs::Permissions::from_mode(0o700)).unwrap();
        let ticket = fixture.isolated();
        let slot = deleted.join(&ticket);
        fs::set_permissions(&slot, fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(list(&fixture.0).unwrap()[0].state, "damaged");
        assert!(restore(&fixture.0, &ticket).is_err());
        assert!(purge(&fixture.0, &ticket).is_err());
        assert_eq!(
            fs::read(slot.join("record/evidence")).unwrap(),
            b"original raw bytes"
        );
        assert_eq!(
            fs::metadata(&slot).unwrap().permissions().mode() & 0o777,
            0o777
        );
    }

    #[test]
    fn existing_large_label_budget_does_not_gain_a_smaller_deletion_limit() {
        let fixture = Fixture::new();
        let label = "x".repeat(70_000);
        let mut metadata = crate::model::contract_tests::completed_metadata_fixture("abc-1");
        metadata.id = "abc-1".into();
        metadata.name = label.clone();
        fs::write(
            fixture.0.join("runs/abc-1/metadata.json"),
            serde_json::to_vec(&metadata).unwrap(),
        )
        .unwrap();
        assert_eq!(delete(&fixture.0, &label).unwrap(), "abc-1");
        assert!(list(&fixture.0).unwrap().is_empty());
    }

    #[test]
    fn isolation_restart_restore_preserves_original_damage_and_bytes() {
        let fixture = Fixture::new();
        assert!(list(&fixture.0).unwrap().is_empty());
        let ticket = fixture.isolated();
        assert!(!fixture.0.join("runs/abc-1").exists());
        let rows = list(&fixture.0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].state, "isolated");
        assert_eq!(restore(&fixture.0, &ticket).unwrap(), "abc-1");
        assert_eq!(
            fs::read(fixture.0.join("runs/abc-1/evidence")).unwrap(),
            b"original raw bytes"
        );
        assert!(!fixture.0.join("runs/abc-1/finalized.json").exists());
        assert!(list(&fixture.0).unwrap().is_empty());
    }

    #[test]
    fn restore_conflict_cannot_overwrite_active_id_or_take_a_path() {
        let fixture = Fixture::new();
        let ticket = fixture.isolated();
        fs::create_dir(fixture.0.join("runs/abc-1")).unwrap();
        fs::write(fixture.0.join("runs/abc-1/new-evidence"), b"new active").unwrap();
        let error = restore(&fixture.0, &ticket).unwrap_err();
        assert!(format!("{error:#}").contains("already exists"));
        assert_eq!(
            fs::read(fixture.0.join("runs/abc-1/new-evidence")).unwrap(),
            b"new active"
        );
        assert_eq!(list(&fixture.0).unwrap()[0].state, "isolated");
        for value in ["../abc-1", "victim", "", "/absolute"] {
            assert!(restore(&fixture.0, value).is_err());
            assert!(purge(&fixture.0, value).is_err());
        }
    }

    #[test]
    fn purge_intent_forbids_restore_even_if_no_unlink_has_happened() {
        let fixture = Fixture::new();
        let ticket = fixture.isolated();
        let storage = open(&fixture.0).unwrap();
        let deleted = deleted_directory(&storage, false).unwrap().unwrap();
        let slot = selected_slot(&deleted, &ticket).unwrap();
        mark_purge(&slot.directory).unwrap();
        assert_eq!(list(&fixture.0).unwrap()[0].state, "purge_started");
        assert!(
            restore(&fixture.0, &ticket)
                .unwrap_err()
                .to_string()
                .contains("cleanup may have started")
        );
        assert!(
            fixture
                .0
                .join("deleted")
                .join(&ticket)
                .join("record/evidence")
                .is_file()
        );
        assert_eq!(purge(&fixture.0, &ticket).unwrap(), "abc-1");
        assert!(list(&fixture.0).unwrap().is_empty());
    }

    #[test]
    fn malformed_intent_receipt_and_replacement_are_enumerable_and_fail_closed() {
        for damage in ["intent", "receipt", "replacement"] {
            let fixture = Fixture::new();
            let ticket = fixture.isolated();
            let directory = fixture.0.join("deleted").join(&ticket);
            match damage {
                "intent" => {
                    fs::write(directory.join("purge-started"), b"uncertain partial write").unwrap()
                }
                "receipt" => fs::write(directory.join("receipt.json"), b"{}").unwrap(),
                _ => {
                    fs::rename(directory.join("record"), directory.join("held-original")).unwrap();
                    fs::create_dir(directory.join("record")).unwrap();
                    fs::write(directory.join("record/foreign"), b"do not delete").unwrap();
                }
            }
            assert_eq!(list(&fixture.0).unwrap()[0].state, "damaged");
            assert!(restore(&fixture.0, &ticket).is_err());
            assert!(purge(&fixture.0, &ticket).is_err());
            if damage == "replacement" {
                assert_eq!(
                    fs::read(directory.join("record/foreign")).unwrap(),
                    b"do not delete"
                );
            }
        }
    }

    #[test]
    fn no_record_slot_is_not_claimed_restorable_and_explicit_purge_cleans_it() {
        let fixture = Fixture::new();
        let ticket = fixture.isolated();
        let directory = fixture.0.join("deleted").join(&ticket);
        fs::remove_dir_all(directory.join("record")).unwrap();
        assert_eq!(list(&fixture.0).unwrap()[0].state, "no_record");
        assert!(restore(&fixture.0, &ticket).is_err());
        assert_eq!(purge(&fixture.0, &ticket).unwrap(), "abc-1");
        assert!(list(&fixture.0).unwrap().is_empty());
    }
}
