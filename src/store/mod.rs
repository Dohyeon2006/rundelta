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
//! finalization marker proves only that the structured metadata and event bytes
//! named by that marker were published and verified. It does not authenticate
//! their origin, cover the raw trace, prove capture success, or guarantee that a
//! completed rename survived a power loss.

use crate::model::{
    Event, LoadOutcome, Metadata, RecordSummary, Run, StorageIntegrity, StorageLoadFailure,
    StorageLoadFailureKind,
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::{CStr, CString},
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    os::fd::{AsRawFd, FromRawFd},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
/// Resolve the storage root selected by CLI input or the standard data paths.
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
/// Process-scoped advisory lock held by its backing file descriptor.
pub(crate) struct Lock(std::fs::File);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.sync_all();
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
}

/// A failed initialization is terminal: publication may already have occurred.
#[derive(Clone, Copy, PartialEq, Eq)]
enum InitialMetadata {
    Unwritten,
    Failed,
    Written,
}

fn unsafe_directory(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(code) if code == libc::ELOOP || code == libc::ENOTDIR)
}

fn open_directory(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}

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

fn validate_record_name(name: &str) -> Result<()> {
    if name.is_empty() || name.chars().any(char::is_control) {
        bail!("name must be nonempty without control characters");
    }
    Ok(())
}

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
    pub(crate) fn begin_run(self, name: &str) -> Result<RunWriter> {
        let id = format!(
            "{:x}-{}",
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
            std::process::id()
        );
        self.begin_run_with_id(name, id)
    }

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
        })
    }
}

impl RunWriter {
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
            .map_err(|error| anyhow::anyhow!("invalid metadata contract: {error}"))?;
        if metadata.state != "running" {
            bail!("initial metadata must describe a running capture");
        }
        self.ensure_run_identity()?;
        self.initial_metadata = InitialMetadata::Failed;
        metadata_at(&self.run_dir, metadata)?;
        self.initial_metadata = InitialMetadata::Written;
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
    pub(crate) fn for_each_trace_line<F>(&mut self, mut consume: F) -> std::io::Result<()>
    where
        F: FnMut(&str) -> std::io::Result<()>,
    {
        self.ensure_run_identity()
            .map_err(|error| std::io::Error::other(format!("{error:#}")))?;
        self.trace_file.seek(SeekFrom::Start(0))?;
        let mut reader = BufReader::new(&mut self.trace_file);
        let mut line = Vec::new();
        loop {
            line.clear();
            if reader.read_until(b'\n', &mut line)? == 0 {
                return Ok(());
            }
            let line = std::str::from_utf8(&line).map_err(|error| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("trace line is not valid UTF-8: {error}"),
                )
            })?;
            consume(line)?;
        }
    }

    /// Read the trace inode held by this writer after the collector exits.
    pub(crate) fn read_trace(&mut self) -> std::io::Result<String> {
        self.ensure_run_identity()
            .map_err(|error| std::io::Error::other(format!("{error:#}")))?;
        self.trace_file.seek(SeekFrom::Start(0))?;
        let mut raw = String::new();
        self.trace_file.read_to_string(&mut raw)?;
        Ok(raw)
    }

    /// Publish events, final metadata and the finalization marker through runfd.
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
        self.ensure_run_identity()?;
        finish_at(&self.run_dir, metadata, events)
    }
}

/// Open or create a storage root and anchor its `root` and `runs` directories.
pub(crate) fn open(root: &Path) -> Result<StorageRoot> {
    StorageRoot::open_with(root, true)?.context("storage root disappeared while opening")
}

fn open_existing(root: &Path) -> Result<Option<StorageRoot>> {
    StorageRoot::open_with(root, false)
}

/// Path compatibility wrapper retained while callers migrate to `StorageRoot`.
#[allow(dead_code)]
fn lock(root: &Path) -> Result<Lock> {
    open(root)?.lock()
}

/// Legacy path writer retained until the RF-630 cleanup pass.
#[allow(dead_code)]
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
fn read_file_at(directory: &File, name: &CStr) -> std::io::Result<Vec<u8>> {
    let mut file = open_regular_file_at(directory, name)?;
    let mut bytes = vec![];
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

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

fn verified_file_check(file: &mut File, expected: &FileCheck, display: &str) -> Result<FileCheck> {
    let status = file_stat(file).map_err(|error| unavailable_io(display, error))?;
    let length = stat_length(&status).map_err(|error| unavailable_io(display, error))?;
    if length != expected.bytes {
        return Err(corrupt(format!(
            "{display}: byte count or SHA-256 mismatch"
        )));
    }
    let actual = calculate_file_check(file).map_err(|error| unavailable_io(display, error))?;
    verify_check(&actual, expected, display)?;
    Ok(actual)
}

fn read_open_file(file: &mut File, display: &str) -> Result<Vec<u8>> {
    file.seek(SeekFrom::Start(0))
        .and_then(|_| {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).map(|_| bytes)
        })
        .map_err(|error| unavailable_io(display, error))
}

