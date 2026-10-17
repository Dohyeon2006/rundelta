//! Descriptor-anchored record persistence and compatibility loading.
//!
//! [`StorageRoot`] owns open descriptors for both the configured root and its
//! `runs` directory. [`RunWriter`] additionally owns the storage lock and open
//! descriptors for one run, its `raw` directory, and its trace inode. Structural
//! operations stay relative to those descriptors so later path renames or swaps
//! cannot redirect an in-progress operation outside the opened storage tree.
//!
//! This module also owns record indexing, schema/contract validation, streaming
//! event verification, atomic publication, and legacy compatibility reads. A
//! default finalization marker binds structured metadata/events only. Opt-in
//! trace-v1 additionally binds a raw normalization-stream receipt; its raw bytes
//! are verified independently when loaded. Neither protocol authenticates origin,
//! proves capture success, nor guarantees that a rename survived a power loss.
//!
//! Selection, validation and publication are separate trust boundaries: an
//! index label helps locate an entry, supported wire facts are checked when it
//! is loaded, and the marker binds the selected protocol's structured bytes. Loading never asks
//! the parser to reconstruct or strengthen an old capture's observations.
//!
//! 中文阅读提示：目录 fd 是 I/O 的锚点，显示路径不是操作权限。锁只协调遵守本协议的
//! recorder/delete；它不阻止外部同权限进程改文件。当前验证覆盖格式版本、部分字段组合、
//! 计数、证据行范围、current 事件形状和字节摘要；trace-v1 单独核验 raw 字节，不核验
//! raw 中的 syscall 意义或归一化完整性，也不加强旧记录的事实。
//! 读取和写入遵守下面的容量/JSON 节点预算；索引名称数量仍随记录数增长。

pub(crate) mod deletion;

use crate::model::{
    ContractResult, ContractViolation, Event, EventFacts, LoadOutcome, Metadata, RawIntegrity,
    RawManifest, RawReceipt, RecordSummary, Run, StorageIntegrity, StorageLoadFailure,
    StorageLoadFailureKind, validate_metadata_contract, validate_run_contract_facts,
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::{CStr, CString, OsStr},
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    os::fd::{AsRawFd, FromRawFd},
    os::unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

/// Storage-operation budgets, not wire-schema limits or corruption evidence.
///
/// A multi-megabyte argv/environment remains usable, while one stored document
/// cannot demand unbounded DTO/container memory. The file/count budgets also
/// bound collect-mode growth; validate-only listing uses the same policy. These
/// limits do not promise recovery from every process-wide allocator/RLIMIT failure.
const MAX_METADATA_BYTES: usize = 8 * 1024 * 1024;
const MAX_MARKER_BYTES: usize = 64 * 1024;
const MAX_EVENT_LINE_BYTES: usize = 8 * 1024 * 1024;
const MAX_EVENT_FILE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_EVENT_COUNT: usize = 1_000_000;
const MAX_JSON_VALUES: usize = 65_536;
/// Bound both recursion and retained ancestor descriptors during deletion.
/// The record root is depth zero; preflight rejects excess before any unlink.
const MAX_DELETE_DEPTH: usize = 128;

fn resource_limit(display: &str, budget: &str, limit: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(format!(
        "{display}: resource limit exceeded ({budget}: {limit})"
    ))
}

/// Reserve explicit capacity fallibly, growing geometrically within a hard cap.
///
/// This avoids both infallible Vec growth and per-byte reallocations. Serde's
/// internal allocations are separately bounded by byte and JSON-value budgets;
/// its allocator failures cannot be intercepted by this helper.
fn reserve_bounded<T>(
    buffer: &mut Vec<T>,
    required: usize,
    limit: usize,
    display: &str,
) -> std::io::Result<()> {
    if required > limit {
        return Err(resource_limit(display, "capacity", limit));
    }
    if required > buffer.capacity() {
        let target = required.max(buffer.capacity().saturating_mul(2)).min(limit);
        buffer
            .try_reserve_exact(target - buffer.len())
            .map_err(|error| {
                std::io::Error::new(
                    std::io::ErrorKind::OutOfMemory,
                    format!("{display}: cannot reserve bounded buffer: {error}"),
                )
            })?;
    }
    Ok(())
}

fn append_bounded(
    buffer: &mut Vec<u8>,
    bytes: &[u8],
    limit: usize,
    display: &str,
) -> std::io::Result<()> {
    let required = buffer
        .len()
        .checked_add(bytes.len())
        .ok_or_else(|| resource_limit(display, "bytes", limit))?;
    reserve_bounded(buffer, required, limit, display)?;
    buffer.extend_from_slice(bytes);
    Ok(())
}

/// Count allocation-bearing JSON tokens without constructing a JSON tree.
///
/// Each string (including keys), scalar and container counts once. Escaped
/// punctuation inside strings does not count. This is a resource preflight,
/// not a syntax validator: serde still decides whether bounded input is valid.
fn check_json_budget(bytes: &[u8], display: &str) -> std::io::Result<()> {
    let mut values = 0usize;
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'"' => {
                values += 1;
                cursor += 1;
                while cursor < bytes.len() {
                    match bytes[cursor] {
                        b'\\' => cursor = cursor.saturating_add(2),
                        b'"' => {
                            cursor += 1;
                            break;
                        }
                        _ => cursor += 1,
                    }
                }
            }
            b'{' | b'[' => {
                values += 1;
                cursor += 1;
            }
            b'}' | b']' | b',' | b':' | b' ' | b'\n' | b'\r' | b'\t' => cursor += 1,
            _ => {
                values += 1;
                cursor += 1;
                while cursor < bytes.len()
                    && !matches!(
                        bytes[cursor],
                        b'{' | b'['
                            | b'}'
                            | b']'
                            | b','
                            | b':'
                            | b'"'
                            | b' '
                            | b'\n'
                            | b'\r'
                            | b'\t'
                    )
                {
                    cursor += 1;
                }
            }
        }
        if values > MAX_JSON_VALUES {
            return Err(resource_limit(display, "JSON values", MAX_JSON_VALUES));
        }
    }
    Ok(())
}

/// Read without read_to_end's implicit growth; recheck the cap during every chunk.
///
/// The initial stat rejects obvious excess but is not a concurrency snapshot.
/// A file growing after stat still cannot grow this owned buffer past the cap.
fn read_bounded(file: &mut File, limit: usize, display: &str) -> std::io::Result<Vec<u8>> {
    if stat_length(&file_stat(file)?)? > limit as u64 {
        return Err(resource_limit(display, "bytes", limit));
    }
    let mut bytes = Vec::new();
    let mut scratch = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut scratch)?;
        if read == 0 {
            return Ok(bytes);
        }
        append_bounded(&mut bytes, &scratch[..read], limit, display)?;
    }
}

/// Bound a line before copying from the fixed BufReader scratch buffer.
fn read_bounded_line(
    reader: &mut impl BufRead,
    line: &mut Vec<u8>,
    limit: usize,
) -> std::io::Result<usize> {
    line.clear();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(line.len());
        }
        let end = available.iter().position(|byte| *byte == b'\n');
        let take = end.map_or(available.len(), |index| index + 1);
        append_bounded(line, &available[..take], limit, "events.jsonl line")?;
        reader.consume(take);
        if end.is_some() {
            return Ok(line.len());
        }
    }
}

/// Fallible, capped serializer sink used before any destination is published.
struct BoundedJson {
    bytes: Vec<u8>,
    limit: usize,
    display: &'static str,
}

impl Write for BoundedJson {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        append_bounded(&mut self.bytes, bytes, self.limit, self.display)?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn serialize_bounded<T: Serialize>(
    value: &T,
    limit: usize,
    display: &'static str,
    pretty: bool,
) -> Result<Vec<u8>> {
    let mut output = BoundedJson {
        bytes: Vec::new(),
        limit,
        display,
    };
    let result = if pretty {
        serde_json::to_writer_pretty(&mut output, value)
    } else {
        serde_json::to_writer(&mut output, value)
    };
    result.map_err(|error| unavailable(format!("{display}: bounded serialization: {error}")))?;
    check_json_budget(&output.bytes, display).map_err(|error| unavailable_io(display, error))?;
    Ok(output.bytes)
}

/// Resolve the storage root selected by CLI input or the standard data paths.
///
/// This selects a spelling only. Opening that spelling and retaining directory
/// identity are the responsibility of [`StorageRoot`], not environment lookup.
pub(crate) fn root(explicit: Option<PathBuf>) -> Result<PathBuf> {
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
/// Advisory recorder/delete lock retained by its owned backing descriptor.
///
/// A stale lock-file name is harmless; exclusion comes from `flock` on the
/// opened inode. This coordinates cooperating operations and does not prevent
/// an external writer from changing entries or replacing the lock-file name.
/// The backing file stores no record payload. Explicit record writers own
/// durability; syncing an empty file cannot persist this kernel lock state.
pub(crate) struct Lock(std::fs::File);
impl Drop for Lock {
    fn drop(&mut self) {
        // Close also releases flock, but an explicit unlock makes the lock
        // available before any descriptor teardown work can overlap another
        // in-process façade attempt.
        // SAFETY: this Lock still owns the live descriptor; flock retains no
        // pointer and unlocking does not transfer descriptor ownership.
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// An opened storage root whose directory identity is stable for its lifetime.
///
/// All structural operations are relative to the held descriptors. The original
/// path is retained only to reproduce the existing user-facing evidence path.
/// Opening the root resolves its configured path once; later operations do not
/// reopen that spelling to recover authority. A renamed root may therefore keep
/// working while its retained display path no longer locates the same directory.
pub(crate) struct StorageRoot {
    root_dir: File,
    runs_dir: File,
    display_root: PathBuf,
}

/// A single in-progress record anchored to its storage, run and raw directories.
///
/// The lock and all descriptors live until this value is dropped, so capture
/// cannot accidentally continue through a path that has been renamed or swapped.
/// Initial metadata is written once; finalization consumes this writer, even on
/// failure. Neither retry nor concurrent finalization can reuse its authority.
/// The value grants one capture permission to publish one record; it does not
/// decide the target outcome or the meaning of capture completeness. Those facts
/// arrive from capture as metadata and are checked only against stored contracts.
pub(crate) struct RunWriter {
    storage: StorageRoot,
    _lock: Lock,
    run_dir: File,
    raw_dir: File,
    trace_file: File,
    id: String,
    name: String,
    evidence_dir: PathBuf,
    initial_metadata: InitialMetadata,
    /// Receipt of the last actual normalization traversal, never a later hash-only read.
    trace_receipt: Option<RawReceipt>,
    /// Initial metadata freezes whether normalization must produce a trace-v1 receipt.
    trace_contract: bool,
}

/// A failed initialization is terminal: publication may already have occurred.
///
/// `Failed` is set before I/O, so a rename followed by a sync error cannot be
/// mistaken for an untouched destination that is safe to initialize again.
#[derive(Clone, Copy, PartialEq, Eq)]
enum InitialMetadata {
    Unwritten,
    Failed,
    Written,
}

fn unsafe_directory(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(code) if code == libc::ELOOP || code == libc::ENOTDIR)
}

/// Open the configured directory without following its final path component.
///
/// Ancestor resolution still belongs to the OS at this opening boundary. Once
/// opened, callers use the descriptor rather than resolving this path again.
fn open_directory(path: &Path) -> std::io::Result<File> {
    // A trailing separator makes Linux resolve a leaf symlink as an ancestor
    // before O_NOFOLLOW is applied. Trim only those separators, preserving the
    // configured ancestor spelling and the special root/empty-path cases.
    let spelling = path.as_os_str().as_bytes();
    let mut end = spelling.len();
    while end > 1 && spelling[end - 1] == b'/' {
        end -= 1;
    }
    let path = Path::new(OsStr::from_bytes(&spelling[..end]));
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}

/// Transfer one newly returned syscall descriptor into Rust ownership.
fn owned_fd(fd: libc::c_int) -> std::io::Result<File> {
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        // SAFETY: every caller passes a newly returned owned descriptor and this
        // function performs its only ownership transfer.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

fn openat_basic(
    directory: &File,
    name: &CStr,
    flags: libc::c_int,
    mode: libc::mode_t,
) -> std::io::Result<File> {
    // SAFETY: `directory` and `name` remain live for the call. `openat` returns
    // a new descriptor, transferred exactly once by `owned_fd`.
    owned_fd(unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags, mode) })
}

/// Open one checked leaf beneath a held directory, rejecting path traversal.
///
/// `openat2` excludes symlinks and mount crossings in that lookup. The ENOSYS
/// fallback has the narrower guarantees described below; callers must retain
/// and recheck directory identity rather than assuming a path is immutable.
fn openat_beneath(
    directory: &File,
    name: &CStr,
    flags: libc::c_int,
    mode: libc::mode_t,
) -> std::io::Result<File> {
    let component = name.to_bytes();
    if component.is_empty() || component == b"." || component == b".." || component.contains(&b'/')
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "storage access requires exactly one ordinary path component",
        ));
    }
    // SAFETY: open_how consists only of kernel ABI integer fields; zero is the
    // documented default for fields added after the version known to libc.
    let mut how = unsafe { std::mem::zeroed::<libc::open_how>() };
    how.flags = flags as u64;
    how.mode = mode as u64;
    how.resolve = libc::RESOLVE_BENEATH
        | libc::RESOLVE_NO_MAGICLINKS
        | libc::RESOLVE_NO_SYMLINKS
        | libc::RESOLVE_NO_XDEV;
    // SAFETY: `how` has the kernel ABI layout supplied by libc and both pointers
    // remain valid for the duration of the syscall.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            directory.as_raw_fd(),
            name.as_ptr(),
            &how,
            std::mem::size_of::<libc::open_how>(),
        ) as libc::c_int
    };
    if fd >= 0 {
        return owned_fd(fd);
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ENOSYS) {
        // Linux 5.3-5.5 can satisfy the single-component contract with openat.
        // O_NOFOLLOW blocks leaf symlinks, and callers recheck opened directory
        // identities. Reject an obvious mount crossing when openat2's NO_XDEV
        // is unavailable. Same-device bind mounts remain a documented legacy-
        // kernel limitation rather than raising the existing pidfd baseline.
        let opened = openat_basic(
            directory,
            name,
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            mode,
        )?;
        if file_stat(directory)?.st_dev != file_stat(&opened)?.st_dev {
            return Err(std::io::Error::from_raw_os_error(libc::EXDEV));
        }
        return Ok(opened);
    }
    Err(error)
}

