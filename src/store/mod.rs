use crate::model::{Event, Metadata, Run, StorageIntegrity};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};
pub fn root(explicit: Option<PathBuf>) -> Result<PathBuf> {
    explicit
        .or_else(|| {
            std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .map(|p| p.join("rundelta"))
        })
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|p| p.join(".local/share/rundelta"))
        })
        .context("specify --storage: HOME and XDG_DATA_HOME are unset")
}
pub struct Lock(std::fs::File);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.sync_all();
    }
}
pub fn lock(root: &Path) -> Result<Lock> {
    use std::os::fd::AsRawFd;
    fs::create_dir_all(root.join("runs"))?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(root.join(".record.lock"))?;
    // Kernel releases the advisory lock even if the recorder is killed.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        bail!("storage busy: another recorder or delete operation holds the lock");
    }
    Ok(Lock(file))
}
pub fn metadata(dir: &Path, m: &Metadata) -> Result<()> {
    let data = serde_json::to_vec_pretty(m)?;
    let temp = dir.join("metadata.tmp");
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temp)?;
    f.write_all(&data)?;
    f.sync_all()?;
    fs::rename(temp, dir.join("metadata.json"))?;
    std::fs::File::open(dir)?.sync_all()?;
    Ok(())
}
/// Index data only: an unreadable label is empty, never fabricated metadata.
pub struct Identity {
    pub id: String,
    pub name: String,
}
struct Entry {
    dir: PathBuf,
    directory: String,
    id: Option<String>,
    name: Option<String>,
    metadata: Result<Metadata>,
}
fn valid_id(id: &str) -> bool {
    id.split_once('-').is_some_and(|(time, pid)| {
        !time.is_empty()
            && time.bytes().all(|b| b.is_ascii_hexdigit())
            && u128::from_str_radix(time, 16).is_ok()
            && !pid.is_empty()
            && pid.bytes().all(|b| b.is_ascii_digit())
            && pid.parse::<u32>().is_ok_and(|p| p > 0)
    })
}
fn read_file(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !f.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "not a regular file",
        ));
    }
    let mut bytes = vec![];
    f.read_to_end(&mut bytes)?;
    Ok(bytes)
}
fn decode_metadata(data: &[u8], id: &str) -> Result<Metadata> {
    let m: Metadata =
        serde_json::from_slice(data).map_err(|e| corrupt(format!("metadata.json: {e}")))?;
    if !matches!(m.schema_version, 1..=3) {
        bail!("unsupported schema {}", m.schema_version);
    }
    if m.schema_version >= 2 && (m.environment.is_none() || m.completeness.is_none()) {
        return Err(corrupt("metadata: missing environment or completeness"));
    }
    if m.id != id {
        return Err(corrupt("record ID does not match directory"));
    }
    Ok(m)
}
fn scan(root: &Path) -> Result<Vec<Entry>> {
    use std::os::unix::ffi::OsStrExt;
    let entries = match fs::read_dir(root.join("runs")) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e).context("enumerate record storage"),
    };
    let mut out = vec![];
    for entry in entries {
        let entry = entry.context("enumerate record directory entry")?;
        let dir = entry.path();
        let leaf = entry.file_name();
        let id = leaf.to_str().filter(|id| valid_id(id)).map(str::to_owned);
        let safe = entry.file_type().is_ok_and(|t| t.is_dir());
        let directory = format!("runs/{}", crate::model::bytes(leaf.as_bytes()));
        let mut name = None;
        let metadata = if safe && id.is_some() {
            read_file(&dir.join("metadata.json"))
                .map_err(|e| corrupt(format!("metadata.json: {e}")))
                .and_then(|data| {
                    // A label is only an index hint; it does not imply data integrity.
                    name = serde_json::from_slice::<serde_json::Value>(&data)
                        .ok()
                        .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(str::to_owned))
                        .filter(|n| !n.is_empty() && !n.chars().any(char::is_control));
                    decode_metadata(&data, id.as_deref().unwrap_or_default())
                })
        } else {
            Err(anyhow::anyhow!(
                "unsafe entry or unrecognizable run ID: {directory}"
            ))
        };
        out.push(Entry {
            dir,
            directory,
            id: if safe { id } else { None },
            name,
            metadata,
        });
    }
    out.sort_by(|a, b| a.directory.cmp(&b.directory));
    Ok(out)
}
/// Name/ID reservations for capture, independent of record validation.
pub fn all(root: &Path) -> Result<Vec<(PathBuf, Identity)>> {
    scan(root)?
        .into_iter()
        .map(|e| {
            let id = e.id.context(format!(
                "unsafe storage entry: {}; cannot reserve a new label",
                e.directory
            ))?;
            Ok((
                e.dir,
                Identity {
                    id,
                    name: e.name.unwrap_or_default(),
                },
            ))
        })
        .collect()
}
fn resolve(root: &Path, key: &str) -> Result<Entry> {
    let mut candidates: Vec<_> = scan(root)?
        .into_iter()
        .filter(|e| e.id.as_deref() == Some(key) || e.name.as_deref() == Some(key))
        .collect();
    if candidates.len() != 1 {
        bail!(
            "record {key:?}: expected one safe ID/label match, found {}; unknown labels require an exact recognizable directory ID; unsafe entries cannot be selected",
            candidates.len()
        );
    }
    Ok(candidates.remove(0))
}
pub fn delete(root: &Path, key: &str) -> Result<String> {
    let _lock = lock(root)?;
    let e = resolve(root, key)?;
    let id = e.id.context("unsafe record directory")?;
    // Recheck the selected leaf. Never follow a symlink or require valid metadata.
    if !fs::symlink_metadata(&e.dir)?.is_dir() {
        bail!("unsafe record directory: {}", e.directory);
    }
    fs::remove_dir_all(&e.dir).with_context(|| format!("delete {}", e.directory))?;
    Ok(id)
}