fn required_record_file_error(display: &str, error: std::io::Error) -> anyhow::Error {
    if error.kind() == std::io::ErrorKind::NotFound {
        corrupt(format!("{display}: {error}"))
    } else {
        unavailable_io(display, error)
    }
}

#[derive(Deserialize)]
struct MetadataIndexHint {
    name: Option<String>,
}

fn metadata_name_hint(directory: &File) -> Result<Option<String>> {
    let file = match open_regular_file_at(directory, c"metadata.json") {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(unavailable_io("read record label", error)),
    };
    let hint = match serde_json::from_reader::<_, MetadataIndexHint>(BufReader::with_capacity(
        64 * 1024,
        file,
    )) {
        Ok(hint) => hint,
        // Missing or malformed stored labels remain unknown index hints. An
        // actual I/O failure cannot establish that a label is absent and must
        // prevent the writer from reserving a potentially duplicate label.
        Err(error) if !error.is_io() => return Ok(None),
        Err(error) => {
            return Err(unavailable_io(
                "read record label",
                std::io::Error::new(
                    error.io_error_kind().unwrap_or(std::io::ErrorKind::Other),
                    error,
                ),
            ));
        }
    };
    Ok(hint
        .name
        .filter(|candidate| !candidate.is_empty() && !candidate.chars().any(char::is_control)))
}

#[derive(Deserialize)]
struct SchemaHeader {
    schema_version: u32,
}

fn decode_schema_header(data: &[u8], display: &str) -> Result<u32> {
    serde_json::from_slice::<SchemaHeader>(data)
        .map(|header| header.schema_version)
        .map_err(|error| corrupt(format!("{display}: {error}")))
}

#[derive(Debug)]
struct ContractViolation(String);

impl std::fmt::Display for ContractViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

type ContractResult = std::result::Result<(), ContractViolation>;