fn stat_at(directory: &File, name: &CStr) -> std::io::Result<libc::stat> {
    let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: the output points to writable storage and inputs stay live.
    let result = unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            status.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        // SAFETY: fstatat initialized the structure on success.
        Ok(unsafe { status.assume_init() })
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn file_stat(file: &File) -> std::io::Result<libc::stat> {
    let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `file` owns a live fd and the output points to writable storage.
    let result = unsafe { libc::fstat(file.as_raw_fd(), status.as_mut_ptr()) };
    if result == 0 {
        // SAFETY: fstat initialized the structure on success.
        Ok(unsafe { status.assume_init() })
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Compare opened storage inode identity within the current OS operation.
///
/// This is a traversal/publication check, not RunDelta's cross-run file identity
/// or content-equality contract, and it does not lock the file's contents.
fn same_file(left: &libc::stat, right: &libc::stat) -> bool {
    left.st_dev == right.st_dev && left.st_ino == right.st_ino
}

fn is_directory(status: &libc::stat) -> bool {
    status.st_mode & libc::S_IFMT == libc::S_IFDIR
}

fn is_regular(status: &libc::stat) -> bool {
    status.st_mode & libc::S_IFMT == libc::S_IFREG
}

fn stat_length(status: &libc::stat) -> std::io::Result<u64> {
    u64::try_from(status.st_size).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "regular file has a negative byte length",
        )
    })
}

/// Collect entry names as bytes while keeping descriptor use independent of count.
///
/// A separately opened directory stream avoids sharing the held directory's
/// cursor. Names are only candidates: each is inspected/opened later and may
/// change meanwhile. Enumeration failure must not masquerade as an empty index.
/// The returned name vector still grows with the directory's entry count.
fn directory_names(directory: &File) -> std::io::Result<Vec<Vec<u8>>> {
    let duplicate = openat_basic(
        directory,
        c".",
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        0,
    )?;
    let raw_fd = duplicate.as_raw_fd();
    // SAFETY: fdopendir takes ownership on success. We forget the File only in
    // that branch and close the resulting DIR exactly once below.
    let stream = unsafe { libc::fdopendir(raw_fd) };
    if stream.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    std::mem::forget(duplicate);
    let mut names = Vec::new();
    let result = loop {
        // SAFETY: Linux exposes thread-local errno at this address.
        unsafe { *libc::__errno_location() = 0 };
        // SAFETY: `stream` remains live until closed after this loop.
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            let error = std::io::Error::last_os_error();
            break if error.raw_os_error() == Some(0) {
                Ok(names)
            } else {
                Err(error)
            };
        }
        // SAFETY: d_name in a successful dirent is NUL-terminated.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name != b"." && name != b".." {
            names.push(name.to_vec());
        }
    };
    // SAFETY: `stream` is a live DIR and owns the duplicated fd.
    unsafe { libc::closedir(stream) };
    result
}

fn open_runs_directory(root: &File) -> std::io::Result<File> {
    openat_basic(
        root,
        c"runs",
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        0,
    )
}

fn create_runs_directory(root: &File) -> std::io::Result<()> {
    // SAFETY: `root` owns a live directory fd and the C string is static.
    let result = unsafe { libc::mkdirat(root.as_raw_fd(), c"runs".as_ptr(), 0o777) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn create_directory_at(parent: &File, name: &CStr, mode: libc::mode_t) -> std::io::Result<()> {
    // SAFETY: the parent fd and component remain live for the syscall.
    let result = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), mode) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Validate a user label for storage/display, without interpreting it as a path.
///
/// Slashes and path-looking labels are intentionally allowed: only generated
/// IDs may become directory components, after separate validation.
fn validate_record_name(name: &str) -> Result<()> {
    if name.is_empty() || name.chars().any(char::is_control) {
        bail!("name must be nonempty without control characters");
    }
    Ok(())
}

/// Reserve the shared exact-match namespace used for both labels and IDs.
fn reject_name_collision(identities: &[Identity], name: &str) -> Result<()> {
    if identities
        .iter()
        .any(|identity| identity.name == name || identity.id == name)
    {
        bail!("record name already exists: {name}");
    }
    Ok(())
}

impl StorageRoot {
    /// Anchor the root and `runs`, optionally creating a previously absent store.
    ///
    /// Read-only callers distinguish absence from an unsafe or unreadable tree;
    /// they must not create storage as a side effect of a load or listing.
    fn open_with(root: &Path, create: bool) -> Result<Option<Self>> {
        if create {
            fs::create_dir_all(root)
                .map_err(|error| unavailable_io("create storage root", error))?;
        }
        let root_dir = match open_directory(root) {
            Ok(directory) => directory,
            Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) if unsafe_directory(&error) => {
                return Err(unavailable(
                    "unsafe storage root: root is not a real directory",
                ));
            }
            Err(error) => return Err(unavailable_io("open storage root", error)),
        };
        let runs_dir = match open_runs_directory(&root_dir) {
            Ok(directory) => directory,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !create => {
                return Ok(None);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match create_runs_directory(&root_dir) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => {
                        return Err(unavailable_io("create runs directory", error));
                    }
                }
                match open_runs_directory(&root_dir) {
                    Ok(directory) => directory,
                    Err(error) if unsafe_directory(&error) => {
                        return Err(unavailable(
                            "unsafe storage root: runs is not a real directory",
                        ));
                    }
                    Err(error) => return Err(unavailable_io("open runs directory", error)),
                }
            }
            Err(error) if unsafe_directory(&error) => {
                return Err(unavailable(
                    "unsafe storage root: runs is not a real directory",
                ));
            }
            Err(error) => return Err(unavailable_io("open runs directory", error)),
        };
        Ok(Some(Self {
            root_dir,
            runs_dir,
            display_root: root.to_owned(),
        }))
    }

    /// Acquire the recorder/delete lock within this anchored storage root.
    ///
    /// Nonblocking acquisition makes contention an explicit error. Read paths
    /// intentionally do not take this lock: they classify missing finalization
    /// separately and verify finalized bytes rather than blocking a recorder.
    pub(crate) fn lock(&self) -> Result<Lock> {
        let file = openat_beneath(
            &self.root_dir,
            c".record.lock",
            libc::O_RDWR | libc::O_CREAT | libc::O_NONBLOCK | libc::O_CLOEXEC,
            0o600,
        )
        .map_err(|error| unavailable_io("open storage lock", error))?;
        if !is_regular(
            &file_stat(&file).map_err(|error| unavailable_io("inspect storage lock", error))?,
        ) {
            return Err(unavailable(
                "unsafe storage lock: .record.lock is not a regular file",
            ));
        }
        // Kernel releases the advisory lock even if the recorder is killed.
        // SAFETY: file owns the live descriptor for the whole call; flock
        // neither retains pointers nor transfers descriptor ownership.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(lock_error(std::io::Error::last_os_error()));
        }
        Ok(Lock(file))
    }

    /// Check the label before an expensive backend probe; `begin_run` rechecks
    /// under its retained lock before creating any record directory.
    pub(crate) fn check_name_available(&self, name: &str) -> Result<()> {
        validate_record_name(name)?;
        let _lock = self.lock()?;
        reject_name_collision(&self.identities()?, name)
    }

    /// Start one record while retaining the storage lock and every directory fd.
    ///
    /// Time/PID provides the ID spelling, not uniqueness proof; the locked index
    /// and exclusive directory creation still reject collisions. If setup fails,
    /// a partial directory may remain for inspection rather than being repaired.
    pub(crate) fn begin_run(self, name: &str) -> Result<RunWriter> {
        let id = format!(
            "{:x}-{}",
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
            std::process::id()
        );
        self.begin_run_with_id(name, id)
    }

    /// Recheck reservations under the lock before granting the writer its fds.
    fn begin_run_with_id(self, name: &str, id: String) -> Result<RunWriter> {
        validate_record_name(name)?;
        if !valid_id(&id) {
            bail!("generated record ID is not one safe path component");
        }
        let lock = self.lock()?;
        let identities = self.identities()?;
        if let Err(error) = reject_name_collision(&identities, name) {
            drop(lock);
            return Err(error);
        }
        if identities
            .iter()
            .any(|identity| identity.name == id || identity.id == id)
        {
            drop(lock);
            bail!("record ID already exists: {id}");
        }

        let id_component = CString::new(id.as_bytes()).expect("validated IDs contain no NUL");
        create_directory_at(&self.runs_dir, &id_component, 0o700)?;
        let run_dir = openat_beneath(
            &self.runs_dir,
            &id_component,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            0,
        )?;
        // mkdir's mode is affected by umask. fchmod freezes the historical 0700
        // contract on the directory we actually opened.
        // SAFETY: run_dir owns a live directory descriptor, and fchmod retains
        // no pointer and does not transfer or close that descriptor.
        if unsafe { libc::fchmod(run_dir.as_raw_fd(), 0o700) } != 0 {
            return Err(std::io::Error::last_os_error()).context("set record directory mode");
        }
        ensure_entry_identity(&self.runs_dir, &id_component, &run_dir)
            .context("record directory changed while opening")?;

        create_directory_at(&run_dir, c"raw", 0o777)?;
        let raw_dir = openat_beneath(
            &run_dir,
            c"raw",
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            0,
        )?;
        let trace_file = openat_beneath(
            &raw_dir,
            c"trace.log",
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC,
            0o600,
        )?;
        let evidence_dir = self.display_root.join("runs").join(&id);
        Ok(RunWriter {
            storage: self,
            _lock: lock,
            run_dir,
            raw_dir,
            trace_file,
            id,
            name: name.to_owned(),
            evidence_dir,
            initial_metadata: InitialMetadata::Unwritten,
            trace_receipt: None,
            trace_contract: false,
        })
    }
}

impl RunWriter {
    /// Check that each stored leaf still names this writer's held inode.
    ///
    /// These observations reject detected directory/file swaps; they do not
    /// make subsequent operations transactional against noncooperating writers.
    fn ensure_run_identity(&self) -> Result<()> {
        let component = CString::new(self.id.as_bytes()).expect("validated IDs contain no NUL");
        ensure_entry_identity(&self.storage.runs_dir, &component, &self.run_dir)
            .map_err(|error| unavailable_io("record directory changed during capture", error))?;
        ensure_entry_identity(&self.run_dir, c"raw", &self.raw_dir)
            .map_err(|error| unavailable_io("raw directory changed during capture", error))?;
        ensure_entry_identity(&self.raw_dir, c"trace.log", &self.trace_file)
            .map_err(|error| unavailable_io("trace file changed during capture", error))?;
        Ok(())
    }

    /// Generated on-disk ID for the record.
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    /// Existing user-visible evidence directory string.
    ///
    /// Reporting may use this spelling; storage I/O must keep using held fds.
    pub(crate) fn evidence_dir(&self) -> &Path {
        &self.evidence_dir
    }

    /// Persist initial running metadata once, relative to the held run fd.
    ///
    /// Pure validation errors precede I/O. Once publication is attempted, any
    /// error makes initialization terminal, including unknown directory durability.
    pub(crate) fn write_metadata(&mut self, metadata: &Metadata) -> Result<()> {
        if self.initial_metadata != InitialMetadata::Unwritten {
            bail!("initial metadata was already written or its publication failed");
        }
        if metadata.id != self.id || metadata.name != self.name {
            bail!("metadata identity does not match the active record writer");
        }
        validate_metadata_contract(metadata)
            .map_err(|error| stored_contract_error("invalid metadata contract", error))?;
        if metadata.state != "running" {
            bail!("initial metadata must describe a running capture");
        }
        self.ensure_run_identity()?;
        self.initial_metadata = InitialMetadata::Failed;
        metadata_at(&self.run_dir, metadata)?;
        self.initial_metadata = InitialMetadata::Written;
        self.trace_contract = metadata.schema_version == 4;
        Ok(())
    }