#[derive(Debug)]
pub struct Corrupt(pub String);
impl std::fmt::Display for Corrupt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "corrupt record: {}", self.0)
    }
}
impl std::error::Error for Corrupt {}
fn corrupt(message: impl Into<String>) -> anyhow::Error {
    Corrupt(message.into()).into()
}
pub fn is_corrupt(error: &anyhow::Error) -> bool {
    error.chain().any(|e| e.is::<Corrupt>())
}
#[derive(Serialize, Deserialize)]
struct FileCheck {
    bytes: u64,
    sha256: String,
}
#[derive(Serialize, Deserialize)]
struct Finalized {
    schema_version: u32,
    run_id: String,
    event_count: usize,
    events: FileCheck,
    metadata: FileCheck,
}
fn check(data: &[u8]) -> FileCheck {
    FileCheck {
        bytes: data.len() as u64,
        sha256: format!("{:x}", Sha256::digest(data)),
    }
}
fn verify(data: &[u8], expected: &FileCheck, file: &str) -> Result<()> {
    let actual = check(data);
    if actual.bytes != expected.bytes || actual.sha256 != expected.sha256 {
        return Err(corrupt(format!("{file}: byte count or SHA-256 mismatch")));
    }
    Ok(())
}
fn atomic_file(dir: &Path, name: &str, data: &[u8]) -> Result<()> {
    let temp = dir.join(format!("{name}.tmp"));
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temp)?;
    f.write_all(data)?;
    f.sync_all()?;
    fs::rename(temp, dir.join(name))?;
    fs::File::open(dir)?.sync_all()?;
    Ok(())
}
fn decode_events(data: &[u8]) -> Result<Vec<Event>> {
    let raw = std::str::from_utf8(data).map_err(|e| corrupt(format!("events.jsonl: {e}")))?;
    if !raw.is_empty() && !raw.ends_with('\n') {
        return Err(corrupt("events.jsonl: incomplete final line"));
    }
    let mut events = vec![];
    for (n, line) in raw.lines().enumerate() {
        let event: Event = serde_json::from_str(line)
            .map_err(|e| corrupt(format!("events.jsonl:{}: {e}", n + 1)))?;
        if !matches!(event.schema_version, 1 | 2) {
            bail!("unsupported event schema {}", event.schema_version);
        }
        events.push(event);
    }
    Ok(events)
}
/// Final marker is the last atomic write; it describes storage finalization, not capture success.
pub fn finish(dir: &Path, m: &Metadata, events: &[Event]) -> Result<()> {
    if m.state == "running" || m.ended.is_none() || m.parsed_events != events.len() {
        bail!("cannot finalize inconsistent metadata");
    }
    let mut data = vec![];
    for e in events {
        serde_json::to_writer(&mut data, e)?;
        data.push(b'\n');
    }
    atomic_file(dir, "events.jsonl", &data)?;
    let reread = fs::read(dir.join("events.jsonl"))?;
    if reread != data || decode_events(&reread)?.len() != m.parsed_events {
        bail!("event read-back verification failed");
    }
    metadata(dir, m)?;
    let meta = fs::read(dir.join("metadata.json"))?;
    if meta != serde_json::to_vec_pretty(m)? {
        bail!("metadata read-back verification failed");
    }
    let marker = Finalized {
        schema_version: 1,
        run_id: m.id.clone(),
        event_count: events.len(),
        events: check(&reread),
        metadata: check(&meta),
    };
    atomic_file(dir, "finalized.json", &serde_json::to_vec_pretty(&marker)?)?;
    Ok(())
}
pub fn load(root: &Path, key: &str) -> Result<Run> {
    let e = resolve(root, key)?;
    let m = e.metadata.with_context(|| e.directory.clone())?;
    read_run(e.dir, m).with_context(|| e.directory)
}
/// A damaged row never prevents listing another row. Preserve the flat metadata
/// fields for readable records; unreadable rows expose only index hints/status.
pub fn records(root: &Path) -> Result<Vec<serde_json::Value>> {
    scan(root)?
        .into_iter()
        .map(|e| {
            let (mut summary, integrity) = match e.metadata.and_then(|m| read_run(e.dir, m)) {
                Ok(run) => (serde_json::to_value(&run.metadata)?, run.storage_integrity),
                Err(error) => (
                    serde_json::json!({"id": e.id, "name": e.name}),
                    StorageIntegrity {
                        status: if is_corrupt(&error) {
                            "corrupt"
                        } else {
                            "unavailable"
                        }
                        .into(),
                        detail: format!("{error:#}"),
                    },
                ),
            };
            summary["directory"] = e.directory.into();
            summary["storage_integrity"] = serde_json::to_value(integrity)?;
            Ok(summary)
        })
        .collect()
}
fn read_run(d: PathBuf, mut m: Metadata) -> Result<Run> {
    let marker_path = d.join("finalized.json");
    let marker = match read_file(&marker_path) {
        Ok(data) => Some(
            serde_json::from_slice::<Finalized>(&data)
                .map_err(|e| corrupt(format!("finalized.json: {e}")))?,
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(corrupt(format!("finalized.json: {e}"))),
    };
    let (events, storage_integrity) = if let Some(marker) = marker {
        if marker.schema_version != 1 || marker.run_id != m.id {
            return Err(corrupt("invalid finalization marker version or run ID"));
        }
        let meta = read_file(&d.join("metadata.json"))
            .map_err(|e| corrupt(format!("metadata.json: {e}")))?;
        verify(&meta, &marker.metadata, "metadata.json")?;
        // Use the verified bytes, not the earlier directory-scan snapshot.
        m = decode_metadata(&meta, &marker.run_id)?;
        if m.state == "running" || m.ended.is_none() {
            return Err(corrupt("finalization marker contradicts run state"));
        }
        let data = read_file(&d.join("events.jsonl"))
            .map_err(|e| corrupt(format!("events.jsonl: {e}")))?;
        verify(&data, &marker.events, "events.jsonl")?;
        let events = decode_events(&data)?;
        if events.len() != marker.event_count || events.len() != m.parsed_events {
            return Err(corrupt("event count mismatch"));
        }
        (events,StorageIntegrity{status:"verified".into(),detail:"metadata and events match finalized byte counts, event count and SHA-256; raw logs are not checksummed".into()})
    } else if m.schema_version >= 3 || m.state == "running" {
        (vec![],StorageIntegrity{status:"unfinalized".into(),detail:"no final marker; capture may still be running or may have stopped before storage finalization".into()})
    } else {
        let data = read_file(&d.join("events.jsonl"))
            .map_err(|e| corrupt(format!("events.jsonl: {e}")))?;
        (
            decode_events(&data)?,
            StorageIntegrity {
                status: "unverified".into(),
                detail: "storage integrity unverified: legacy record has no finalization checksums"
                    .into(),
            },
        )
    };
    Ok(Run {
        metadata: m,
        events,
        storage_integrity,
    })
}

#[cfg(test)]
mod identity_tests {
    use super::valid_id;
    #[test]
    fn recovery_accepts_only_generated_id_shape() {
        for id in ["18d6e85e2b868e79-24369", "abc-1"] {
            assert!(valid_id(id));
        }
        for id in [
            "../abc-1",
            "/abc-1",
            "abc-1/child",
            "abc-0",
            "abc--1",
            "not-a-run-id",
            "",
            "abc-999999999999999",
            "abc-1\n",
        ] {
            assert!(!valid_id(id), "{id:?}");
        }
    }
}