fn contract_violation(message: impl Into<String>) -> ContractViolation {
    ContractViolation(message.into())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoredState {
    Running,
    Completed,
    CaptureFailed,
    Interrupted,
}

impl StoredState {
    fn parse(value: &str) -> std::result::Result<Self, ContractViolation> {
        match value {
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "capture_failed" => Ok(Self::CaptureFailed),
            "interrupted" => Ok(Self::Interrupted),
            _ => Err(contract_violation("unknown capture state")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoredCompleteness {
    Pending,
    CompleteForSupportedEvents,
    Partial,
    Incomplete,
}

impl StoredCompleteness {
    fn parse(value: Option<&str>) -> std::result::Result<Self, ContractViolation> {
        match value {
            Some("pending") => Ok(Self::Pending),
            Some("complete_for_supported_events") => Ok(Self::CompleteForSupportedEvents),
            Some("partial") => Ok(Self::Partial),
            Some("incomplete") => Ok(Self::Incomplete),
            Some(_) => Err(contract_violation("unknown capture completeness")),
            None => Err(contract_violation("completeness is missing")),
        }
    }
}

fn stored_capture_facts(
    metadata: &Metadata,
) -> std::result::Result<(StoredState, StoredCompleteness), ContractViolation> {
    Ok((
        StoredState::parse(&metadata.state)?,
        StoredCompleteness::parse(metadata.completeness.as_deref())?,
    ))
}

/// Validate facts written by the current metadata schema without applying
/// current semantics retroactively to legacy free-form records.
fn validate_metadata_contract(metadata: &Metadata) -> ContractResult {
    if metadata.exit_code.is_some() && metadata.signal.is_some() {
        return Err(contract_violation(
            "target exit code and signal are mutually exclusive",
        ));
    }
    if metadata.collector_exit_code.is_some() && metadata.collector_signal.is_some() {
        return Err(contract_violation(
            "collector exit code and signal are mutually exclusive",
        ));
    }
    if metadata.schema_version != 3 {
        return Ok(());
    }
    if !valid_id(&metadata.id) {
        return Err(contract_violation("ID is not one safe generated component"));
    }
    if metadata.name.is_empty() || metadata.name.chars().any(char::is_control) {
        return Err(contract_violation(
            "name must be nonempty without control characters",
        ));
    }
    if metadata.command.is_empty() {
        return Err(contract_violation("command must contain an executable"));
    }
    if metadata.tool_version.is_empty() || metadata.cwd.is_empty() || metadata.backend.is_empty() {
        return Err(contract_violation(
            "tool version, cwd and backend must be nonempty",
        ));
    }
    let environment = metadata
        .environment
        .as_ref()
        .ok_or_else(|| contract_violation("environment is missing"))?;
    if environment
        .keys()
        .any(|key| key.is_empty() || key.contains('=') || key.chars().any(char::is_control))
    {
        return Err(contract_violation(
            "environment keys must be nonempty without '=' or control characters",
        ));
    }

    let (state, completeness) = stored_capture_facts(metadata)?;
    match (state, completeness) {
        (StoredState::Running, StoredCompleteness::Pending) => {
            if metadata.ended.is_some()
                || metadata.exit_code.is_some()
                || metadata.signal.is_some()
                || metadata.collector_exit_code.is_some()
                || metadata.collector_signal.is_some()
                || metadata.target_exit_raw.is_some()
                || !metadata.warnings.is_empty()
                || metadata.parsed_events != 0
                || metadata.raw_lines != 0
            {
                return Err(contract_violation(
                    "running metadata contains finalized capture facts",
                ));
            }
        }
        (
            StoredState::Completed,
            StoredCompleteness::CompleteForSupportedEvents | StoredCompleteness::Partial,
        ) => {
            if metadata.ended.is_none() {
                return Err(contract_violation("completed metadata has no end time"));
            }
            if completeness == StoredCompleteness::Partial && metadata.warnings.is_empty() {
                return Err(contract_violation(
                    "partial metadata records no evidence limitation",
                ));
            }
            if metadata.target_exit_raw.is_none() {
                return Err(contract_violation(
                    "completed metadata has no raw target outcome",
                ));
            }
            if metadata.collector_exit_code.is_none() && metadata.collector_signal.is_none() {
                return Err(contract_violation(
                    "completed metadata has no collector outcome",
                ));
            }
        }
        (StoredState::CaptureFailed, StoredCompleteness::Incomplete) => {
            if metadata.ended.is_none() {
                return Err(contract_violation("final metadata has no end time"));
            }
            if metadata.warnings.is_empty() {
                return Err(contract_violation(
                    "failed capture records no failure warning",
                ));
            }
        }
        (StoredState::Interrupted, StoredCompleteness::Incomplete) => {
            if metadata.ended.is_none() {
                return Err(contract_violation("final metadata has no end time"));
            }
        }
        (StoredState::Completed, _) => {
            return Err(contract_violation(
                "completed metadata must be complete or partial",
            ));
        }
        (StoredState::CaptureFailed | StoredState::Interrupted, _) => {
            return Err(contract_violation(
                "failed or interrupted capture must be incomplete",
            ));
        }
        (StoredState::Running, _) => {
            return Err(contract_violation("running metadata must be pending"));
        }
    }

    if completeness == StoredCompleteness::CompleteForSupportedEvents
        && !metadata.warnings.is_empty()
    {
        return Err(contract_violation(
            "complete metadata cannot contain capture limitations",
        ));
    }
    if (metadata.exit_code.is_some() || metadata.signal.is_some())
        && metadata.target_exit_raw.is_none()
    {
        return Err(contract_violation(
            "structured target outcome has no raw target outcome",
        ));
    }
    if completeness == StoredCompleteness::CompleteForSupportedEvents {
        let target_matches_collector = match (metadata.exit_code, metadata.signal) {
            (Some(code), None) => {
                metadata.collector_exit_code == Some(code) && metadata.collector_signal.is_none()
            }
            (None, Some(signal)) => {
                metadata.collector_signal == Some(signal) && metadata.collector_exit_code.is_none()
            }
            _ => false,
        };
        if !target_matches_collector {
            return Err(contract_violation(
                "complete metadata lacks a confirmed target outcome",
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct EventFacts {
    count: usize,
    first_unsupported_schema: Option<u32>,
    has_non_v2: bool,
    invalid_evidence_order: bool,
    max_end_line: usize,
    context_uncertain: bool,
}

impl EventFacts {
    fn observe(&mut self, event: &Event) {
        self.count += 1;
        if !matches!(event.schema_version, 1 | 2) && self.first_unsupported_schema.is_none() {
            self.first_unsupported_schema = Some(event.schema_version);
        }
        self.has_non_v2 |= event.schema_version != 2;
        self.invalid_evidence_order |=
            event.evidence.line == 0 || event.evidence.line > event.evidence.end_line;
        self.max_end_line = self.max_end_line.max(event.evidence.end_line);
        self.context_uncertain |= event.context_uncertain;
    }

    fn from_events(events: &[Event]) -> Self {
        let mut facts = Self::default();
        for event in events {
            facts.observe(event);
        }
        facts
    }
}

/// Validate facts that require both current metadata and normalized events.
fn validate_run_contract_facts(
    metadata: &Metadata,
    events: &EventFacts,
    finalized: bool,
) -> ContractResult {
    validate_metadata_contract(metadata)?;
    if metadata.schema_version == 3
        && finalized
        && StoredState::parse(&metadata.state)? == StoredState::Running
    {
        return Err(contract_violation(
            "finalization marker contradicts running metadata",
        ));
    }
    if metadata.schema_version == 3 && events.has_non_v2 {
        return Err(contract_violation(
            "current metadata schema requires only event schema 2",
        ));
    }
    if metadata.parsed_events != events.count {
        return Err(contract_violation(
            "metadata parsed event count does not match events",
        ));
    }
    if events.invalid_evidence_order || events.max_end_line > metadata.raw_lines {
        return Err(contract_violation(
            "event evidence lines fall outside the recorded raw trace",
        ));
    }
    if metadata.schema_version != 3 {
        return Ok(());
    }
    match StoredCompleteness::parse(metadata.completeness.as_deref())? {
        StoredCompleteness::CompleteForSupportedEvents if events.context_uncertain => {
            return Err(contract_violation(
                "complete metadata contains context-uncertain evidence",
            ));
        }
        _ => {}
    }
    Ok(())
}

fn validate_run_contract(metadata: &Metadata, events: &[Event], finalized: bool) -> ContractResult {
    validate_run_contract_facts(metadata, &EventFacts::from_events(events), finalized)
}

fn decode_metadata(data: &[u8], id: &str) -> Result<Metadata> {
    let schema_version = decode_schema_header(data, "metadata.json")?;
    if !matches!(schema_version, 1..=3) {
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
        .map_err(|error| corrupt(format!("metadata contract: {error}")))?;
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

    fn delete_resolved(&self, e: Entry) -> Result<String> {
        let id = e.id.ok_or_else(|| unavailable("unsafe record directory"))?;
        let run_dir = e
            .run_dir
            .ok_or_else(|| unavailable(format!("unsafe record directory: {}", e.directory)))?;
        let component = CString::new(id.as_bytes()).expect("validated IDs contain no NUL");
        ensure_entry_identity(&self.runs_dir, &component, &run_dir).map_err(|error| {
            unavailable_io(format!("unsafe record directory: {}", e.directory), error)
        })?;
        remove_directory_contents(&run_dir)
            .map_err(|error| unavailable_io(format!("delete {}", e.directory), error))?;
        // Check again after recursion. A swapped leaf is never traversed, and
        // AT_REMOVEDIR cannot follow a late symlink if it changes after this check.
        ensure_entry_identity(&self.runs_dir, &component, &run_dir).map_err(|error| {
            unavailable_io(format!("unsafe record directory: {}", e.directory), error)
        })?;
        unlink_at(&self.runs_dir, &component, libc::AT_REMOVEDIR)
            .map_err(|error| unavailable_io(format!("delete {}", e.directory), error))?;
        Ok(id)
    }
}

fn ensure_entry_identity(parent: &File, name: &CStr, opened: &File) -> std::io::Result<()> {
    let expected = file_stat(opened)?;
    let actual = stat_at(parent, name)?;
    if same_file(&expected, &actual) {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "directory entry no longer names the opened directory",
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

fn remove_directory_contents(directory: &File) -> std::io::Result<()> {
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
            remove_directory_contents(&child)?;
            ensure_entry_identity(directory, &component, &child)?;
            unlink_at(directory, &component, libc::AT_REMOVEDIR)?;
        } else {
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

fn neutral_load_failure(error: anyhow::Error) -> StorageLoadFailure {
    StorageLoadFailure {
        kind: storage_load_failure_kind(&error),
        message: format!("{error:#}"),
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

fn decode_finalized(data: &[u8]) -> Result<Finalized> {
    let schema_version = decode_schema_header(data, "finalized.json")?;
    if schema_version != 1 {
        return Err(unsupported(format!(
            "unsupported finalization marker schema {schema_version}"
        )));
    }
    serde_json::from_slice(data).map_err(|error| corrupt(format!("finalized.json: {error}")))
}

fn check(data: &[u8]) -> FileCheck {
    FileCheck {
        bytes: data.len() as u64,
        sha256: format!("{:x}", Sha256::digest(data)),
    }
}

fn calculate_file_check(file: &mut File) -> std::io::Result<FileCheck> {
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
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes = checked_byte_count(bytes, read)?;
        hasher.update(&buffer[..read]);
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
    Ok(FileCheck {
        bytes,
        sha256: format!("{:x}", hasher.finalize()),
    })
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
enum CommitStage {
    BeforePublish,
    PublishedDurabilityUnknown,
    Durable,
}

#[derive(Debug)]
struct CommitFailure {
    file: String,
    operation: &'static str,
    stage: CommitStage,
    source: std::io::Error,
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
        }
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
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

struct StagedFile<'a> {
    directory: &'a File,
    file: File,
    name: Option<CString>,
}

impl StagedFile<'_> {
    fn cleanup(&mut self) -> std::io::Result<()> {
        let Some(name) = self.name.as_ref() else {
            return Ok(());
        };
        match unlink_at(self.directory, name, 0) {
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
}

impl Drop for StagedFile<'_> {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

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

struct AtomicStaging<'a> {
    staged: StagedFile<'a>,
    published: CString,
    display: &'static str,
    control: AtomicControl,
}

struct PublishedFile {
    file: File,
    stage: CommitStage,
}

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
    let staged = create_staging(directory, control.force_named_fallback).map_err(|error| {
        CommitFailure::new(
            name,
            "create temporary file",
            CommitStage::BeforePublish,
            error,
        )
    })?;
    control.check(name, AtomicStep::Write).map_err(|error| {
        CommitFailure::new(
            name,
            "write temporary file",
            CommitStage::BeforePublish,
            error,
        )
    })?;
    Ok(AtomicStaging {
        staged,
        published,
        display: name,
        control,
    })
}

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
            CommitFailure::new(
                display,
                "copy anonymous temporary file to named fallback",
                CommitStage::BeforePublish,
                error,
            )
        })?;
    control
        .check(display, AtomicStep::FallbackFileSync)
        .and_then(|()| named.file.sync_all())
        .map_err(|error| {
            CommitFailure::new(
                display,
                "sync named fallback temporary file",
                CommitStage::BeforePublish,
                error,
            )
        })?;
    Ok(named)
}

impl AtomicStaging<'_> {
    fn write_all(&mut self, data: &[u8]) -> std::result::Result<(), CommitFailure> {
        self.staged.file.write_all(data).map_err(|error| {
            CommitFailure::new(
                self.display,
                "write temporary file",
                CommitStage::BeforePublish,
                error,
            )
        })
    }

    fn publish(mut self) -> std::result::Result<PublishedFile, CommitFailure> {
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
struct EventFileSummary {
    check: FileCheck,
    facts: EventFacts,
}

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
        // Serialize one DTO at a time: memory is bounded by the largest event,
        // while the wire bytes remain exactly the historical compact JSONL.
        let mut line = serde_json::to_vec(event)?;
        line.push(b'\n');
        self.atomic.write_all(&line)?;
        self.bytes = checked_byte_count(self.bytes, line.len())?;
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

fn metadata_at(directory: &File, metadata: &Metadata) -> Result<()> {
    validate_metadata_contract(metadata)
        .map_err(|error| anyhow::anyhow!("invalid metadata contract: {error}"))?;
    atomic_replace(
        directory,
        "metadata.json",
        &serde_json::to_vec_pretty(metadata)?,
    )?;
    Ok(())
}

fn decode_event_line(line: &[u8], line_number: usize) -> Result<Event> {
    let display = format!("events.jsonl:{line_number}");
    let schema_version = decode_schema_header(line, &display)?;
    if !matches!(schema_version, 1 | 2) {
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
enum EventReadMode {
    CollectEvents,
    ValidateOnly,
}

#[derive(Debug)]
struct DecodedEventStream {
    facts: EventFacts,
    events: Option<Vec<Event>>,
}

struct EventStreamReadback {
    check: FileCheck,
    decoded: Result<DecodedEventStream>,
}

fn stream_events(file: &mut File, mode: EventReadMode) -> std::io::Result<EventStreamReadback> {
    let before = file_stat(file)?;
    if !is_regular(&before) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "not a regular file",
        ));
    }
    let expected_length = stat_length(&before)?;
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
        buffer.clear();
        if reader.read_until(b'\n', &mut buffer)? == 0 {
            break;
        }
        saw_bytes = true;
        line_number += 1;
        bytes = checked_byte_count(bytes, buffer.len())?;
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
                    events.push(event);
                }
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

fn stream_event_readback(file: &mut File) -> std::io::Result<EventStreamReadback> {
    stream_events(file, EventReadMode::ValidateOnly)
}

#[derive(Debug)]
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

fn validate_schema_tuple(tuple: &SchemaTuple) -> Result<()> {
    if !matches!(tuple.metadata, 1..=3) {
        return Err(unsupported(format!(
            "unsupported metadata schema {}",
            tuple.metadata
        )));
    }
    if let Some(marker) = tuple.marker
        && marker != 1
    {
        return Err(unsupported(format!(
            "unsupported finalization marker schema {marker}"
        )));
    }
    if let Some(version) = tuple.events.first_unsupported_schema {
        return Err(unsupported(format!("unsupported event schema {version}")));
    }
    if tuple.metadata == 3 && tuple.events.has_non_v2 {
        return Err(corrupt(
            "current metadata schema requires only event schema 2",
        ));
    }
    Ok(())
}

#[cfg(test)]
fn finalized_integrity(metadata_schema: u32, events: &[Event]) -> Result<StorageIntegrity> {
    finalized_integrity_from_facts(metadata_schema, EventFacts::from_events(events))
}

fn finalized_integrity_from_facts(
    metadata_schema: u32,
    events: EventFacts,
) -> Result<StorageIntegrity> {
    validate_schema_tuple(&SchemaTuple::finalized(metadata_schema, events, 1))?;
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
/// Legacy path finalizer retained until the RF-630 cleanup pass.
#[allow(dead_code)]
fn finish(dir: &Path, m: &Metadata, events: &[Event]) -> Result<()> {
    let directory =
        open_directory(dir).map_err(|error| unavailable_io("open record directory", error))?;
    finish_at(&directory, m, events)
}

fn finish_at(directory: &File, m: &Metadata, events: &[Event]) -> Result<()> {
    finish_at_with_control(directory, m, events, AtomicControl::default())
}

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
    let actual = read_file_at(directory, component)
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

fn finish_at_with_control(
    directory: &File,
    m: &Metadata,
    events: &[Event],
    control: AtomicControl,
) -> Result<()> {
    if m.schema_version != 3 || events.iter().any(|event| event.schema_version != 2) {
        bail!("writer requires metadata schema 3 and event schema 2");
    }
    validate_run_contract(m, events, true)
        .map_err(|error| anyhow::anyhow!("cannot finalize inconsistent metadata: {error}"))?;
    let mut event_staging = EventJsonlStaging::begin(directory, control)?;
    for event in events {
        event_staging.push(event)?;
    }
    let (published_events, staged_events) = event_staging.publish()?;
    let reread_events =
        read_back_published_events(directory, &published_events, &staged_events, control)?;
    validate_run_contract_facts(m, &reread_events.facts, true)
        .map_err(|error| anyhow::anyhow!("cannot finalize inconsistent metadata: {error}"))?;
    if reread_events.facts.count != m.parsed_events {
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
    let metadata_bytes = serde_json::to_vec_pretty(m)?;
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
    let marker = Finalized {
        schema_version: 1,
        run_id: m.id.clone(),
        event_count: reread_events.facts.count,
        events: reread_events.check,
        metadata: check(&meta),
    };
    atomic_replace_with_control(
        directory,
        "finalized.json",
        &serde_json::to_vec_pretty(&marker)?,
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
    /// Event files are streamed in validate-only mode: checksums, schemas,
    /// counts, and cross-file contracts are checked without retaining a
    /// `Vec<Event>` for each listed record.
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

/// A damaged row never prevents listing another row. Preserve the flat metadata
/// fields for readable records; unreadable rows expose only index hints/status.
pub(crate) fn records(root: &Path) -> Result<Vec<RecordSummary>> {
    let Some(storage) = open_existing(root)? else {
        return Ok(vec![]);
    };
    storage.records()
}
struct LoadedRecord {
    metadata: Metadata,
    events: Option<Vec<Event>>,
    storage_integrity: StorageIntegrity,
}

fn empty_events(mode: EventReadMode) -> Option<Vec<Event>> {
    match mode {
        EventReadMode::CollectEvents => Some(Vec::new()),
        EventReadMode::ValidateOnly => None,
    }
}

fn read_record_at(directory: &File, id: &str, mode: EventReadMode) -> Result<LoadedRecord> {
    let marker = match read_file_at(directory, c"finalized.json") {
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

        // Both finalized files have passed their marker checks before either
        // schema/DTO is interpreted. Rechecking metadata bytes protects that
        // priority if the inode changes in place between verification and use.
        let metadata_bytes = read_open_file(&mut metadata_file, "metadata.json")?;
        verify(&metadata_bytes, &marker.metadata, "metadata.json")?;
        let metadata = decode_metadata(&metadata_bytes, &marker.run_id)?;
        let streamed = stream_events(&mut events_file, mode)
            .map_err(|error| unavailable_io("events.jsonl", error))?;
        verify_check(&streamed.check, &marker.events, "events.jsonl")?;
        let decoded = streamed.decoded?;
        if decoded.facts.count != marker.event_count
            || decoded.facts.count != metadata.parsed_events
        {
            return Err(corrupt("event count mismatch"));
        }
        validate_run_contract_facts(&metadata, &decoded.facts, true)
            .map_err(|error| corrupt(format!("run contract: {error}")))?;
        let storage_integrity =
            finalized_integrity_from_facts(metadata.schema_version, decoded.facts)?;
        return Ok(LoadedRecord {
            metadata,
            events: decoded.events,
            storage_integrity,
        });
    }

    let mut metadata_file =
        open_required_record_file(directory, c"metadata.json", "metadata.json")?;
    let metadata_bytes = read_open_file(&mut metadata_file, "metadata.json")?;
    let metadata = decode_metadata(&metadata_bytes, id)?;
    if metadata.schema_version >= 3 || metadata.state == "running" {
        return Ok(LoadedRecord {
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
        .map_err(|error| corrupt(format!("run contract: {error}")))?;
    Ok(LoadedRecord {
        metadata,
        events: decoded.events,
        storage_integrity: StorageIntegrity {
            status: "unverified".into(),
            detail: "storage integrity unverified: legacy record has no finalization checksums"
                .into(),
        },
    })
}

fn read_run_at(directory: &File, id: &str) -> Result<Run> {
    let loaded = read_record_at(directory, id, EventReadMode::CollectEvents)?;
    Ok(Run {
        metadata: loaded.metadata,
        events: loaded
            .events
            .expect("CollectEvents always retains successfully decoded events"),
        storage_integrity: loaded.storage_integrity,
    })
}

#[cfg(test)]
fn read_run(directory: PathBuf, metadata: Metadata) -> Result<Run> {
    let directory = open_directory(&directory)?;
    read_run_at(&directory, &metadata.id)
}

#[cfg(test)]
mod identity_tests {
    use super::{
        AtomicControl, AtomicStaging, AtomicStep, CommitFailure, CommitStage, EventFacts,
        EventFileSummary, EventReadMode, FileCheck, Finalized, PublishedFile, StorageError, all,
        atomic_replace_with_control, check, copy_anonymous_to_named, create_named_staging,
        create_staging, decode_events, decode_finalized, decode_metadata, delete, file_stat,
        finalized_integrity, finish, finish_at_with_control, load, load_outcome, lock, lock_error,
        metadata, neutral_load_failure, open, open_directory, read_back_published_events,
        read_record_at, read_run, records, required_record_file_error, storage_load_failure_kind,
        stream_event_readback, tmpfile_is_unsupported, tmpfile_link_is_unsupported, unlink_at,
        valid_id, validate_metadata_contract, validate_run_contract, verify_staging_for_publish,
    };
    use crate::model::{
        Event, Evidence, LoadOutcome, Metadata, RecordSummary, StorageLoadFailureKind,
    };
    use std::{
        collections::BTreeMap,
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

    fn metadata_fixture(id: &str) -> Metadata {
        Metadata {
            schema_version: 3,
            tool_version: "test".into(),
            id: id.into(),
            name: "test".into(),
            command: vec!["true".into()],
            cwd: "/".into(),
            started: 1,
            ended: None,
            backend: "test".into(),
            state: "running".into(),
            exit_code: None,
            signal: None,
            warnings: vec![],
            parsed_events: 0,
            raw_lines: 0,
            environment: Some(BTreeMap::new()),
            completeness: Some("pending".into()),
            collector_exit_code: None,
            collector_signal: None,
            target_exit_raw: None,
        }
    }

    fn event_fixture(schema_version: u32) -> Event {
        Event {
            schema_version,
            pid: "1".into(),
            kind: "exit".into(),
            operation: "exit".into(),
            path: None,
            outcome: "+++ exited with 0 +++".into(),
            evidence: Evidence {
                file: "raw/trace.log".into(),
                line: 1,
                end_line: 1,
            },
            argv: None,
            child_pid: None,
            cwd_before: None,
            context_uncertain: false,
        }
    }

    fn completed_metadata_fixture(id: &str) -> Metadata {
        let mut metadata = metadata_fixture(id);
        metadata.state = "completed".into();
        // Wall-clock time may move backwards; ordering is not a storage invariant.
        metadata.ended = Some(0);
        metadata.exit_code = Some(0);
        metadata.collector_exit_code = Some(0);
        metadata.target_exit_raw = Some("opaque target exit spelling".into());
        metadata.completeness = Some("complete_for_supported_events".into());
        metadata.parsed_events = 1;
        metadata.raw_lines = 1;
        metadata
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
                    },
                    RecordSummary::Readable {
                        metadata: right,
                        directory: right_directory,
                        storage_integrity: right_integrity,
                    },
                ) => {
                    assert_eq!(
                        serde_json::to_value(left).unwrap(),
                        serde_json::to_value(right).unwrap()
                    );
                    assert_eq!(left_directory, right_directory);
                    assert_eq!(left_integrity.status, right_integrity.status);
                    assert_eq!(left_integrity.detail, right_integrity.detail);
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
        assert_eq!(valid_readback.decoded.unwrap().facts.count, 1);

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
    fn current_contract_accepts_each_real_writer_state_and_opaque_raw_outcomes() {
        let event = event_fixture(2);
        let running = metadata_fixture("abc-1");
        validate_metadata_contract(&running).unwrap();

        let completed = completed_metadata_fixture("abc-1");
        validate_run_contract(&completed, std::slice::from_ref(&event), true).unwrap();

        let mut partial = completed.clone();
        partial.exit_code = None;
        partial.completeness = Some("partial".into());
        partial.target_exit_raw = Some("+++ killed by SIG_FUTURE +++".into());
        partial.warnings = vec!["unrecognized target termination signal".into()];
        validate_run_contract(&partial, std::slice::from_ref(&event), true).unwrap();

        let mut failed = completed.clone();
        failed.state = "capture_failed".into();
        failed.completeness = Some("incomplete".into());
        failed.warnings = vec!["collector status does not confirm target completion".into()];
        validate_run_contract(&failed, std::slice::from_ref(&event), true).unwrap();

        let mut interrupted = completed;
        interrupted.state = "interrupted".into();
        interrupted.completeness = Some("incomplete".into());
        validate_run_contract(&interrupted, &[event], true).unwrap();
    }

    #[test]
    fn metadata_contract_rejects_outcome_state_and_writer_fact_contradictions() {
        let completed = completed_metadata_fixture("abc-1");
        let mut cases = Vec::new();

        let mut target_both = completed.clone();
        target_both.signal = Some(9);
        cases.push(("target outcome", target_both));

        let mut collector_both = completed.clone();
        collector_both.collector_signal = Some(9);
        cases.push(("collector outcome", collector_both));

        let mut running_ended = metadata_fixture("abc-1");
        running_ended.ended = Some(2);
        cases.push(("running ended", running_ended));

        let mut running_pending = metadata_fixture("abc-1");
        running_pending.completeness = Some("partial".into());
        cases.push(("running completeness", running_pending));

        let mut running_outcome = metadata_fixture("abc-1");
        running_outcome.exit_code = Some(0);
        running_outcome.target_exit_raw = Some("opaque".into());
        cases.push(("running outcome", running_outcome));

        let mut completed_pending = completed.clone();
        completed_pending.completeness = Some("pending".into());
        cases.push(("completed completeness", completed_pending));

        let mut completed_without_end = completed.clone();
        completed_without_end.ended = None;
        cases.push(("completed end", completed_without_end));

        let mut structured_without_raw = completed.clone();
        structured_without_raw.target_exit_raw = None;
        cases.push(("structured target without raw", structured_without_raw));

        let mut complete_with_warning = completed.clone();
        complete_with_warning.warnings.push("limitation".into());
        cases.push(("complete warning", complete_with_warning));

        let mut complete_without_target = completed.clone();
        complete_without_target.exit_code = None;
        complete_without_target.target_exit_raw = Some("opaque unknown".into());
        cases.push(("complete target confirmation", complete_without_target));

        let mut complete_mismatch = completed.clone();
        complete_mismatch.collector_exit_code = Some(7);
        cases.push(("complete collector confirmation", complete_mismatch));

        let mut partial_without_limitation = completed.clone();
        partial_without_limitation.completeness = Some("partial".into());
        cases.push(("partial limitation", partial_without_limitation));

        let mut partial_without_raw = completed.clone();
        partial_without_raw.completeness = Some("partial".into());
        partial_without_raw.warnings = vec!["unknown target outcome".into()];
        partial_without_raw.exit_code = None;
        partial_without_raw.target_exit_raw = None;
        cases.push(("partial raw target", partial_without_raw));

        let mut partial_without_collector = completed.clone();
        partial_without_collector.completeness = Some("partial".into());
        partial_without_collector.warnings = vec!["unknown target outcome".into()];
        partial_without_collector.exit_code = None;
        partial_without_collector.collector_exit_code = None;
        cases.push(("partial collector outcome", partial_without_collector));

        let mut failed_complete = completed.clone();
        failed_complete.state = "capture_failed".into();
        failed_complete.warnings = vec!["failure".into()];
        cases.push(("failed completeness", failed_complete));

        let mut failed_without_warning = completed.clone();
        failed_without_warning.state = "capture_failed".into();
        failed_without_warning.completeness = Some("incomplete".into());
        cases.push(("failed warning", failed_without_warning));

        let mut interrupted_complete = completed.clone();
        interrupted_complete.state = "interrupted".into();
        cases.push(("interrupted completeness", interrupted_complete));

        let mut empty_command = completed.clone();
        empty_command.command.clear();
        cases.push(("empty command", empty_command));

        let mut bad_name = completed.clone();
        bad_name.name = "bad\nname".into();
        cases.push(("bad name", bad_name));

        let mut bad_environment = completed.clone();
        bad_environment
            .environment
            .as_mut()
            .unwrap()
            .insert("BAD=KEY".into(), None);
        cases.push(("bad environment", bad_environment));

        let mut unknown_state = completed;
        unknown_state.state = "future_state".into();
        cases.push(("unknown state", unknown_state));

        let mut unknown_completeness = completed_metadata_fixture("abc-1");
        unknown_completeness.completeness = Some("future_completeness".into());
        cases.push(("unknown completeness", unknown_completeness));

        for (label, metadata) in cases {
            assert!(
                validate_metadata_contract(&metadata).is_err(),
                "{label} must fail closed"
            );
        }
    }

    #[test]
    fn run_contract_rejects_count_coordinate_and_completeness_contradictions() {
        let metadata = completed_metadata_fixture("abc-1");
        let event = event_fixture(2);

        let mut wrong_count = metadata.clone();
        wrong_count.parsed_events = 2;
        assert!(validate_run_contract(&wrong_count, std::slice::from_ref(&event), true).is_err());

        for (label, line, end_line, raw_lines) in [
            ("zero line", 0, 1, 1),
            ("reversed range", 2, 1, 2),
            ("past raw trace", 1, 2, 1),
        ] {
            let mut invalid = event.clone();
            invalid.evidence.line = line;
            invalid.evidence.end_line = end_line;
            let mut metadata = metadata.clone();
            metadata.raw_lines = raw_lines;
            assert!(
                validate_run_contract(&metadata, &[invalid], true).is_err(),
                "{label} must fail closed"
            );
        }

        let mut uncertain = event;
        uncertain.context_uncertain = true;
        assert!(validate_run_contract(&metadata, &[uncertain], true).is_err());
    }

    #[test]
    fn legacy_contract_checks_only_objective_outcome_count_and_coordinates() {
        let mut legacy = completed_metadata_fixture("abc-1");
        legacy.schema_version = 1;
        legacy.environment = None;
        legacy.completeness = None;
        legacy.warnings = vec!["legacy free-form warning".into()];
        let event = event_fixture(1);
        validate_run_contract(&legacy, std::slice::from_ref(&event), false).unwrap();

        let mut contradictory = legacy.clone();
        contradictory.signal = Some(9);
        assert!(validate_metadata_contract(&contradictory).is_err());

        let mut wrong_count = legacy.clone();
        wrong_count.parsed_events = 2;
        assert!(validate_run_contract(&wrong_count, std::slice::from_ref(&event), false).is_err());

        let mut invalid_evidence = event;
        invalid_evidence.evidence.end_line = 2;
        assert!(validate_run_contract(&legacy, &[invalid_evidence], false).is_err());
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
            let error = required_record_file_error(
                "metadata.json",
                std::io::Error::from_raw_os_error(errno),
            );
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
}