    /// Return the strace output handle path for the precreated trace inode.
    ///
    /// Directory fds and the recorder's trace fd are CLOEXEC. Strace receives no
    /// storage descriptor; it reopens only this regular file through the live
    /// recorder process. If procfs cannot provide that handle, capture fails
    /// before the target starts rather than falling back to a mutable path.
    pub(crate) fn trace_target(&self) -> Result<PathBuf> {
        self.ensure_run_identity()?;
        let target = PathBuf::from(format!(
            "/proc/{}/fd/{}",
            std::process::id(),
            self.trace_file.as_raw_fd()
        ));
        let probe = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC)
            .open(&target)
            .context("procfs cannot reopen the anchored trace file")?;
        if !same_file(&file_stat(&self.trace_file)?, &file_stat(&probe)?) {
            bail!("procfs trace handle does not identify the anchored trace file");
        }
        Ok(target)
    }

    /// Return the current byte length of the anchored trace inode.
    ///
    /// This is a storage fact only. Callers retain responsibility for deciding
    /// whether a particular length has capture or parsing significance.
    pub(crate) fn trace_len(&self) -> Result<u64> {
        self.ensure_run_identity()?;
        let status = file_stat(&self.trace_file)
            .map_err(|error| unavailable_io("inspect anchored trace file", error))?;
        u64::try_from(status.st_size)
            .map_err(|_| unavailable("anchored trace file has a negative byte length"))
    }

    /// Read the anchored trace one logical line at a time.
    ///
    /// A trailing LF (and any preceding CR) is retained so an incremental
    /// parser can distinguish a terminated final line. Each line is validated
    /// as UTF-8 independently, and memory use is bounded by the longest line.
    ///
    /// The line bound is not a fixed quota, and the consumer may retain more
    /// state. A consumer/UTF-8 error stops this traversal after any earlier
    /// callbacks; capture decides whether their partial results can be retained.
    /// For trace-v1, successful EOF freezes a byte/line/digest receipt of this
    /// exact input. Failure freezes an unverified reason; hash-only I/O cannot
    /// replace it. The default structured-only contract does not hash raw here.
    pub(crate) fn for_each_trace_line<F>(&mut self, mut consume: F) -> std::io::Result<()>
    where
        F: FnMut(&str) -> std::io::Result<()>,
    {
        // A failed or interrupted new traversal cannot retain a previous EOF
        // receipt. Hash precisely the bytes delivered to the consumer, including
        // framing; a later read-back only verifies them and never creates one.
        self.trace_receipt = None;
        let trace_v1 = self.trace_contract;
        let traversal = (|| {
            self.ensure_run_identity()
                .map_err(|error| std::io::Error::other(format!("{error:#}")))?;
            self.trace_file.seek(SeekFrom::Start(0))?;
            let mut reader = BufReader::new(&mut self.trace_file);
            let mut line = Vec::new();
            let mut hasher = trace_v1.then(Sha256::new);
            let mut bytes = 0u64;
            let mut logical_lines = 0usize;
            loop {
                line.clear();
                if reader.read_until(b'\n', &mut line)? == 0 {
                    return Ok(hasher.map(|hasher| RawReceipt::Parsed {
                        bytes,
                        logical_lines,
                        sha256: format!("{:x}", hasher.finalize()),
                    }));
                }
                if let Some(hasher) = &mut hasher {
                    bytes = checked_byte_count(bytes, line.len())?;
                    logical_lines = logical_lines.checked_add(1).ok_or_else(|| {
                        std::io::Error::other("raw trace logical line count exceeds usize")
                    })?;
                    hasher.update(&line);
                }
                let line = std::str::from_utf8(&line).map_err(|error| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("trace line is not valid UTF-8: {error}"),
                    )
                })?;
                consume(line)?;
            }
        })();
        match traversal {
            Ok(receipt) => {
                self.trace_receipt = receipt;
                Ok(())
            }
            Err(error) => {
                self.trace_receipt = trace_v1.then(|| RawReceipt::Unverified {
                    reason: format!("normalization stream did not reach a successful EOF: {error}"),
                });
                Err(error)
            }
        }
    }

    /// Read the trace inode held by this writer after the collector exits.
    ///
    /// Compatibility/test adapter: unlike `for_each_trace_line`, this retains the
    /// whole UTF-8 input. Both adapters assume capture has stopped the collector;
    /// this method itself neither waits for it nor establishes a stable snapshot.
    #[cfg(test)]
    pub(crate) fn read_trace(&mut self) -> std::io::Result<String> {
        self.ensure_run_identity()
            .map_err(|error| std::io::Error::other(format!("{error:#}")))?;
        self.trace_file.seek(SeekFrom::Start(0))?;
        let mut raw = String::new();
        self.trace_file.read_to_string(&mut raw)?;
        Ok(raw)
    }

    /// Publish events, metadata and the selected protocol's last finalization marker.
    ///
    /// Consumes write authority on both success and failure. The lock and all
    /// anchored descriptors stay alive through publication, read-back and cleanup;
    /// commit-stage errors propagate unchanged, never as permission to retry.
    pub(crate) fn finish(self, metadata: &Metadata, events: &[Event]) -> Result<()> {
        if self.initial_metadata != InitialMetadata::Written {
            bail!("record finalization requires successfully written initial metadata");
        }
        if metadata.id != self.id || metadata.name != self.name {
            bail!("metadata identity does not match the active record writer");
        }
        if (metadata.schema_version == 4) != self.trace_contract {
            bail!("final metadata changes the initially selected recording contract");
        }
        self.ensure_run_identity()?;
        let manifest = (metadata.schema_version == 4).then(|| RawManifest {
            schema_version: 1,
            run_id: self.id.clone(),
            file: "raw/trace.log".into(),
            receipt: self
                .trace_receipt
                .unwrap_or_else(|| RawReceipt::Unverified {
                    reason: "raw normalization traversal was not attempted".into(),
                }),
        });
        finish_at_with_manifest_control(
            &self.run_dir,
            metadata,
            events,
            manifest.as_ref(),
            AtomicControl::default(),
        )
    }
}

/// Open or create a storage root and anchor its `root` and `runs` directories.
pub(crate) fn open(root: &Path) -> Result<StorageRoot> {
    StorageRoot::open_with(root, true)?.context("storage root disappeared while opening")
}

fn open_existing(root: &Path) -> Result<Option<StorageRoot>> {
    StorageRoot::open_with(root, false)
}

/// Test compatibility wrapper for the path-based locking oracle.
#[cfg(test)]
fn lock(root: &Path) -> Result<Lock> {
    open(root)?.lock()
}

/// Test compatibility writer for the legacy path-based oracle.
#[cfg(test)]
fn metadata(dir: &Path, m: &Metadata) -> Result<()> {
    let directory =
        open_directory(dir).map_err(|error| unavailable_io("open record directory", error))?;
    metadata_at(&directory, m)
}
/// Index data only: missing or malformed labels are empty, never fabricated
/// metadata. An I/O failure prevents building this reservation set.
struct Identity {
    id: String,
    name: String,
}
/// One scan result carrying selection hints separately from opened authority.
///
/// An unknown/malformed label never creates metadata. Errors stay attached so
/// reservation can fail closed while loading applies its own integrity priority.
struct Entry {
    run_dir: Option<File>,
    directory: String,
    id: Option<String>,
    name: Option<String>,
    open_error: Option<anyhow::Error>,
    // Reservation must fail on label-read I/O errors. Loads independently
    // enforce marker/checksum priority instead of treating an index hint as
    // authority for the record's integrity classification.
    index_error: Option<anyhow::Error>,
}
/// Recognize the complete generated ID spelling used as one directory component.
///
/// Recognition admits selection, not trust in the record's bytes. No prefix,
/// normalization or fuzzy match is substituted for the user's exact key.
fn valid_id(id: &str) -> bool {
    crate::model::valid_record_id(id)
}

/// Read one anchored regular structured file within its explicit byte budget.
///
/// Metadata/marker callers use whole-file decoding; event callers use streaming.
fn read_file_at(directory: &File, name: &CStr, limit: usize) -> std::io::Result<Vec<u8>> {
    let mut file = open_regular_file_at(directory, name)?;
    read_bounded(&mut file, limit, "structured record file")
}

/// Open a leaf nonblockingly, then reject devices/FIFOs before reading it.
///
/// O_NONBLOCK prevents a substituted FIFO from hanging the open before its type
/// can be checked. The opened descriptor remains the subsequent read authority.
fn open_regular_file_at(directory: &File, name: &CStr) -> std::io::Result<File> {
    let file = openat_beneath(
        directory,
        name,
        libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
        0,
    )?;
    if !is_regular(&file_stat(&file)?) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "not a regular file",
        ));
    }
    Ok(file)
}

fn open_required_record_file(directory: &File, name: &CStr, display: &str) -> Result<File> {
    open_regular_file_at(directory, name)
        .map_err(|error| required_record_file_error(display, error))
}

/// Verify marker length before streaming the anchored bytes and comparing SHA-256.
///
/// Resource ceilings precede hashing. Within them, byte corruption still has
/// priority over interpreting a possibly misleading DTO/schema.
fn verified_file_check(file: &mut File, expected: &FileCheck, display: &str) -> Result<FileCheck> {
    let status = file_stat(file).map_err(|error| unavailable_io(display, error))?;
    let length = stat_length(&status).map_err(|error| unavailable_io(display, error))?;
    let limit = if display == "events.jsonl" {
        MAX_EVENT_FILE_BYTES
    } else if display == "raw-integrity.json" {
        MAX_MARKER_BYTES as u64
    } else {
        MAX_METADATA_BYTES as u64
    };
    if length > limit {
        return Err(unavailable_io(
            display,
            resource_limit(display, "bytes", limit),
        ));
    }
    if length != expected.bytes {
        return Err(corrupt(format!(
            "{display}: byte count or SHA-256 mismatch"
        )));
    }
    let actual = calculate_file_check(file).map_err(|error| unavailable_io(display, error))?;
    verify_check(&actual, expected, display)?;
    Ok(actual)
}

/// Rewind and materialize an already opened file.
///
/// Finalized readers recheck these bytes against the marker before decoding.
fn read_open_file(file: &mut File, display: &str) -> Result<Vec<u8>> {
    file.seek(SeekFrom::Start(0))
        .and_then(|_| read_bounded(file, MAX_METADATA_BYTES, display))
        .map_err(|error| unavailable_io(display, error))
}

/// Missing required structure is damage; inability to access it is not evidence of damage.
fn required_record_file_error(display: &str, error: std::io::Error) -> anyhow::Error {
    if error.kind() == std::io::ErrorKind::NotFound {
        corrupt(format!("{display}: {error}"))
    } else {
        unavailable_io(display, error)
    }
}

#[derive(Deserialize)]
/// Minimal, untrusted label view used before full record verification.
struct MetadataIndexHint {
    name: Option<String>,
}

/// Read only a usable label for exact indexing, preserving real I/O failures.
///
/// Malformed JSON or label fields may hide a label, leaving the directory ID
/// selectable. This does not prove that an unknown historical label is free,
/// validate metadata, or authorize reconstruction of the missing value.
fn metadata_name_hint(directory: &File) -> Result<Option<String>> {
    let mut file = match open_regular_file_at(directory, c"metadata.json") {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(unavailable_io("read record label", error)),
    };
    let bytes = read_bounded(&mut file, MAX_METADATA_BYTES, "read record label")
        .map_err(|error| unavailable_io("read record label", error))?;
    check_json_budget(&bytes, "read record label")
        .map_err(|error| unavailable_io("read record label", error))?;
    let hint = match serde_json::from_slice::<MetadataIndexHint>(&bytes) {
        Ok(hint) => hint,
        // Missing or malformed stored labels remain unknown index hints. An
        // read/resource failure above cannot establish that a label is absent
        // and prevents reserving a potentially duplicate label.
        Err(_) => return Ok(None),
    };
    Ok(hint
        .name
        .filter(|candidate| !candidate.is_empty() && !candidate.chars().any(char::is_control)))
}

#[derive(Deserialize)]
/// Decode the explicit version before attempting a version-specific DTO.
struct SchemaHeader {
    schema_version: u32,
}

/// Distinguish an unreadable header from a readable but unsupported schema.
fn decode_schema_header(data: &[u8], display: &str) -> Result<u32> {
    serde_json::from_slice::<SchemaHeader>(data)
        .map(|header| header.schema_version)
        .map_err(|error| corrupt(format!("{display}: {error}")))
}

fn validate_run_contract(metadata: &Metadata, events: &[Event], finalized: bool) -> ContractResult {
    crate::model::validate_run_contract(metadata, events, finalized)
}

/// Decode a supported metadata version without migrating its saved evidence.
///
/// V2/V3 require their introduced fields. Directory ID agreement is checked
/// independently of the earlier label hint; current-only rules stay in the
/// contract validator rather than upgrading legacy records on read.
fn decode_metadata(data: &[u8], id: &str) -> Result<Metadata> {
    if data.len() > MAX_METADATA_BYTES {
        return Err(unavailable_io(
            "metadata.json",
            resource_limit("metadata.json", "bytes", MAX_METADATA_BYTES),
        ));
    }
    check_json_budget(data, "metadata.json")
        .map_err(|error| unavailable_io("metadata.json", error))?;
    let schema_version = decode_schema_header(data, "metadata.json")?;
    if !matches!(schema_version, 1..=4) {
        return Err(unsupported(format!(
            "unsupported metadata schema {schema_version}"
        )));
    }
    let m: Metadata =
        serde_json::from_slice(data).map_err(|e| corrupt(format!("metadata.json: {e}")))?;
    if m.schema_version >= 2 && (m.environment.is_none() || m.completeness.is_none()) {
        return Err(corrupt("metadata: missing environment or completeness"));
    }
    if m.id != id {
        return Err(corrupt("record ID does not match directory"));
    }
    validate_metadata_contract(&m)
        .map_err(|error| stored_contract_error("metadata contract", error))?;
    Ok(m)
}
fn selection_error(key: &str, count: usize) -> anyhow::Error {
    unavailable(format!(
        "record {key:?}: expected one safe ID/label match, found {count}; unknown labels require an exact recognizable directory ID; unsafe entries cannot be selected"
    ))
}

impl StorageRoot {
    /// Visit one anchored entry at a time. Callers may retain one selected
    /// directory, but listing and label reservation release it before the next
    /// entry so the descriptor bound is independent of the record count.
    ///
    /// Sorting the collected names gives deterministic presentation, not a filesystem
    /// transaction. Names and eventual summaries still occupy count-dependent
    /// memory. A visitor decides whether one entry error aborts reservation or
    /// becomes an isolated listing row.
    fn scan(&self, mut visit: impl FnMut(Entry) -> Result<()>) -> Result<()> {
        let mut names: Vec<_> = directory_names(&self.runs_dir)
            .map_err(|error| unavailable_io("enumerate record storage", error))?
            .into_iter()
            .map(|leaf| (format!("runs/{}", crate::model::bytes(&leaf)), leaf))
            .collect();
        names.sort_by(|left, right| left.0.cmp(&right.0));
        for (directory, leaf) in names {
            let component = CString::new(leaf.as_slice())
                .expect("directory entries cannot contain an interior NUL");
            let recognized = std::str::from_utf8(&leaf)
                .ok()
                .filter(|id| valid_id(id))
                .map(str::to_owned);
            let status = stat_at(&self.runs_dir, &component);
            let safe = status.as_ref().is_ok_and(is_directory) && recognized.is_some();
            let id = safe.then_some(recognized).flatten();
            let mut name = None;
            let mut run_dir = None;
            let mut index_error = None;
            let open_error = if let (Some(expected), Some(_)) = (status.as_ref().ok(), id.as_ref())
            {
                match openat_beneath(
                    &self.runs_dir,
                    &component,
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                    0,
                ) {
                    Ok(directory_fd) => match file_stat(&directory_fd) {
                        Ok(actual) if same_file(expected, &actual) => {
                            // A label is only an index hint. Decode failures are
                            // deliberately ignored here so record loading can
                            // enforce marker/checksum error priority first.
                            match metadata_name_hint(&directory_fd) {
                                Ok(hint) => name = hint,
                                Err(error) => index_error = Some(error),
                            }
                            run_dir = Some(directory_fd);
                            None
                        }
                        Ok(_) => Some(unavailable(format!(
                            "unsafe record directory changed while opening: {directory}"
                        ))),
                        Err(error) => Some(unavailable_io("inspect record directory", error)),
                    },
                    Err(error) => Some(unavailable_io("open record directory", error)),
                }
            } else {
                Some(unavailable(format!(
                    "unsafe entry or unrecognizable run ID: {directory}"
                )))
            };
            visit(Entry {
                run_dir,
                directory,
                id,
                name,
                open_error,
                index_error,
            })?;
        }
        Ok(())
    }

    /// Retain a candidate only after counting every exact safe ID/label match.
    ///
    /// A first match is insufficient: an ID-shaped label can collide with another
    /// directory ID. The key never becomes a pathname, and an unknown label may
    /// only be bypassed by selecting its exact recognizable directory ID.
    fn resolve(&self, key: &str) -> Result<Entry> {
        let mut candidate = None;
        let mut count = 0;
        self.scan(|entry| {
            if entry.id.as_deref() == Some(key) || entry.name.as_deref() == Some(key) {
                count += 1;
                if candidate.is_none() {
                    candidate = Some(entry);
                }
            }
            Ok(())
        })?;
        if count != 1 {
            return Err(selection_error(key, count));
        }
        Ok(candidate.expect("one matched entry was retained"))
    }

    /// Return name/ID reservation facts without treating labels as paths.
    ///
    /// Unlike listing, reservation cannot ignore access/enumeration failures:
    /// doing so could create a duplicate visible label. A malformed label remains
    /// unknown; its directory ID is reserved without inventing the label.
    fn identities(&self) -> Result<Vec<Identity>> {
        let mut identities = Vec::new();
        self.scan(|entry| {
            if let Some(error) = entry.open_error.or(entry.index_error) {
                return Err(error).with_context(|| {
                    format!("scan {}: cannot reserve a new label", entry.directory)
                });
            }
            let id = entry.id.ok_or_else(|| {
                unavailable(format!(
                    "unsafe storage entry: {}; cannot reserve a new label",
                    entry.directory
                ))
            })?;
            identities.push(Identity {
                id,
                name: entry.name.unwrap_or_default(),
            });
            Ok(())
        })?;
        Ok(identities)
    }

    /// Delete one exact, uniquely resolved ID or label from this storage root.
    fn delete(&self, key: &str) -> Result<String> {
        let _lock = self.lock()?;
        let e = self.resolve(key)?;
        self.delete_resolved(e)
    }

    /// Move the selected inode into explicit isolation before physical cleanup.
    ///
    /// The caller holds the cooperating lock. Failed cleanup cannot expose a
    /// partly deleted active record; same-UID mutation is still outside that lock.
    fn delete_resolved(&self, e: Entry) -> Result<String> {
        deletion::isolate(self, e)
    }
}

/// Check that a current directory entry names the inode already held by a caller.
///
/// The observation does not freeze the entry after return. Structural operations
/// must continue relative to held descriptors and avoid following replacement
/// leaf symlinks even when another process ignores the advisory lock.
fn ensure_entry_identity(parent: &File, name: &CStr, opened: &File) -> std::io::Result<()> {
    let expected = file_stat(opened)?;
    let actual = stat_at(parent, name)?;
    if same_file(&expected, &actual) {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "directory entry no longer names the opened inode",
        ))
    }
}

fn unlink_at(parent: &File, name: &CStr, flags: libc::c_int) -> std::io::Result<()> {
    // SAFETY: both inputs stay live and unlinkat does not retain their pointers.
    let result = unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Detect an empty replacement removed after the last identity observation.
///
/// Linux leaves an unlinked directory's held inode with zero links. A successful
/// unlink alone is insufficient: a noncooperating writer can rename the selected
/// directory and put another empty directory in its place after our precheck.
/// This postcondition detects that case but cannot roll back replacement removal.
fn unlink_opened_directory(parent: &File, name: &CStr, opened: &File) -> std::io::Result<()> {
    ensure_entry_identity(parent, name, opened)?;
    unlink_at(parent, name, libc::AT_REMOVEDIR)?;
    if file_stat(opened)?.st_nlink != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "selected directory remains linked after removal",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DeletePass {
    Preflight,
    Remove,
}

/// Recurse through anchored real directories and unlink every other type as a leaf.
///
/// `openat_beneath` applies the mount/symlink boundary to each opened child;
/// its legacy-kernel same-device bind-mount limitation still applies. Recursion
/// retains ancestor fds up to the depth budget; per-directory name vectors still
/// occupy memory proportional to the enumerated entries. A read-only preflight
/// rejects existing excess depth or access failures before any unlink. It is not
/// a snapshot: later external changes or I/O failures can still leave a partly
/// deleted record. Every removal repeats the depth and identity checks.
fn walk_delete_tree(directory: &File, depth: usize, pass: DeletePass) -> std::io::Result<()> {
    if depth > MAX_DELETE_DEPTH {
        return Err(resource_limit(
            "delete record",
            "directory depth",
            MAX_DELETE_DEPTH,
        ));
    }
    for leaf in directory_names(directory)? {
        let component =
            CString::new(leaf).expect("directory entries cannot contain an interior NUL");
        let status = stat_at(directory, &component)?;
        if is_directory(&status) {
            let child = openat_beneath(
                directory,
                &component,
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                0,
            )?;
            let opened = file_stat(&child)?;
            if !same_file(&status, &opened) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "directory entry changed while deleting",
                ));
            }
            walk_delete_tree(&child, depth + 1, pass)?;
            ensure_entry_identity(directory, &component, &child)?;
            if pass == DeletePass::Remove {
                unlink_opened_directory(directory, &component, &child)?;
            }
        } else if pass == DeletePass::Remove {
            // Symlinks, FIFOs and devices are unlinked as leaves and never opened.
            unlink_at(directory, &component, 0)?;
        }
    }
    Ok(())
}

/// Name/ID reservations for capture, independent of record validation.
#[allow(dead_code)]
fn all(root: &Path) -> Result<Vec<(PathBuf, Identity)>> {
    let Some(storage) = open_existing(root)? else {
        return Ok(vec![]);
    };
    Ok(storage
        .identities()?
        .into_iter()
        .map(|identity| {
            let dir = root.join("runs").join(&identity.id);
            (dir, identity)
        })
        .collect())
}

/// Delete one exact ID or label without following storage symlinks.
pub(crate) fn delete(root: &Path, key: &str) -> Result<String> {
    open(root)?.delete(key)
}

/// Stable storage failure classes used at the persistence boundary.
///
/// `Unavailable` retains an optional OS error as its source. In particular,
/// resource and permission failures must never be relabeled as record damage.
#[derive(Debug)]
enum StorageError {
    /// Bytes or mutually related facts contradict a supported storage schema.
    Corrupt(String),
    /// A well-formed schema header names a version this binary cannot read.
    Unsupported(String),
    /// Storage could not be safely or currently accessed without claiming damage.
    Unavailable {
        message: String,
        source: Option<std::io::Error>,
    },
    /// The advisory recorder/delete lock is held by another operation.
    Busy(String),
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Corrupt(message) => write!(f, "corrupt record: {message}"),
            Self::Unsupported(message) | Self::Busy(message) => f.write_str(message),
            Self::Unavailable {
                message,
                source: Some(source),
            } => write!(f, "{message}: {source}"),
            Self::Unavailable {
                message,
                source: None,
            } => f.write_str(message),
        }
    }
}
impl std::error::Error for StorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Unavailable {
                source: Some(source),
                ..
            } => Some(source),
            _ => None,
        }
    }
}

fn corrupt(message: impl Into<String>) -> anyhow::Error {
    StorageError::Corrupt(message.into()).into()
}

/// Keep unsupported contract vocabulary distinct from contradictory supported facts.
fn stored_contract_error(context: &str, error: ContractViolation) -> anyhow::Error {
    let message = format!("{context}: {error}");
    match error.kind {
        StorageLoadFailureKind::Unsupported => unsupported(message),
        StorageLoadFailureKind::Corrupt => corrupt(message),
        StorageLoadFailureKind::Unavailable | StorageLoadFailureKind::Busy => unavailable(message),
    }
}

fn unsupported(message: impl Into<String>) -> anyhow::Error {
    StorageError::Unsupported(message.into()).into()
}

fn unavailable(message: impl Into<String>) -> anyhow::Error {
    StorageError::Unavailable {
        message: message.into(),
        source: None,
    }
    .into()
}

fn unavailable_io(message: impl Into<String>, source: std::io::Error) -> anyhow::Error {
    StorageError::Unavailable {
        message: message.into(),
        source: Some(source),
    }
    .into()
}

fn lock_error(source: std::io::Error) -> anyhow::Error {
    let errno = source.raw_os_error();
    if errno == Some(libc::EAGAIN) || errno == Some(libc::EWOULDBLOCK) {
        StorageError::Busy(
            "storage busy: another recorder or delete operation holds the lock".into(),
        )
        .into()
    } else {
        unavailable_io("acquire storage lock", source)
    }
}

/// Preserve the typed storage cause through contextual application error wrappers.
///
/// An unknown failure defaults to unavailable, since absence of a recognized
/// damage cause cannot justify classifying stored evidence as corrupt.
fn storage_load_failure_kind(error: &anyhow::Error) -> StorageLoadFailureKind {
    error
        .chain()
        .find_map(|source| source.downcast_ref::<StorageError>())
        .map_or(StorageLoadFailureKind::Unavailable, |error| match error {
            StorageError::Corrupt(_) => StorageLoadFailureKind::Corrupt,
            StorageError::Unsupported(_) => StorageLoadFailureKind::Unsupported,
            StorageError::Unavailable { .. } => StorageLoadFailureKind::Unavailable,
            StorageError::Busy(_) => StorageLoadFailureKind::Busy,
        })
}

/// Remove concrete I/O error types before diff consumes the load result.
///
/// Storage describes access/integrity failure; comparison owns how that failure
/// affects report reliability and exit status. Neither is inferred from text.
fn neutral_load_failure(error: anyhow::Error) -> StorageLoadFailure {
    StorageLoadFailure {
        kind: storage_load_failure_kind(&error),
        message: format!("{error:#}"),
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Exact serialized-byte fingerprint used by the structured finalization marker.
///
/// Hashing includes framing/newlines; it neither authenticates origin nor compares
/// observed target files. Markers bind structured components; a bound raw manifest
/// carries the separate normalization-stream receipt.
struct FileCheck {
    bytes: u64,
    sha256: String,
}
#[derive(Serialize, Deserialize)]
/// Last-published descriptor of finalized metadata/events, independent of capture outcome.
///
/// A marker can describe an interrupted or failed capture. Readers still check
/// ID, bytes, event count and supported contracts; merely finding the file does
/// not establish trusted storage or authorize promotion of a legacy record.
struct Finalized {
    schema_version: u32,
    run_id: String,
    event_count: usize,
    events: FileCheck,
    metadata: FileCheck,
    /// Present only in marker v2; binds the independently classified raw receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    raw_manifest: Option<FileCheck>,
}

/// Decode a supported marker explicitly; never manufacture a replacement during a read.
fn decode_finalized(data: &[u8]) -> Result<Finalized> {
    if data.len() > MAX_MARKER_BYTES {
        return Err(unavailable_io(
            "finalized.json",
            resource_limit("finalized.json", "bytes", MAX_MARKER_BYTES),
        ));
    }
    check_json_budget(data, "finalized.json")
        .map_err(|error| unavailable_io("finalized.json", error))?;
    let schema_version = decode_schema_header(data, "finalized.json")?;
    if !matches!(schema_version, 1 | 2) {
        return Err(unsupported(format!(
            "unsupported finalization marker schema {schema_version}"
        )));
    }
    let marker: Finalized = serde_json::from_slice(data)
        .map_err(|error| corrupt(format!("finalized.json: {error}")))?;
    if (schema_version == 2) != marker.raw_manifest.is_some() {
        return Err(corrupt(
            "finalization marker version contradicts raw manifest binding",
        ));
    }
    Ok(marker)
}

/// Decode a bound receipt without asking a newer parser to reinterpret its raw bytes.
fn decode_raw_manifest(data: &[u8]) -> Result<RawManifest> {
    if data.len() > MAX_MARKER_BYTES {
        return Err(unavailable_io(
            "raw-integrity.json",
            resource_limit("raw-integrity.json", "bytes", MAX_MARKER_BYTES),
        ));
    }
    check_json_budget(data, "raw-integrity.json")
        .map_err(|error| unavailable_io("raw-integrity.json", error))?;
    let schema = decode_schema_header(data, "raw-integrity.json")?;
    if schema != 1 {
        return Err(unsupported(format!(
            "unsupported raw manifest schema {schema}"
        )));
    }
    let manifest: RawManifest = serde_json::from_slice(data)
        .map_err(|error| corrupt(format!("raw-integrity.json: {error}")))?;
    Ok(manifest)
}

/// Record raw verification as a separate fact; it cannot erase verified structured facts.
///
/// Reads stay beneath the held record/raw descriptors. Missing or mismatching
/// promised evidence is corruption; permission/resource access is unavailable.
/// An intentionally unverified receipt does not trigger speculative raw I/O.
fn raw_integrity_at(directory: &File, manifest: RawManifest) -> RawIntegrity {
    let result = verify_raw_receipt_at(directory, &manifest);
    let (status, detail) = match result {
        Ok(true) => ("verified", "saved raw bytes match the bound normalization-stream receipt; this does not authenticate origin or prove capture success".into()),
        Ok(false) => {
            let RawReceipt::Unverified { reason } = &manifest.receipt else { unreachable!() };
            ("unverified", reason.clone())
        }
        Err(error) => (match storage_load_failure_kind(&error) {
            StorageLoadFailureKind::Corrupt => "corrupt",
            StorageLoadFailureKind::Unsupported => "unsupported",
            StorageLoadFailureKind::Unavailable | StorageLoadFailureKind::Busy => "unavailable",
        }, format!("{error:#}")),
    };
    RawIntegrity {
        status: status.into(),
        detail,
        manifest: Some(manifest),
    }
}

fn verify_raw_receipt_at(directory: &File, manifest: &RawManifest) -> Result<bool> {
    let RawReceipt::Parsed {
        bytes,
        logical_lines,
        sha256,
    } = &manifest.receipt
    else {
        return Ok(false);
    };
    let raw_dir = openat_beneath(
        directory,
        c"raw",
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        0,
    )
    .map_err(|error| required_record_file_error("raw directory", error))?;
    let mut trace = open_required_record_file(&raw_dir, c"trace.log", "raw/trace.log")?;
    let length =
        stat_length(&file_stat(&trace).map_err(|error| unavailable_io("raw/trace.log", error))?)
            .map_err(|error| unavailable_io("raw/trace.log", error))?;
    if length != *bytes {
        return Err(corrupt("raw/trace.log: byte count or SHA-256 mismatch"));
    }
    let (actual, lines) = calculate_file_check_with_lines(&mut trace, true)
        .map_err(|error| unavailable_io("raw/trace.log", error))?;
    if lines != *logical_lines {
        return Err(corrupt(
            "raw/trace.log: logical line count differs from normalization receipt",
        ));
    }
    verify_check(
        &actual,
        &FileCheck {
            bytes: *bytes,
            sha256: sha256.clone(),
        },
        "raw/trace.log",
    )?;
    ensure_entry_identity(directory, c"raw", &raw_dir)
        .map_err(|error| unavailable_io("raw directory changed while verifying", error))?;
    ensure_entry_identity(&raw_dir, c"trace.log", &trace)
        .map_err(|error| unavailable_io("raw trace changed while verifying", error))?;
    Ok(true)
}

fn check(data: &[u8]) -> FileCheck {
    FileCheck {
        bytes: data.len() as u64,
        sha256: format!("{:x}", Sha256::digest(data)),
    }
}

/// Hash an opened regular inode with fixed-size scratch space and length rechecks.
///
/// These checks detect observed length changes but are not an OS snapshot or a
/// lock against same-size concurrent writes. The caller must compare the digest
/// against its marker/staged bytes before treating it as an integrity result.
fn calculate_file_check(file: &mut File) -> std::io::Result<FileCheck> {
    calculate_file_check_with_lines(file, false).map(|(check, _)| check)
}

/// Count raw framing during the checksum pass; this is not syscall parsing.
fn calculate_file_check_with_lines(
    file: &mut File,
    count_lines: bool,
) -> std::io::Result<(FileCheck, usize)> {
    let before = file_stat(file)?;
    if !is_regular(&before) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "not a regular file",
        ));
    }
    let expected_length = stat_length(&before)?;
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut bytes = 0u64;
    let mut logical_lines = 0usize;
    let mut last_byte = None;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes = checked_byte_count(bytes, read)?;
        hasher.update(&buffer[..read]);
        if count_lines {
            logical_lines = logical_lines
                .checked_add(buffer[..read].iter().filter(|&&byte| byte == b'\n').count())
                .ok_or_else(|| {
                    std::io::Error::other("raw trace logical line count exceeds usize")
                })?;
            last_byte = Some(buffer[read - 1]);
        }
    }
    let after = file_stat(file)?;
    if !same_file(&before, &after)
        || stat_length(&after)? != expected_length
        || bytes != expected_length
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "regular file changed while checksumming",
        ));
    }
    if count_lines && last_byte.is_some_and(|byte| byte != b'\n') {
        logical_lines = logical_lines
            .checked_add(1)
            .ok_or_else(|| std::io::Error::other("raw trace logical line count exceeds usize"))?;
    }
    Ok((
        FileCheck {
            bytes,
            sha256: format!("{:x}", hasher.finalize()),
        },
        logical_lines,
    ))
}

fn checked_byte_count(current: u64, added: usize) -> std::io::Result<u64> {
    current.checked_add(added as u64).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "storage file byte count exceeds u64",
        )
    })
}

fn verify(data: &[u8], expected: &FileCheck, file: &str) -> Result<()> {
    let actual = check(data);
    verify_check(&actual, expected, file)
}

fn verify_check(actual: &FileCheck, expected: &FileCheck, file: &str) -> Result<()> {
    if actual.bytes != expected.bytes || actual.sha256 != expected.sha256 {
        return Err(corrupt(format!("{file}: byte count or SHA-256 mismatch")));
    }
    Ok(())
}
fn rename_at(
    old_directory: &File,
    old_name: &CStr,
    new_directory: &File,
    new_name: &CStr,
) -> std::io::Result<()> {
    // SAFETY: descriptors and names remain live and renameat retains no pointer.
    let result = unsafe {
        libc::renameat(
            old_directory.as_raw_fd(),
            old_name.as_ptr(),
            new_directory.as_raw_fd(),
            new_name.as_ptr(),
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Furthest publication stage observed when a single atomic replacement stops.
///
/// Visibility changes at rename; directory sync is a later operation. `Durable`
/// records successful sync calls, not experimentally proven power-loss behavior.
enum CommitStage {
    /// The destination has not been replaced by this operation.
    BeforePublish,
    /// Rename succeeded, but syncing the containing directory failed.
    PublishedDurabilityUnknown,
    /// File and directory sync succeeded; later verification may still fail.
    Durable,
}

#[derive(Debug)]
/// Preserve whether an I/O error happened before or after destination visibility.
///
/// A post-rename error is not a retry instruction: replacing again could hide a
/// partially finalized record whose first publication already reached readers.
struct CommitFailure {
    file: String,
    operation: &'static str,
    stage: CommitStage,
    source: std::io::Error,
    cleanup: Option<CleanupFailure>,
}

impl std::fmt::Display for CommitFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.stage {
            CommitStage::BeforePublish => write!(
                f,
                "atomic replace of {} failed before publish during {}: {}",
                self.file, self.operation, self.source
            ),
            CommitStage::PublishedDurabilityUnknown => write!(
                f,
                "atomic replace of {} was published but directory durability is unknown during {}: {}",
                self.file, self.operation, self.source
            ),
            CommitStage::Durable => write!(
                f,
                "atomic replace of {} was durable but {} failed: {}",
                self.file, self.operation, self.source
            ),
        }?;
        if let Some(cleanup) = &self.cleanup {
            write!(f, "; additionally {cleanup}")?;
        }
        Ok(())
    }
}

impl std::error::Error for CommitFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

impl CommitFailure {
    fn new(
        file: impl Into<String>,
        operation: &'static str,
        stage: CommitStage,
        source: std::io::Error,
    ) -> Self {
        Self {
            file: file.into(),
            operation,
            stage,
            source,
            cleanup: None,
        }
    }
}

#[derive(Debug)]
/// A failed explicit cleanup attempt, separate from the operation's primary cause.
///
/// The name describes the attempted unpublished entry. It is not an assertion
/// that the entry still exists: a later best-effort Drop retry may succeed.
struct CleanupFailure {
    temporary: String,
    source: std::io::Error,
}

impl std::fmt::Display for CleanupFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "temporary cleanup failed for {:?}: {}",
            self.temporary, self.source
        )
    }
}

#[derive(Debug)]
/// Keep a non-I/O staging failure and its typed cause chain when abort also fails.
struct StagingAbortFailure {
    primary: anyhow::Error,
    cleanup: CleanupFailure,
}

impl std::fmt::Display for StagingAbortFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}; additionally {}", self.primary, self.cleanup)
    }
}

impl std::error::Error for StagingAbortFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.primary.as_ref())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Deterministic failure checkpoints for the same publication path used in production.
enum AtomicStep {
    Write,
    FileSync,
    Link,
    FallbackCopy,
    FallbackFileSync,
    Rename,
    DirectorySync,
    ReadBack,
}

#[derive(Debug, Clone, Copy, Default)]
/// Explicit local fault-control seam; ordinary callers always pass the default.
///
/// No environment/global switch changes publication behavior. Tests can exercise
/// stage and fallback outcomes without filling storage or guessing random names.
struct AtomicControl {
    fail: Option<(&'static str, AtomicStep)>,
    force_named_fallback: bool,
    #[cfg(test)]
    force_link_unsupported: bool,
}

impl AtomicControl {
    fn check(&self, file: &str, step: AtomicStep) -> std::io::Result<()> {
        if self.fail == Some((file, step)) {
            Err(std::io::Error::from_raw_os_error(libc::EIO))
        } else {
            Ok(())
        }
    }
}

/// Own one temporary inode and, once materialized, its unpublished directory name.
///
/// The borrowed directory keeps cleanup anchored. `name = None` means there is
/// no owned temporary entry to unlink, including after rename consumes that name.
/// Explicit aborts retain cleanup failures alongside the primary error. Drop is
/// a best-effort fallback for unwinding or an owner abandoned without an abort;
/// it cannot report success or provide recovery when its cleanup fails.
struct StagedFile<'a> {
    directory: &'a File,
    file: File,
    name: Option<CString>,
}

/// A surviving link means cleanup did not establish removal of the owned inode.
///
/// It can result from a late name swap or an external hard link. This detects an
/// unsatisfied postcondition; it neither identifies which happened nor restores
/// an entry already removed by unlink. Zero links is an observation, not durable
/// exclusion of a same-UID writer or a proof of filesystem health.
fn require_unlinked_staging_inode(links: libc::nlink_t) -> std::io::Result<()> {
    if links == 0 {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "owned temporary inode remains linked after removal",
        ))
    }
}

impl StagedFile<'_> {
    /// Remove this name only while it still identifies the owned staging inode.
    ///
    /// Absence needs no name removal. A replacement observed before unlink is
    /// left untouched. A surviving owned-inode link after unlink is an error;
    /// this cannot make check-and-unlink atomic or restore a removed replacement.
    fn cleanup(&mut self) -> std::io::Result<()> {
        let Some(name) = self.name.as_ref() else {
            return Ok(());
        };
        let removal = ensure_entry_identity(self.directory, name, &self.file)
            .and_then(|()| unlink_at(self.directory, name, 0))
            .and_then(|()| require_unlinked_staging_inode(file_stat(&self.file)?.st_nlink));
        match removal {
            Ok(()) => {
                self.name = None;
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.name = None;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    /// Diagnose an abort while this owner still knows the exact temporary entry.
    ///
    /// Only one named staging inode is owned at a time. Anonymous-to-named
    /// fallback leaves the old source anonymous, so an outer abort cannot lose
    /// a second named cleanup failure from the fallback owner.
    fn cleanup_failure(&mut self) -> Option<CleanupFailure> {
        let temporary = self.name.as_ref()?.to_string_lossy().into_owned();
        self.cleanup()
            .err()
            .map(|source| CleanupFailure { temporary, source })
    }

    fn abort_commit(&mut self, mut primary: CommitFailure) -> CommitFailure {
        primary.cleanup = self.cleanup_failure().or(primary.cleanup);
        primary
    }

    fn abort(&mut self, primary: anyhow::Error) -> anyhow::Error {
        match self.cleanup_failure() {
            Some(cleanup) => StagingAbortFailure { primary, cleanup }.into(),
            None => primary,
        }
    }
}

impl Drop for StagedFile<'_> {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

/// Generate an unpredictable temporary component without reusing a fixed `.tmp` path.
///
/// Randomness reduces collisions; exclusive creation/linking establishes actual
/// ownership. Failure to obtain random bytes is an error, not permission to use
/// a predictable alternative that might name someone else's existing inode.
fn random_staging_name() -> std::io::Result<CString> {
    let mut random = [0u8; 16];
    let mut filled = 0;
    while filled < random.len() {
        // SAFETY: getrandom writes at most the supplied remaining byte count to
        // the live array and retains no pointer.
        let result = unsafe {
            libc::syscall(
                libc::SYS_getrandom,
                random[filled..].as_mut_ptr(),
                random.len() - filled,
                0,
            )
        };
        if result > 0 {
            filled += result as usize;
        } else if result == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "getrandom returned no bytes",
            ));
        } else {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
    let mut name = String::from(".rundelta-tmp-");
    for byte in random {
        use std::fmt::Write as _;
        write!(&mut name, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(CString::new(name).expect("generated staging names contain no NUL"))
}

fn tmpfile_is_unsupported(error: &std::io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(code)
            if code == libc::EOPNOTSUPP
                || code == libc::EINVAL
                || code == libc::ENOSYS
                || code == libc::EISDIR
    )
}

fn tmpfile_link_is_unsupported(error: &std::io::Error) -> bool {
    // Some old or restricted kernel/filesystem combinations accept O_TMPFILE
    // but cannot materialize it through AT_EMPTY_PATH and report ENOENT. At
    // this point only the anonymous inode exists, so retrying with random
    // O_EXCL is safe. ENOENT from the initial O_TMPFILE open is intentionally
    // not accepted here: a missing/unreachable held directory fails closed.
    tmpfile_is_unsupported(error)
        || matches!(
            error.raw_os_error(),
            Some(code) if code == libc::EPERM || code == libc::EXDEV || code == libc::ENOENT
        )
}

/// Allocate a fresh O_EXCL temporary inode, retrying only random-name collisions.
fn create_named_staging(directory: &File) -> std::io::Result<StagedFile<'_>> {
    for _ in 0..32 {
        let name = random_staging_name()?;
        match openat_beneath(
            directory,
            &name,
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC,
            0o600,
        ) {
            Ok(file) => {
                return Ok(StagedFile {
                    directory,
                    file,
                    name: Some(name),
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique random staging name",
    ))
}

/// Prefer an unnamed inode; fall back only for recognized unsupported O_TMPFILE cases.
///
/// Permission, missing-directory and other I/O failures remain errors rather than
/// being hidden by an alternate writer path. Neither path modifies a preexisting
/// fixed temporary leaf.
fn create_staging(directory: &File, force_named: bool) -> std::io::Result<StagedFile<'_>> {
    if !force_named {
        match openat_basic(
            directory,
            c".",
            libc::O_RDWR | libc::O_TMPFILE | libc::O_CLOEXEC,
            0o600,
        ) {
            Ok(file) => {
                return Ok(StagedFile {
                    directory,
                    file,
                    name: None,
                });
            }
            Err(error) if tmpfile_is_unsupported(&error) => {}
            Err(error) => return Err(error),
        }
    }
    create_named_staging(directory)
}

/// Give an already written anonymous inode a fresh name needed for atomic rename.
fn link_anonymous_staging(staged: &mut StagedFile<'_>) -> std::io::Result<()> {
    debug_assert!(staged.name.is_none());
    for _ in 0..32 {
        let name = random_staging_name()?;
        // SAFETY: the anonymous file and directory descriptors are live, both
        // C strings remain valid, and linkat retains no pointer.
        let result = unsafe {
            libc::linkat(
                staged.file.as_raw_fd(),
                c"".as_ptr(),
                staged.directory.as_raw_fd(),
                name.as_ptr(),
                libc::AT_EMPTY_PATH,
            )
        };
        if result == 0 {
            staged.name = Some(name);
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(error);
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not link an anonymous file under a unique staging name",
    ))
}

/// Reject a detected temporary-name swap before renaming that leaf into the destination.
fn verify_staging_identity(staged: &StagedFile<'_>) -> std::io::Result<()> {
    let name = staged.name.as_ref().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "anonymous staging file has no publishable directory entry",
        )
    })?;
    ensure_entry_identity(staged.directory, name, &staged.file)
}

fn verify_staging_for_publish(
    staged: &StagedFile<'_>,
    file: &'static str,
) -> std::result::Result<(), CommitFailure> {
    verify_staging_identity(staged).map_err(|error| {
        CommitFailure::new(
            file,
            "verify temporary file identity",
            CommitStage::BeforePublish,
            error,
        )
    })
}

fn ordinary_component(name: &str) -> std::io::Result<CString> {
    let name = CString::new(name).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "storage file name contains NUL",
        )
    })?;
    let bytes = name.to_bytes();
    if bytes.is_empty() || bytes == b"." || bytes == b".." || bytes.contains(&b'/') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "storage file name is not one ordinary path component",
        ));
    }
    Ok(name)
}

/// Single-use staging authority for one predetermined destination component.
///
/// The destination is separate from the temporary name: cleanup must never treat
/// a successfully published record file as an unpublished temporary file.
struct AtomicStaging<'a> {
    staged: StagedFile<'a>,
    published: CString,
    display: &'static str,
    control: AtomicControl,
}

/// Retain the published inode for post-rename identity and byte verification.
struct PublishedFile {
    file: File,
    stage: CommitStage,
}

/// Allocate and validate write authority before any destination replacement.
fn begin_atomic_replace_with_control<'a>(
    directory: &'a File,
    name: &'static str,
    control: AtomicControl,
) -> std::result::Result<AtomicStaging<'a>, CommitFailure> {
    let published = ordinary_component(name).map_err(|error| {
        CommitFailure::new(
            name,
            "validate destination",
            CommitStage::BeforePublish,
            error,
        )
    })?;
    let mut staged = create_staging(directory, control.force_named_fallback).map_err(|error| {
        CommitFailure::new(
            name,
            "create temporary file",
            CommitStage::BeforePublish,
            error,
        )
    })?;
    control.check(name, AtomicStep::Write).map_err(|error| {
        staged.abort_commit(CommitFailure::new(
            name,
            "write temporary file",
            CommitStage::BeforePublish,
            error,
        ))
    })?;
    Ok(AtomicStaging {
        staged,
        published,
        display: name,
        control,
    })
}

/// Copy a fresh staging inode using bounded scratch space, not a total-byte quota.
fn copy_file_bounded(source: &mut File, destination: &mut File) -> std::io::Result<()> {
    source.seek(SeekFrom::Start(0))?;
    destination.seek(SeekFrom::Start(0))?;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            return Ok(());
        }
        destination.write_all(&buffer[..read])?;
    }
}

/// Recover from unsupported anonymous linking while still before publication.
///
/// Only newly owned temporary inodes participate. The copied fallback must be
/// synced separately; syncing the source does not persist the copied destination.
fn copy_anonymous_to_named<'a>(
    source: &mut StagedFile<'a>,
    display: &'static str,
    control: AtomicControl,
) -> std::result::Result<StagedFile<'a>, CommitFailure> {
    let mut named = create_named_staging(source.directory).map_err(|error| {
        CommitFailure::new(
            display,
            "create named temporary file",
            CommitStage::BeforePublish,
            error,
        )
    })?;
    control
        .check(display, AtomicStep::FallbackCopy)
        .and_then(|()| copy_file_bounded(&mut source.file, &mut named.file))
        .map_err(|error| {
            named.abort_commit(CommitFailure::new(
                display,
                "copy anonymous temporary file to named fallback",
                CommitStage::BeforePublish,
                error,
            ))
        })?;
    control
        .check(display, AtomicStep::FallbackFileSync)
        .and_then(|()| named.file.sync_all())
        .map_err(|error| {
            named.abort_commit(CommitFailure::new(
                display,
                "sync named fallback temporary file",
                CommitStage::BeforePublish,
                error,
            ))
        })?;
    Ok(named)
}

impl AtomicStaging<'_> {
    fn write_all(&mut self, data: &[u8]) -> std::result::Result<(), CommitFailure> {
        self.staged.file.write_all(data).map_err(|error| {
            self.staged.abort_commit(CommitFailure::new(
                self.display,
                "write temporary file",
                CommitStage::BeforePublish,
                error,
            ))
        })
    }

    /// Consume staging authority through file sync, name/identity checks, rename and directory sync.
    ///
    /// 中文阅读提示：rename 是可见性边界；之后目录 sync 失败不能说成“尚未写入”。
    /// retained fd 用于提交后复核，不能只凭它仍可读就认定目标名字仍指向同一 inode。
    fn publish(mut self) -> std::result::Result<PublishedFile, CommitFailure> {
        self.publish_inner()
            .map_err(|error| self.staged.abort_commit(error))
    }

    /// Publication clears the temporary name at rename, before any later error.
    /// The outer abort therefore cannot unlink an already published destination.
    fn publish_inner(&mut self) -> std::result::Result<PublishedFile, CommitFailure> {
        self.control
            .check(self.display, AtomicStep::FileSync)
            .and_then(|()| self.staged.file.sync_all())
            .map_err(|error| {
                CommitFailure::new(
                    self.display,
                    "sync temporary file",
                    CommitStage::BeforePublish,
                    error,
                )
            })?;

        if self.staged.name.is_none() {
            self.control
                .check(self.display, AtomicStep::Link)
                .map_err(|error| {
                    CommitFailure::new(
                        self.display,
                        "link anonymous temporary file",
                        CommitStage::BeforePublish,
                        error,
                    )
                })?;
            #[cfg(test)]
            let link_result = if self.control.force_link_unsupported {
                Err(std::io::Error::from_raw_os_error(libc::EPERM))
            } else {
                link_anonymous_staging(&mut self.staged)
            };
            #[cfg(not(test))]
            let link_result = link_anonymous_staging(&mut self.staged);
            if let Err(error) = link_result {
                if !tmpfile_link_is_unsupported(&error) {
                    return Err(CommitFailure::new(
                        self.display,
                        "link anonymous temporary file",
                        CommitStage::BeforePublish,
                        error,
                    ));
                }

                self.staged =
                    copy_anonymous_to_named(&mut self.staged, self.display, self.control)?;
            }
        }

        let temporary = self
            .staged
            .name
            .as_ref()
            .expect("every publishable staging file has a directory entry");
        verify_staging_for_publish(&self.staged, self.display)?;
        let published_file = self.staged.file.try_clone().map_err(|error| {
            CommitFailure::new(
                self.display,
                "retain temporary file handle",
                CommitStage::BeforePublish,
                error,
            )
        })?;
        self.control
            .check(self.display, AtomicStep::Rename)
            .map_err(|error| {
                CommitFailure::new(
                    self.display,
                    "rename temporary file",
                    CommitStage::BeforePublish,
                    error,
                )
            })?;
        rename_at(
            self.staged.directory,
            temporary,
            self.staged.directory,
            &self.published,
        )
        .map_err(|error| {
            CommitFailure::new(
                self.display,
                "rename temporary file",
                CommitStage::BeforePublish,
                error,
            )
        })?;
        // rename consumed the temporary directory entry; cleanup must not unlink
        // the newly published destination even when directory sync later fails.
        self.staged.name = None;
        self.control
            .check(self.display, AtomicStep::DirectorySync)
            .and_then(|()| self.staged.directory.sync_all())
            .map_err(|error| {
                CommitFailure::new(
                    self.display,
                    "sync containing directory",
                    CommitStage::PublishedDurabilityUnknown,
                    error,
                )
            })?;
        Ok(PublishedFile {
            file: published_file,
            stage: CommitStage::Durable,
        })
    }
}

#[derive(Debug)]
/// Byte fingerprint and limited contract facts measured from one event stream.
struct EventFileSummary {
    check: FileCheck,
    facts: EventFacts,
}

/// Serialize/hash events incrementally while retaining one atomic publish authority.
///
/// This avoids a second whole JSONL buffer; the caller's event slice still exists,
/// and each serialized line must fit the same budget enforced by readers.
struct EventJsonlStaging<'a> {
    atomic: AtomicStaging<'a>,
    hasher: Sha256,
    bytes: u64,
    facts: EventFacts,
}

impl<'a> EventJsonlStaging<'a> {
    fn begin(directory: &'a File, control: AtomicControl) -> Result<Self> {
        Ok(Self {
            atomic: begin_atomic_replace_with_control(directory, "events.jsonl", control)?,
            hasher: Sha256::new(),
            bytes: 0,
            facts: EventFacts::default(),
        })
    }

    fn push(&mut self, event: &Event) -> Result<()> {
        self.push_inner(event)
            .map_err(|error| self.atomic.staged.abort(error))
    }

    /// Keep bounded serialization and byte/fact updates separate from abort.
    /// Failed serialization never advances the staged summary or publishes it.
    fn push_inner(&mut self, event: &Event) -> Result<()> {
        let mut line =
            serialize_bounded(event, MAX_EVENT_LINE_BYTES - 1, "events.jsonl line", false)?;
        append_bounded(&mut line, b"\n", MAX_EVENT_LINE_BYTES, "events.jsonl line")
            .map_err(|error| unavailable_io("events.jsonl", error))?;
        let bytes = checked_byte_count(self.bytes, line.len())?;
        if bytes > MAX_EVENT_FILE_BYTES || self.facts.count() >= MAX_EVENT_COUNT {
            return Err(unavailable_io(
                "events.jsonl",
                resource_limit(
                    "events.jsonl",
                    "bytes/events",
                    format!("{MAX_EVENT_FILE_BYTES}/{MAX_EVENT_COUNT}"),
                ),
            ));
        }
        self.atomic.write_all(&line)?;
        self.bytes = bytes;
        self.hasher.update(&line);
        self.facts.observe(event);
        Ok(())
    }

    fn publish(self) -> Result<(PublishedFile, EventFileSummary)> {
        let summary = EventFileSummary {
            check: FileCheck {
                bytes: self.bytes,
                sha256: format!("{:x}", self.hasher.finalize()),
            },
            facts: self.facts,
        };
        Ok((self.atomic.publish()?, summary))
    }
}

fn atomic_replace_with_control(
    directory: &File,
    name: &'static str,
    data: &[u8],
    control: AtomicControl,
) -> std::result::Result<CommitStage, CommitFailure> {
    let mut staging = begin_atomic_replace_with_control(directory, name, control)?;
    staging.write_all(data)?;
    staging.publish().map(|published| published.stage)
}

fn atomic_replace(
    directory: &File,
    name: &'static str,
    data: &[u8],
) -> std::result::Result<CommitStage, CommitFailure> {
    atomic_replace_with_control(directory, name, data, AtomicControl::default())
}

/// Check metadata facts and atomically replace its bytes, without deciding finalization.
fn metadata_at(directory: &File, metadata: &Metadata) -> Result<()> {
    validate_metadata_contract(metadata)
        .map_err(|error| stored_contract_error("invalid metadata contract", error))?;
    atomic_replace(
        directory,
        "metadata.json",
        &serialize_bounded(metadata, MAX_METADATA_BYTES, "metadata.json", true)?,
    )?;
    Ok(())
}

/// Decode one explicitly supported event schema; preserve saved coordinates verbatim.
///
/// Bounded DTO decoding precedes the versioned model shape check in [`EventFacts`].
/// Older metadata never inherits current shape requirements on a compatibility read.
fn decode_event_line(line: &[u8], line_number: usize) -> Result<Event> {
    let display = format!("events.jsonl:{line_number}");
    if line.len() >= MAX_EVENT_LINE_BYTES {
        return Err(unavailable_io(
            &display,
            resource_limit(&display, "line bytes including LF", MAX_EVENT_LINE_BYTES),
        ));
    }
    check_json_budget(line, &display).map_err(|error| unavailable_io(&display, error))?;
    let schema_version = decode_schema_header(line, &display)?;
    if !matches!(schema_version, 1..=3) {
        return Err(unsupported(format!(
            "unsupported event schema {schema_version} at line {line_number}"
        )));
    }
    serde_json::from_slice(line).map_err(|error| corrupt(format!("{display}: {error}")))
}

#[cfg(test)]
fn decode_events(data: &[u8]) -> Result<Vec<Event>> {
    let raw = std::str::from_utf8(data).map_err(|e| corrupt(format!("events.jsonl: {e}")))?;
    if !raw.is_empty() && !raw.ends_with('\n') {
        return Err(corrupt("events.jsonl: incomplete final line"));
    }
    let mut events = vec![];
    for (n, line) in raw.lines().enumerate() {
        events.push(decode_event_line(line.as_bytes(), n + 1)?);
    }
    Ok(events)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Share decoding/integrity checks while choosing whether to retain event DTOs.
enum EventReadMode {
    CollectEvents,
    ValidateOnly,
}

#[derive(Debug)]
/// Successful decode facts plus optional materialization for show/diff callers.
struct DecodedEventStream {
    facts: EventFacts,
    events: Option<Vec<Event>>,
}

/// Keep byte verification available even when interpreting those bytes failed.
///
/// Separating the two results lets marker corruption outrank a misleading
/// unsupported-schema/JSON error found inside bytes that do not match the marker.
struct EventStreamReadback {
    check: FileCheck,
    decoded: Result<DecodedEventStream>,
}

/// Hash every JSONL byte while collecting supported DTO facts or validate-only summaries.
///
/// UTF-8, termination and decode errors are deferred until the checksum is known.
/// After a decode failure the partial event vector is discarded, but the remaining
/// bytes are still hashed. An actual read/fstat error instead returns immediately.
/// Byte, line, JSON-value and event-count budgets apply before DTO growth;
/// collect-mode Vec capacity is reserved fallibly. Resource failure is unavailable.
fn stream_events(file: &mut File, mode: EventReadMode) -> std::io::Result<EventStreamReadback> {
    let before = file_stat(file)?;
    if !is_regular(&before) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "not a regular file",
        ));
    }
    let expected_length = stat_length(&before)?;
    if expected_length > MAX_EVENT_FILE_BYTES {
        return Err(resource_limit(
            "events.jsonl",
            "bytes",
            MAX_EVENT_FILE_BYTES,
        ));
    }
    file.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut buffer = Vec::new();
    let mut hasher = Sha256::new();
    let mut bytes = 0u64;
    let mut facts = EventFacts::default();
    let mut first_utf8_error = None;
    let mut first_decode_error = None;
    let mut saw_bytes = false;
    let mut final_line_terminated = true;
    let mut line_number = 0usize;
    let mut events = match mode {
        EventReadMode::CollectEvents => Some(Vec::new()),
        EventReadMode::ValidateOnly => None,
    };

    loop {
        if read_bounded_line(&mut reader, &mut buffer, MAX_EVENT_LINE_BYTES)? == 0 {
            break;
        }
        saw_bytes = true;
        line_number += 1;
        if line_number > MAX_EVENT_COUNT {
            return Err(resource_limit("events.jsonl", "events", MAX_EVENT_COUNT));
        }
        bytes = checked_byte_count(bytes, buffer.len())?;
        if bytes > MAX_EVENT_FILE_BYTES {
            return Err(resource_limit(
                "events.jsonl",
                "bytes",
                MAX_EVENT_FILE_BYTES,
            ));
        }
        hasher.update(&buffer);
        final_line_terminated = buffer.ends_with(b"\n");

        let raw_line = match std::str::from_utf8(&buffer) {
            Ok(line) => line,
            Err(error) => {
                if first_utf8_error.is_none() {
                    first_utf8_error = Some(corrupt(format!("events.jsonl: {error}")));
                }
                events = None;
                continue;
            }
        };
        // An incomplete final line has priority over JSON/schema decoding, as
        // in the compatibility decoder. Defer all errors until the checksum is
        // known so damaged bytes cannot be misclassified as schema failures.
        let Some(raw_line) = raw_line.strip_suffix('\n') else {
            continue;
        };
        let raw_line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        match decode_event_line(raw_line.as_bytes(), line_number) {
            Ok(event) => {
                facts.observe(&event);
                if let Some(events) = events.as_mut() {
                    reserve_bounded(
                        events,
                        events.len() + 1,
                        MAX_EVENT_COUNT,
                        "events.jsonl events",
                    )?;
                    events.push(event);
                }
            }
            // A decoder resource failure is not another malformed line to
            // defer: keeping an earlier JSON/schema error would mislabel the
            // operation's inability to allocate/interpret the bounded DTO.
            Err(error)
                if storage_load_failure_kind(&error) == StorageLoadFailureKind::Unavailable =>
            {
                return Err(std::io::Error::other(error));
            }
            Err(error) if first_decode_error.is_none() => {
                first_decode_error = Some(error);
                events = None;
            }
            Err(_) => {}
        }
    }

    let after = file_stat(reader.get_ref())?;
    if !same_file(&before, &after)
        || stat_length(&after)? != expected_length
        || bytes != expected_length
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "regular file changed while streaming",
        ));
    }

    let decoded = if let Some(error) = first_utf8_error {
        Err(error)
    } else if saw_bytes && !final_line_terminated {
        Err(corrupt("events.jsonl: incomplete final line"))
    } else if let Some(error) = first_decode_error {
        Err(error)
    } else {
        Ok(DecodedEventStream { facts, events })
    };
    Ok(EventStreamReadback {
        check: FileCheck {
            bytes,
            sha256: format!("{:x}", hasher.finalize()),
        },
        decoded,
    })
}

/// Verify publication facts without allocating another complete event vector.
fn stream_event_readback(file: &mut File) -> std::io::Result<EventStreamReadback> {
    stream_events(file, EventReadMode::ValidateOnly)
}

#[derive(Debug)]
/// Versions and aggregate event facts used to reject unsupported current tuples.
///
/// Legacy readable combinations remain legacy: accepting their formats does not
/// strengthen their saved completeness or promote their integrity classification.
struct SchemaTuple {
    metadata: u32,
    events: EventFacts,
    marker: Option<u32>,
}

impl SchemaTuple {
    fn finalized(metadata: u32, events: EventFacts, marker: u32) -> Self {
        Self {
            metadata,
            events,
            marker: Some(marker),
        }
    }
}

/// Apply supported version boundaries without silently mixing protocol tuples.
fn validate_schema_tuple(tuple: &SchemaTuple) -> Result<()> {
    if !matches!(tuple.metadata, 1..=4) {
        return Err(unsupported(format!(
            "unsupported metadata schema {}",
            tuple.metadata
        )));
    }
    if let Some(marker) = tuple.marker
        && !matches!(marker, 1 | 2)
    {
        return Err(unsupported(format!(
            "unsupported finalization marker schema {marker}"
        )));
    }
    if let Some(version) = tuple.events.first_unsupported_schema() {
        return Err(unsupported(format!("unsupported event schema {version}")));
    }
    if tuple.metadata == 3 && tuple.events.has_non_v2() {
        return Err(corrupt(
            "current metadata schema requires only event schema 2",
        ));
    }
    if tuple.metadata == 4 && tuple.events.has_non_v3() {
        return Err(corrupt("trace-v1 metadata requires only event schema 3"));
    }
    if tuple.metadata <= 3 && tuple.events.has_v3() {
        return Err(corrupt(
            "legacy metadata cannot adopt trace-v1 event schema 3",
        ));
    }
    if let Some(marker) = tuple.marker
        && ((tuple.metadata == 4) != (marker == 2))
    {
        return Err(corrupt(
            "metadata and finalization marker versions contradict",
        ));
    }
    Ok(())
}

#[cfg(test)]
fn finalized_integrity(metadata_schema: u32, events: &[Event]) -> Result<StorageIntegrity> {
    finalized_integrity_from_facts(metadata_schema, EventFacts::from_events(events))
}

/// Classify already checked marker bytes without giving legacy metadata new trust.
///
/// This helper checks tuple compatibility; its callers perform the byte/count
/// verification. It does not inspect raw evidence or determine capture reliability.
#[cfg(test)]
fn finalized_integrity_from_facts(
    metadata_schema: u32,
    events: EventFacts,
) -> Result<StorageIntegrity> {
    finalized_integrity_with_marker(metadata_schema, events, 1)
}

fn finalized_integrity_with_marker(
    metadata_schema: u32,
    events: EventFacts,
    marker_schema: u32,
) -> Result<StorageIntegrity> {
    validate_schema_tuple(&SchemaTuple::finalized(
        metadata_schema,
        events,
        marker_schema,
    ))?;
    if metadata_schema == 4 {
        return Ok(StorageIntegrity {
            status: "verified".into(),
            detail: "metadata, events and raw manifest match finalized byte counts, event count and SHA-256; raw integrity is classified separately".into(),
        });
    }
    if metadata_schema == 3 {
        return Ok(StorageIntegrity {
            status: "verified".into(),
            detail: "metadata and events match finalized byte counts, event count and SHA-256; raw logs are not checksummed".into(),
        });
    }
    Ok(StorageIntegrity {
        status: "unverified".into(),
        detail: "storage integrity unverified: legacy metadata cannot be promoted by a finalization marker; marker byte checks passed"
            .into(),
    })
}

/// Final marker is the last atomic write; it describes storage finalization, not capture success.
/// Test compatibility finalizer for the legacy path-based oracle.
#[cfg(test)]
fn finish(dir: &Path, m: &Metadata, events: &[Event]) -> Result<()> {
    let directory =
        open_directory(dir).map_err(|error| unavailable_io("open record directory", error))?;
    finish_at(&directory, m, events)
}

#[cfg(test)]
fn finish_at(directory: &File, m: &Metadata, events: &[Event]) -> Result<()> {
    finish_at_with_control(directory, m, events, AtomicControl::default())
}

/// Reopen a published metadata leaf and compare its bytes with the serialized input.
///
/// The supplied stage preserves whether a read-back failure followed a completed
/// rename/sync; equality of bytes here is not a new claim about target behavior.
fn read_back_exact(
    directory: &File,
    name: &'static str,
    component: &CStr,
    expected: &[u8],
    stage: CommitStage,
    control: AtomicControl,
) -> std::result::Result<Vec<u8>, CommitFailure> {
    control
        .check(name, AtomicStep::ReadBack)
        .map_err(|error| CommitFailure::new(name, "read back published file", stage, error))?;
    let actual = read_file_at(directory, component, MAX_METADATA_BYTES)
        .map_err(|error| CommitFailure::new(name, "read back published file", stage, error))?;
    if actual != expected {
        return Err(CommitFailure::new(
            name,
            "verify read-back bytes",
            stage,
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "published bytes differ from the staged bytes",
            ),
        ));
    }
    Ok(actual)
}

/// Bind the published event name back to its retained inode and measured stream facts.
///
/// Both name identity and byte/fact agreement matter: the retained fd alone could
/// keep reading the old inode after an external process replaced the visible leaf.
/// This verification still has the semantic limits of [`EventFacts`].
fn read_back_published_events(
    directory: &File,
    published: &PublishedFile,
    expected: &EventFileSummary,
    control: AtomicControl,
) -> Result<EventFileSummary> {
    let stage = published.stage;
    control
        .check("events.jsonl", AtomicStep::ReadBack)
        .map_err(|error| {
            CommitFailure::new("events.jsonl", "read back published file", stage, error)
        })?;

    // Open the published component rather than trusting the pre-rename handle.
    // Both handles must identify the same regular inode before and after the
    // read, otherwise a same-UID replacement is rejected before a marker can
    // describe bytes that are no longer reachable by the record name.
    let mut opened = openat_beneath(
        directory,
        c"events.jsonl",
        libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
        0,
    )
    .map_err(|error| {
        CommitFailure::new("events.jsonl", "read back published file", stage, error)
    })?;
    let opened_stat = file_stat(&opened).map_err(|error| {
        CommitFailure::new("events.jsonl", "inspect published file", stage, error)
    })?;
    let held_stat = file_stat(&published.file).map_err(|error| {
        CommitFailure::new("events.jsonl", "inspect retained file", stage, error)
    })?;
    if !is_regular(&opened_stat) || !same_file(&opened_stat, &held_stat) {
        return Err(CommitFailure::new(
            "events.jsonl",
            "verify published file identity",
            stage,
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "published file no longer identifies the staged regular file",
            ),
        )
        .into());
    }
    ensure_entry_identity(directory, c"events.jsonl", &published.file).map_err(|error| {
        CommitFailure::new(
            "events.jsonl",
            "verify published file identity",
            stage,
            error,
        )
    })?;

    let readback = stream_event_readback(&mut opened).map_err(|error| {
        CommitFailure::new("events.jsonl", "read back published file", stage, error)
    })?;
    ensure_entry_identity(directory, c"events.jsonl", &published.file).map_err(|error| {
        CommitFailure::new(
            "events.jsonl",
            "verify published file identity",
            stage,
            error,
        )
    })?;
    let opened_after = file_stat(&opened).map_err(|error| {
        CommitFailure::new("events.jsonl", "inspect published file", stage, error)
    })?;
    let held_after = file_stat(&published.file).map_err(|error| {
        CommitFailure::new("events.jsonl", "inspect retained file", stage, error)
    })?;
    if !same_file(&opened_after, &held_after) {
        return Err(CommitFailure::new(
            "events.jsonl",
            "verify published file identity",
            stage,
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "published file identity changed during read-back",
            ),
        )
        .into());
    }
    if readback.check != expected.check {
        return Err(CommitFailure::new(
            "events.jsonl",
            "verify read-back bytes",
            stage,
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "published bytes differ from the staged bytes",
            ),
        )
        .into());
    }
    let decoded = readback.decoded?;
    let facts = decoded.facts;
    if facts != expected.facts {
        return Err(CommitFailure::new(
            "events.jsonl",
            "verify read-back event facts",
            stage,
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "published event facts differ from the staged events",
            ),
        )
        .into());
    }
    Ok(EventFileSummary {
        check: readback.check,
        facts,
    })
}

/// Compatibility/test entry point for the default structured-only protocol.
#[cfg(test)]
fn finish_at_with_control(
    directory: &File,
    m: &Metadata,
    events: &[Event],
    control: AtomicControl,
) -> Result<()> {
    finish_at_with_manifest_control(directory, m, events, None, control)
}

/// Publish events -> metadata -> optional raw receipt -> marker, preserving failure stages.
///
/// 中文阅读提示：先验证合同和实际消费的 raw 收据，再发布并复读结构化文件；trace-v1
/// 还发布并复读 manifest，复核 raw 字节之后最后写 marker。前序失败不合成 marker，
/// 不回滚已公开文件；marker 发布错误也保留阶段，不能解释为整个记录从未可见。
/// 这不是防外部同权限修改的事务，不证明采集成功、来源认证或断电恢复。
fn finish_at_with_manifest_control(
    directory: &File,
    m: &Metadata,
    events: &[Event],
    manifest: Option<&RawManifest>,
    control: AtomicControl,
) -> Result<()> {
    let valid_tuple = match m.schema_version {
        3 => manifest.is_none() && events.iter().all(|event| event.schema_version == 2),
        4 => manifest.is_some() && events.iter().all(|event| event.schema_version == 3),
        _ => false,
    };
    if !valid_tuple {
        bail!(
            "writer requires metadata/events/marker tuple 3/2/1 or trace-v1 tuple 4/3/2 with a raw manifest"
        );
    }
    if events.len() > MAX_EVENT_COUNT {
        return Err(unavailable_io(
            "events.jsonl",
            resource_limit("events.jsonl", "events", MAX_EVENT_COUNT),
        ));
    }
    validate_run_contract(m, events, true)
        .map_err(|error| stored_contract_error("cannot finalize inconsistent metadata", error))?;
    // Reject an unwritable metadata budget before even publishing the event leaf.
    let metadata_bytes = serialize_bounded(m, MAX_METADATA_BYTES, "metadata.json", true)?;
    let manifest_bytes = manifest
        .map(|manifest| {
            crate::model::validate_raw_manifest(manifest, m)
                .map_err(|error| stored_contract_error("raw manifest contract", error))?;
            verify_raw_receipt_at(directory, manifest)?;
            serialize_bounded(manifest, MAX_MARKER_BYTES, "raw-integrity.json", true)
        })
        .transpose()?;
    let mut event_staging = EventJsonlStaging::begin(directory, control)?;
    for event in events {
        event_staging.push(event)?;
    }
    let (published_events, staged_events) = event_staging.publish()?;
    let reread_events =
        read_back_published_events(directory, &published_events, &staged_events, control)?;
    validate_run_contract_facts(m, &reread_events.facts, true)
        .map_err(|error| stored_contract_error("cannot finalize inconsistent metadata", error))?;
    if reread_events.facts.count() != m.parsed_events {
        return Err(CommitFailure::new(
            "events.jsonl",
            "verify read-back event count",
            published_events.stage,
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "published event count differs from metadata",
            ),
        )
        .into());
    }
    let metadata_stage =
        atomic_replace_with_control(directory, "metadata.json", &metadata_bytes, control)?;
    let meta = read_back_exact(
        directory,
        "metadata.json",
        c"metadata.json",
        &metadata_bytes,
        metadata_stage,
        control,
    )?;
    let raw_manifest_check = match (manifest, manifest_bytes) {
        (Some(manifest), Some(bytes)) => {
            let stage =
                atomic_replace_with_control(directory, "raw-integrity.json", &bytes, control)?;
            let actual = read_back_exact(
                directory,
                "raw-integrity.json",
                c"raw-integrity.json",
                &bytes,
                stage,
                control,
            )?;
            // Verify the saved raw leaf once more after structured publication.
            // This compares with the consumption digest, never updates it.
            control
                .check("raw/trace.log", AtomicStep::ReadBack)
                .map_err(|error| unavailable_io("verify raw before final marker", error))?;
            verify_raw_receipt_at(directory, manifest)?;
            Some(check(&actual))
        }
        (None, None) => None,
        _ => unreachable!("manifest bytes follow the validated tuple"),
    };
    let marker = Finalized {
        schema_version: if m.schema_version == 4 { 2 } else { 1 },
        run_id: m.id.clone(),
        event_count: reread_events.facts.count(),
        events: reread_events.check,
        metadata: check(&meta),
        raw_manifest: raw_manifest_check,
    };
    atomic_replace_with_control(
        directory,
        "finalized.json",
        &serialize_bounded(&marker, MAX_MARKER_BYTES, "finalized.json", true)?,
        control,
    )?;
    Ok(())
}
impl StorageRoot {
    /// Load one exact, uniquely resolved run from this anchored storage root.
    fn load(&self, key: &str) -> Result<Run> {
        let mut e = self.resolve(key)?;
        if let Some(error) = e.open_error.take() {
            return Err(error).with_context(|| e.directory);
        }
        let id =
            e.id.as_deref()
                .ok_or_else(|| unavailable("unsafe record directory"))?;
        let run_dir = e
            .run_dir
            .ok_or_else(|| unavailable(format!("unsafe record directory: {}", e.directory)))?;
        read_run_at(&run_dir, id).with_context(|| e.directory)
    }

    /// List record facts while isolating damage to the affected row.
    ///
    /// Older tuples stream checksums, schemas, counts and cross-file facts in
    /// validate-only mode. The trace-v1 marker-v2 path temporarily collects one
    /// record's events for the same pure generation/provenance validation used
    /// by full loads. Summary construction discards that vector; returned rows
    /// retain metadata/component facts, never events. Existing per-record
    /// resource budgets apply, but do not guarantee recovery from arbitrary
    /// allocation failure; index and summary totals still grow with record count.
    /// Returned per-entry load errors become unreadable rows; inability to enumerate the index
    /// remains an operation error because the omitted entries cannot be known.
    fn records(&self) -> Result<Vec<RecordSummary>> {
        let mut records = Vec::new();
        self.scan(|e| {
            let Entry {
                run_dir,
                directory,
                id,
                name,
                open_error,
                index_error: _,
            } = e;
            let loaded = match (open_error, run_dir, id.as_deref()) {
                (Some(error), _, _) => Err(error),
                (None, Some(run_dir), Some(id)) => {
                    read_record_at(&run_dir, id, EventReadMode::ValidateOnly)
                }
                (None, _, _) => Err(unavailable("unsafe record directory")),
            };
            let summary = match loaded {
                Ok(record) => RecordSummary::Readable {
                    metadata: Box::new(record.metadata),
                    directory,
                    storage_integrity: record.storage_integrity,
                    raw_integrity: record.raw_integrity,
                },
                Err(error) => RecordSummary::Unreadable {
                    id,
                    name,
                    directory,
                    storage_integrity: StorageIntegrity {
                        status: match storage_load_failure_kind(&error) {
                            StorageLoadFailureKind::Corrupt => "corrupt",
                            StorageLoadFailureKind::Unsupported => "unsupported",
                            StorageLoadFailureKind::Unavailable | StorageLoadFailureKind::Busy => {
                                "unavailable"
                            }
                        }
                        .into(),
                        detail: format!("{error:#}"),
                    },
                },
            };
            records.push(summary);
            Ok(())
        })?;
        Ok(records)
    }
}

/// Load and validate one exact record ID or label.
pub(crate) fn load(root: &Path, key: &str) -> Result<Run> {
    let Some(storage) = open_existing(root)? else {
        return Err(selection_error(key, 0));
    };
    storage.load(key)
}

/// Load one run while hiding concrete persistence and operating-system errors.
pub(crate) fn load_outcome(root: &Path, key: &str) -> LoadOutcome<Run> {
    match load(root, key) {
        Ok(run) => LoadOutcome::Loaded(run),
        Err(error) => LoadOutcome::Failed(neutral_load_failure(error)),
    }
}

/// Isolate returned per-record load errors from other listing rows. Preserve flat
/// metadata fields for readable records; unreadable rows expose only hints/status.
/// Root/index enumeration errors still fail the operation rather than returning
/// a misleading partial inventory. No row is repaired or deleted while listing.
pub(crate) fn records(root: &Path) -> Result<Vec<RecordSummary>> {
    let Some(storage) = open_existing(root)? else {
        return Ok(vec![]);
    };
    storage.records()
}
/// Internal load result shared by full materialization and validate-only listing.
struct LoadedRecord {
    metadata: Metadata,
    events: Option<Vec<Event>>,
    storage_integrity: StorageIntegrity,
    raw_integrity: Option<RawIntegrity>,
}

fn empty_events(mode: EventReadMode) -> Option<Vec<Event>> {
    match mode {
        EventReadMode::CollectEvents => Some(Vec::new()),
        EventReadMode::ValidateOnly => None,
    }
}

/// Load one anchored entry with marker/byte errors preceding DTO interpretation.
///
/// A present marker is decoded first, then every bound structured file must match it
/// before their schemas are interpreted. Without a marker, current/running records
/// expose no events and remain unfinalized; non-running legacy records may be read
/// but remain unverified. Trace-v1 independently checks its saved raw receipt;
/// older tuples never inspect raw or optional sidecars. No branch reparses raw,
/// writes a marker or repairs data. Buffers/events obey the shared resource policy.
fn read_record_at(directory: &File, id: &str, mode: EventReadMode) -> Result<LoadedRecord> {
    let marker = match read_file_at(directory, c"finalized.json", MAX_MARKER_BYTES) {
        Ok(data) => Some(decode_finalized(&data)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(unavailable_io("finalized.json", e)),
    };
    if let Some(marker) = marker {
        if marker.run_id != id {
            return Err(corrupt("finalization marker run ID does not match record"));
        }
        let mut metadata_file =
            open_required_record_file(directory, c"metadata.json", "metadata.json")?;
        verified_file_check(&mut metadata_file, &marker.metadata, "metadata.json")?;
        let mut events_file =
            open_required_record_file(directory, c"events.jsonl", "events.jsonl")?;
        verified_file_check(&mut events_file, &marker.events, "events.jsonl")?;
        let manifest_bytes = match &marker.raw_manifest {
            Some(expected) => {
                let mut file = open_required_record_file(
                    directory,
                    c"raw-integrity.json",
                    "raw-integrity.json",
                )?;
                verified_file_check(&mut file, expected, "raw-integrity.json")?;
                let bytes = read_open_file(&mut file, "raw-integrity.json")?;
                verify(&bytes, expected, "raw-integrity.json")?;
                Some(bytes)
            }
            None => None,
        };

        // All bound files have passed their marker checks before any
        // schema/DTO is interpreted. Rechecking metadata bytes protects that
        // priority if the inode changes in place between verification and use.
        let metadata_bytes = read_open_file(&mut metadata_file, "metadata.json")?;
        verify(&metadata_bytes, &marker.metadata, "metadata.json")?;
        let metadata = decode_metadata(&metadata_bytes, &marker.run_id)?;
        // Trace-v1 provenance identifies a concrete normalized event. Retain its
        // bounded DTO stream even for listing to apply the same pure validator;
        // older tuples keep their original validate-only memory behavior.
        let effective_mode = if marker.schema_version == 2 {
            EventReadMode::CollectEvents
        } else {
            mode
        };
        let streamed = stream_events(&mut events_file, effective_mode)
            .map_err(|error| unavailable_io("events.jsonl", error))?;
        verify_check(&streamed.check, &marker.events, "events.jsonl")?;
        let decoded = streamed.decoded?;
        if decoded.facts.count() != marker.event_count
            || decoded.facts.count() != metadata.parsed_events
        {
            return Err(corrupt("event count mismatch"));
        }
        validate_run_contract_facts(&metadata, &decoded.facts, true)
            .map_err(|error| stored_contract_error("run contract", error))?;
        if metadata.schema_version == 4 {
            validate_run_contract(&metadata, decoded.events.as_deref().unwrap_or(&[]), true)
                .map_err(|error| stored_contract_error("run contract", error))?;
        }
        let storage_integrity = finalized_integrity_with_marker(
            metadata.schema_version,
            decoded.facts,
            marker.schema_version,
        )?;
        let raw_integrity = manifest_bytes
            .map(|bytes| {
                let manifest = decode_raw_manifest(&bytes)?;
                crate::model::validate_raw_manifest(&manifest, &metadata)
                    .map_err(|error| stored_contract_error("raw manifest contract", error))?;
                Ok::<_, anyhow::Error>(raw_integrity_at(directory, manifest))
            })
            .transpose()?;
        return Ok(LoadedRecord {
            metadata,
            events: decoded.events,
            storage_integrity,
            raw_integrity,
        });
    }

    let mut metadata_file =
        open_required_record_file(directory, c"metadata.json", "metadata.json")?;
    let metadata_bytes = read_open_file(&mut metadata_file, "metadata.json")?;
    let metadata = decode_metadata(&metadata_bytes, id)?;
    if metadata.schema_version >= 3 || metadata.state == "running" {
        return Ok(LoadedRecord {
            raw_integrity: (metadata.schema_version == 4).then(|| RawIntegrity {
                status: "unverified".into(),
                detail: "no bound final marker; raw normalization-stream receipt is unverified".into(),
                manifest: None,
            }),
            metadata,
            events: empty_events(mode),
            storage_integrity: StorageIntegrity {
                status: "unfinalized".into(),
                detail: "no final marker; capture may still be running or may have stopped before storage finalization".into(),
            },
        });
    }

    let mut events_file = open_required_record_file(directory, c"events.jsonl", "events.jsonl")?;
    let streamed = stream_events(&mut events_file, mode)
        .map_err(|error| unavailable_io("events.jsonl", error))?;
    let decoded = streamed.decoded?;
    validate_run_contract_facts(&metadata, &decoded.facts, false)
        .map_err(|error| stored_contract_error("run contract", error))?;
    Ok(LoadedRecord {
        metadata,
        events: decoded.events,
        storage_integrity: StorageIntegrity {
            status: "unverified".into(),
            detail: "storage integrity unverified: legacy record has no finalization checksums"
                .into(),
        },
        raw_integrity: None,
    })
}

/// Materialize the same checked load path used by listing, including saved events.
fn read_run_at(directory: &File, id: &str) -> Result<Run> {
    let loaded = read_record_at(directory, id, EventReadMode::CollectEvents)?;
    Ok(Run {
        metadata: loaded.metadata,
        events: loaded
            .events
            .expect("CollectEvents always retains successfully decoded events"),
        storage_integrity: loaded.storage_integrity,
        raw_integrity: loaded.raw_integrity,
    })
}

#[cfg(test)]
fn read_run(directory: PathBuf, metadata: Metadata) -> Result<Run> {
    let directory = open_directory(&directory)?;
    read_run_at(&directory, &metadata.id)
}

#[cfg(test)]
mod identity_tests;
