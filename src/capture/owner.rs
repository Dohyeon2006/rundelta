//! Private same-PID capture admission and raw request transport.
//!
//! A new executable image removes the caller's threads and reapers before any
//! capture resource is acquired. Loader hooks are omitted only for this image;
//! an owned raw environment is restored before the backend starts. This binary
//! adapter is not an embedding API or an authentication boundary.

use super::{policy::CaptureLimits, supervision::require_single_threaded_recorder};
use crate::model::RecordingEvidenceContract;
use anyhow::{Context, Result, bail, ensure};
use std::{
    ffi::{CStr, CString, OsString},
    fs::File,
    io::{self, Read, Write},
    marker::PhantomData,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::FileExt,
        },
    },
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

const ENTRY: &str = "__rundelta_capture_owner_v1";
const MAGIC: &[u8; 8] = b"RDCAP01\0";
const HEADER: usize = 72;
const LENGTH: usize = 8;
const ROOT_SUFFIX: usize = b"/.local/share/rundelta".len();
const SEALS: i32 = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;

unsafe extern "C" {
    static mut environ: *mut *mut libc::c_char;
}

/// Decoded input with private fields; transport never makes capture decisions.
pub(super) struct Request {
    root: PathBuf,
    name: String,
    command: Vec<OsString>,
    selected: Vec<String>,
    environment: Vec<CString>,
    limits: CaptureLimits,
    contract: RecordingEvidenceContract,
}

impl Request {
    pub(super) fn root(&self) -> &Path {
        &self.root
    }
    pub(super) fn name(&self) -> &str {
        &self.name
    }
    pub(super) fn command(&self) -> &[OsString] {
        &self.command
    }
    pub(super) fn selected(&self) -> &[String] {
        &self.selected
    }
    pub(super) fn limits(&self) -> CaptureLimits {
        self.limits
    }
    pub(super) fn contract(&self) -> RecordingEvidenceContract {
        self.contract
    }

    /// Install only in the admitted fresh image, never in a parallel test host.
    pub(super) fn install_environment(&mut self) -> Result<Environment> {
        require_single_threaded_recorder()?;
        let strings = std::mem::take(&mut self.environment);
        let mut pointers = Vec::new();
        pointers.try_reserve_exact(
            strings
                .len()
                .checked_add(1)
                .context("environment pointer count overflow")?,
        )?;
        pointers.extend(strings.iter().map(|value| value.as_ptr().cast_mut()));
        pointers.push(std::ptr::null_mut());
        // SAFETY: admission established a fresh single-threaded binary image;
        // its capture callgraph never changes environ, creates native threads,
        // or loads caller hooks. Both allocations remain owned and immobile
        // until Environment restores this pointer before releasing them.
        let original = unsafe { environ };
        // SAFETY: pointers is a null-terminated array of live CStrings. The
        // !Send/!Sync owner encloses all getenv and backend inheritance uses.
        unsafe { environ = pointers.as_mut_ptr() };
        Ok(Environment {
            _strings: strings,
            _pointers: pointers,
            original,
            _exclusive: PhantomData,
        })
    }
}

/// Own a process-global environment pointer for the admitted capture lifetime.
///
/// Normalizing through a map would discard malformed entries and duplicate
/// order, changing both target inheritance and first-key getenv semantics.
pub(super) struct Environment {
    _strings: Vec<CString>,
    _pointers: Vec<*mut libc::c_char>,
    original: *mut *mut libc::c_char,
    _exclusive: PhantomData<Rc<()>>,
}

impl Drop for Environment {
    fn drop(&mut self) {
        // SAFETY: this sole admitted owner has not changed environ or spawned
        // host threads. Restore the prior live image allocation first; only
        // afterward may Rust release the installed pointer vector/CStrings.
        unsafe { environ = self.original };
    }
}

fn loader_entry(value: &[u8]) -> bool {
    value.starts_with(b"LD_") || value.split(|byte| *byte == b'=').next() == Some(b"GLIBC_TUNABLES")
}

/// Snapshot the raw C list without converting it to an environment map.
/// Native code must not concurrently mutate environ during this copy. Sealing
/// freezes the copy only; it does not establish exclusion against arbitrary FFI.
fn snapshot_environment() -> Result<Vec<CString>> {
    let mut values = Vec::new();
    // SAFETY: the current CLI runtime supplies a null-terminated environ list.
    // Its snapshot premise excludes concurrent native setenv/pointer mutation;
    // no claim is made about malicious or unsynchronized embedding hosts.
    let mut cursor = unsafe { environ };
    if cursor.is_null() {
        return Ok(values);
    }
    loop {
        // SAFETY: cursor addresses the next entry of that live terminated list.
        let pointer = unsafe { *cursor };
        if pointer.is_null() {
            break;
        }
        // SAFETY: each non-null environ entry is a live C string for this copy.
        let value = unsafe { CStr::from_ptr(pointer) };
        values
            .try_reserve(1)
            .context("allocate raw environment snapshot")?;
        values.push(value.to_owned());
        // SAFETY: the list contains another pointer or its null terminator.
        cursor = unsafe { cursor.add(1) };
    }
    Ok(values)
}

fn request_bound(arg_max: usize, pointer_bytes: usize) -> Result<usize> {
    ensure!(
        arg_max > 0 && pointer_bytes > 0,
        "invalid capture request kernel budget"
    );
    let factor = LENGTH
        .div_ceil(pointer_bytes)
        .checked_add(1)
        .context("capture request budget overflow")?;
    arg_max
        .checked_mul(factor)
        .and_then(|value| value.checked_add(HEADER + 2 * LENGTH + ROOT_SUFFIX))
        .context("capture request budget overflow")
}

fn kernel_request_bound() -> Result<usize> {
    // SAFETY: sysconf takes a constant query and retains no application pointer.
    let value = unsafe { libc::sysconf(libc::_SC_ARG_MAX) };
    let value = usize::try_from(value).context("query capture request ARG_MAX")?;
    request_bound(value, std::mem::size_of::<*const libc::c_char>())
}

fn parts(request: &Request) -> impl Iterator<Item = &[u8]> {
    [request.root.as_os_str().as_bytes(), request.name.as_bytes()]
        .into_iter()
        .chain(request.selected.iter().map(|value| value.as_bytes()))
        .chain(request.command.iter().map(|value| value.as_bytes()))
        .chain(request.environment.iter().map(|value| value.as_bytes()))
}

fn write_request(request: &Request, output: &mut impl Write, bound: usize) -> Result<()> {
    let total = parts(request).try_fold(HEADER, |total, value| {
        ensure!(
            !value.contains(&0),
            "capture request contains an interior NUL"
        );
        total
            .checked_add(LENGTH)
            .and_then(|total| total.checked_add(value.len()))
            .context("capture request length overflow")
    })?;
    ensure!(
        total <= bound,
        "capture request exceeds its kernel-derived input budget"
    );
    let mut header = Vec::with_capacity(HEADER);
    header.extend_from_slice(MAGIC);
    header.extend_from_slice(&1_u32.to_le_bytes());
    header.extend_from_slice(&(HEADER as u32).to_le_bytes());
    header.extend_from_slice(&u64::try_from(total)?.to_le_bytes());
    header.push(match request.contract {
        RecordingEvidenceContract::StructuredV3 => 0,
        RecordingEvidenceContract::TraceV1 => 1,
    });
    header.push(
        u8::from(request.limits.max_duration().is_some())
            | (u8::from(request.limits.max_trace_bytes().is_some()) << 1),
    );
    header.extend_from_slice(&0_u16.to_le_bytes());
    let duration = request.limits.max_duration().unwrap_or_default();
    header.extend_from_slice(&duration.as_secs().to_le_bytes());
    header.extend_from_slice(&duration.subsec_nanos().to_le_bytes());
    header.extend_from_slice(
        &request
            .limits
            .max_trace_bytes()
            .unwrap_or_default()
            .to_le_bytes(),
    );
    header.extend_from_slice(&request.limits.terminate_grace().as_secs().to_le_bytes());
    header.extend_from_slice(
        &request
            .limits
            .terminate_grace()
            .subsec_nanos()
            .to_le_bytes(),
    );
    for count in [
        request.command.len(),
        request.selected.len(),
        request.environment.len(),
    ] {
        header.extend_from_slice(&u32::try_from(count)?.to_le_bytes());
    }
    ensure!(
        header.len() == HEADER,
        "capture request header length mismatch"
    );
    output
        .write_all(&header)
        .context("write capture request header")?;
    for value in parts(request) {
        output
            .write_all(&u64::try_from(value.len())?.to_le_bytes())
            .context("write capture request length")?;
        output
            .write_all(value)
            .context("write capture request bytes")?;
    }
    Ok(())
}

fn read_request(input: &mut impl Read, total: usize, bound: usize) -> Result<Request> {
    ensure!(
        total >= HEADER && total <= bound,
        "capture request size outside kernel-derived input budget"
    );
    let mut header = [0_u8; HEADER];
    input
        .read_exact(&mut header)
        .context("read capture request header")?;
    let u32_at =
        |at| u32::from_le_bytes(header[at..at + 4].try_into().expect("fixed header offset"));
    let u64_at =
        |at| u64::from_le_bytes(header[at..at + 8].try_into().expect("fixed header offset"));
    ensure!(
        &header[..8] == MAGIC && u32_at(8) == 1 && u32_at(12) == HEADER as u32,
        "unknown capture request header"
    );
    ensure!(
        usize::try_from(u64_at(16))? == total,
        "capture request declared length mismatch"
    );
    let contract = match header[24] {
        0 => RecordingEvidenceContract::StructuredV3,
        1 => RecordingEvidenceContract::TraceV1,
        _ => bail!("unknown capture request contract"),
    };
    let flags = header[25];
    ensure!(
        flags & !3 == 0 && header[26..28] == [0, 0],
        "unknown capture request flags"
    );
    let duration = |seconds, nanos| -> Result<Duration> {
        ensure!(nanos < 1_000_000_000, "invalid capture request duration");
        Ok(Duration::new(seconds, nanos))
    };
    let max_duration = duration(u64_at(28), u32_at(36))?;
    let trace_bytes = u64_at(40);
    ensure!(
        flags & 1 != 0 || max_duration.is_zero(),
        "unexpected absent duration value"
    );
    ensure!(
        flags & 2 != 0 || trace_bytes == 0,
        "unexpected absent trace-byte value"
    );
    let grace = duration(u64_at(48), u32_at(56))?;
    let counts = [u32_at(60), u32_at(64), u32_at(68)];
    ensure!(counts[0] > 0, "capture request requires a target command");
    let count = counts.iter().try_fold(2_usize, |count, value| {
        count
            .checked_add(usize::try_from(*value)?)
            .context("capture request count overflow")
    })?;
    let mut remaining = total - HEADER;
    ensure!(
        count <= remaining / LENGTH,
        "capture request count exceeds remaining bytes"
    );
    let mut field = || -> Result<Vec<u8>> {
        let mut length = [0_u8; LENGTH];
        input
            .read_exact(&mut length)
            .context("read capture request field length")?;
        remaining = remaining
            .checked_sub(LENGTH)
            .context("capture request truncated field")?;
        let length = usize::try_from(u64::from_le_bytes(length))?;
        ensure!(
            length <= remaining,
            "capture request field exceeds remaining bytes"
        );
        let mut value = Vec::new();
        value
            .try_reserve_exact(length)
            .context("allocate capture request field")?;
        value.resize(length, 0);
        input
            .read_exact(&mut value)
            .context("read capture request field bytes")?;
        remaining -= length;
        ensure!(
            !value.contains(&0),
            "capture request contains an interior NUL"
        );
        Ok(value)
    };
    let root = PathBuf::from(OsString::from_vec(field()?));
    let name = String::from_utf8(field()?).context("capture request name is not UTF-8")?;
    let mut selected = Vec::new();
    for _ in 0..counts[1] {
        selected.try_reserve(1)?;
        selected
            .push(String::from_utf8(field()?).context("capture request selector is not UTF-8")?);
    }
    let mut command = Vec::new();
    for _ in 0..counts[0] {
        command.try_reserve(1)?;
        command.push(OsString::from_vec(field()?));
    }
    let mut environment = Vec::new();
    for _ in 0..counts[2] {
        environment.try_reserve(1)?;
        environment.push(CString::new(field()?)?);
    }
    ensure!(remaining == 0, "capture request has trailing bytes");
    Ok(Request {
        root,
        name,
        command,
        selected,
        environment,
        limits: CaptureLimits::from_durations(
            (flags & 1 != 0).then_some(max_duration),
            (flags & 2 != 0).then_some(trace_bytes),
            grace,
        ),
        contract,
    })
}

struct ReadAt<'a> {
    file: &'a File,
    offset: u64,
}
impl Read for ReadAt<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let read = self.file.read_at(output, self.offset)?;
        self.offset = self
            .offset
            .checked_add(read as u64)
            .ok_or_else(|| io::Error::other("capture request offset overflow"))?;
        Ok(read)
    }
}

/// Replace the old image without acquiring capture children or signal scopes.
pub(super) fn enter(
    root: &Path,
    name: &str,
    command: &[OsString],
    selected: &[String],
    limits: CaptureLimits,
    contract: RecordingEvidenceContract,
) -> Result<std::convert::Infallible> {
    let request = Request {
        root: root.into(),
        name: name.into(),
        command: command.into(),
        selected: selected.into(),
        environment: snapshot_environment()?,
        limits,
        contract,
    };
    // SAFETY: constant NUL-terminated name, only valid memfd creation flags.
    let descriptor =
        unsafe { libc::memfd_create(c"rundelta-capture-v1".as_ptr(), libc::MFD_ALLOW_SEALING) };
    ensure!(
        descriptor >= 0,
        "create capture request: {}",
        io::Error::last_os_error()
    );
    // SAFETY: successful memfd_create returned a new descriptor owned only here.
    let mut file = unsafe { File::from_raw_fd(descriptor) };
    if descriptor < 3 {
        // SAFETY: the owned memfd is live; F_DUPFD selects a new descriptor
        // above stdio without opening or repurposing the original closed slot.
        let duplicate = unsafe { libc::fcntl(descriptor, libc::F_DUPFD, 3) };
        ensure!(
            duplicate >= 3,
            "move capture request above standard IO: {}",
            io::Error::last_os_error()
        );
        // SAFETY: successful F_DUPFD returned a new uniquely owned descriptor.
        // Assignment closes the old low descriptor, preserving the closed slot.
        file = unsafe { File::from_raw_fd(duplicate) };
    }
    let descriptor = file.as_raw_fd();
    write_request(&request, &mut file, kernel_request_bound()?)?;
    // SAFETY: this owned memfd remains live and no mapping is writable.
    let sealed = unsafe { libc::fcntl(descriptor, libc::F_ADD_SEALS, SEALS) };
    ensure!(
        sealed == 0,
        "seal capture request: {}",
        io::Error::last_os_error()
    );
    let args = [
        CString::new("rundelta")?,
        CString::new(ENTRY)?,
        CString::new(descriptor.to_string())?,
    ];
    let mut argv = args.iter().map(|arg| arg.as_ptr()).collect::<Vec<_>>();
    argv.push(std::ptr::null());
    let mut environment = request
        .environment
        .iter()
        .filter(|value| !loader_entry(value.as_bytes()))
        .map(|value| value.as_ptr())
        .collect::<Vec<_>>();
    environment.push(std::ptr::null());
    // SAFETY: all argv/environment strings and terminated pointer arrays remain
    // alive through execve. Only this sealed request crosses into the new image;
    // no Rust pre_exec work runs in a forked multithreaded process.
    unsafe {
        libc::execve(
            c"/proc/self/exe".as_ptr(),
            argv.as_ptr(),
            environment.as_ptr(),
        )
    };
    Err(io::Error::last_os_error()).context("exec current capture-owner image")
}

/// Recognize only the private fresh-image entry; normal CLI input is untouched.
pub(super) fn resume() -> Result<Option<Request>> {
    // Normal startup defers without touching Rust's eager argv snapshot. The
    // kernel startup string already belongs to private admission; this early
    // check adds no filesystem lookup or separate mutable process state.
    // SAFETY: getauxval reads a kernel-provided constant startup entry.
    let execfn = unsafe { libc::getauxval(libc::AT_EXECFN) };
    if execfn == 0 {
        return Ok(None);
    }
    // SAFETY: nonzero AT_EXECFN addresses a live terminated startup C string.
    let executable_name = unsafe { CStr::from_ptr(execfn as *const libc::c_char) };
    if executable_name.to_bytes() != b"/proc/self/exe" {
        return Ok(None);
    }
    let Some(descriptor_argument) = private_descriptor(std::env::args_os())? else {
        return Ok(None);
    };
    require_single_threaded_recorder()?;
    ensure!(
        !snapshot_environment()?
            .iter()
            .any(|value| loader_entry(value.as_bytes())),
        "capture-owner startup loader environment is not isolated"
    );
    let token = descriptor_argument
        .to_str()
        .context("capture request descriptor is not ASCII")?;
    ensure!(
        !token.is_empty() && token.bytes().all(|byte| byte.is_ascii_digit()),
        "invalid capture request descriptor"
    );
    let descriptor: i32 = token
        .parse()
        .context("invalid capture request descriptor")?;
    ensure!(
        descriptor >= 3,
        "capture request descriptor must not use standard IO"
    );
    // SAFETY: F_GETFD only queries a descriptor number. A successful query
    // establishes that it is open before constructing its Rust IO owner; the
    // fresh single-threaded image has no competing closer or inherited handler.
    let descriptor_flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    ensure!(
        descriptor_flags >= 0,
        "capture request descriptor is not open"
    );
    // SAFETY: the private entry explicitly transfers ownership of this nonstdio
    // descriptor. Invalid types/contents are rejected while the File closes it.
    let file = unsafe { File::from_raw_fd(descriptor) };
    let metadata = file
        .metadata()
        .context("inspect capture request descriptor")?;
    use std::os::unix::fs::MetadataExt;
    ensure!(
        metadata.is_file() && metadata.nlink() == 0,
        "capture request must be anonymous regular storage"
    );
    // SAFETY: file owns the descriptor for this fcntl query and flag update.
    let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
    ensure!(
        seals >= 0 && seals & SEALS == SEALS,
        "capture request must be fully sealed"
    );
    // SAFETY: setting close-on-exec never transfers this owned descriptor.
    let protected = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
    ensure!(
        protected == 0,
        "protect capture request descriptor: {}",
        io::Error::last_os_error()
    );
    let request = read_request(
        &mut ReadAt {
            file: &file,
            offset: 0,
        },
        usize::try_from(metadata.len())?,
        kernel_request_bound()?,
    )?;
    Ok(Some(request))
}

/// Inspect only the selector and exact private arity, avoiding a second argv
/// collection. Rust args_os itself snapshots argv eagerly; normal startup is
/// rejected above before calling that API. This is not source authentication.
fn private_descriptor(mut args: impl Iterator<Item = OsString>) -> Result<Option<OsString>> {
    let _ = args.next();
    if args.next().is_none_or(|value| value != ENTRY) {
        return Ok(None);
    }
    let descriptor = args
        .next()
        .context("invalid private capture-owner invocation")?;
    ensure!(
        args.next().is_none(),
        "invalid private capture-owner invocation"
    );
    Ok(Some(descriptor))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Request {
        Request {
            root: PathBuf::from(OsString::from_vec(b"/raw\xff".to_vec())),
            name: "fixture".into(),
            command: vec![OsString::from_vec(b"arg\xfe".to_vec())],
            selected: vec!["A".into()],
            environment: [
                b"A=first".as_slice(),
                b"A=second",
                b"malformed",
                b"=weird",
                b"",
                b"N=v\xff",
            ]
            .into_iter()
            .map(|value| CString::new(value).unwrap())
            .collect(),
            limits: CaptureLimits::from_durations(
                Some(Duration::new(1, 23)),
                Some(0),
                Duration::new(2, 45),
            ),
            contract: RecordingEvidenceContract::TraceV1,
        }
    }
    fn encoded() -> Vec<u8> {
        let mut bytes = Vec::new();
        write_request(&fixture(), &mut bytes, usize::MAX).unwrap();
        bytes
    }
    #[test]
    fn raw_frame_preserves_native_bytes_duplicates_empty_and_exact_policy() {
        let bytes = encoded();
        let result = read_request(&mut io::Cursor::new(&bytes), bytes.len(), bytes.len()).unwrap();
        let original = fixture();
        assert_eq!(result.root, original.root);
        assert_eq!(result.command, original.command);
        assert_eq!(result.environment, original.environment);
        assert_eq!(result.selected, original.selected);
        assert_eq!(result.limits, original.limits);
        assert_eq!(result.contract, original.contract);
    }
    #[test]
    fn malformed_headers_counts_lengths_and_truncation_fail_closed() {
        let original = encoded();
        for at in [0, 8, 12, 16, 24, 25, 26, 60, 64, 68, 72] {
            let mut wrong = original.clone();
            wrong[at] = 0xff;
            assert!(
                read_request(&mut io::Cursor::new(&wrong), wrong.len(), usize::MAX).is_err(),
                "offset {at}"
            );
        }
        for length in [0, HEADER - 1, HEADER, original.len() - 1] {
            assert!(
                read_request(
                    &mut io::Cursor::new(&original[..length]),
                    length,
                    usize::MAX
                )
                .is_err()
            );
        }
        assert!(
            read_request(
                &mut io::Cursor::new(&original),
                original.len(),
                original.len() - 1
            )
            .is_err()
        );
        let mut trailing = original.clone();
        trailing.push(0);
        assert!(read_request(&mut io::Cursor::new(&trailing), trailing.len(), usize::MAX).is_err());
    }

    #[test]
    fn zero_command_is_not_a_valid_private_cli_snapshot() {
        let mut request = fixture();
        request.command.clear();
        let mut encoded = Vec::new();
        write_request(&request, &mut encoded, usize::MAX).unwrap();
        assert!(read_request(&mut io::Cursor::new(&encoded), encoded.len(), usize::MAX).is_err());
    }
    #[test]
    fn private_selector_does_not_consume_ordinary_command_arguments() {
        let ordinary = [OsString::from("binary"), OsString::from("diff")]
            .into_iter()
            .chain(std::iter::from_fn(|| panic!("ordinary argv was consumed")));
        assert!(private_descriptor(ordinary).unwrap().is_none());
        for arguments in [vec!["binary", ENTRY], vec!["binary", ENTRY, "3", "extra"]] {
            assert!(private_descriptor(arguments.into_iter().map(OsString::from)).is_err());
        }
        assert_eq!(
            private_descriptor(["binary", ENTRY, "3"].into_iter().map(OsString::from)).unwrap(),
            Some(OsString::from("3"))
        );
    }

    #[test]
    fn absent_limits_and_empty_optional_lists_have_one_unambiguous_encoding() {
        let mut request = fixture();
        request.selected.clear();
        request.environment.clear();
        request.limits = CaptureLimits::default();
        request.contract = RecordingEvidenceContract::StructuredV3;
        let mut encoded = Vec::new();
        write_request(&request, &mut encoded, usize::MAX).unwrap();
        let result =
            read_request(&mut io::Cursor::new(&encoded), encoded.len(), usize::MAX).unwrap();
        assert_eq!(result.limits, request.limits);
        assert!(result.selected.is_empty());
        assert!(result.environment.is_empty());
        for (at, invalid) in [(28, 1_u32), (36, 1), (56, 1_000_000_000)] {
            let mut wrong = encoded.clone();
            wrong[at..at + 4].copy_from_slice(&invalid.to_le_bytes());
            assert!(read_request(&mut io::Cursor::new(&wrong), wrong.len(), usize::MAX).is_err());
        }
    }
    #[test]
    fn kernel_bound_accounts_for_native_pointer_width_without_json_inflation() {
        assert_eq!(request_bound(1000, 8).unwrap(), 2110);
        assert_eq!(request_bound(1000, 4).unwrap(), 3110);
        assert!(request_bound(0, 8).is_err());
        assert!(request_bound(1000, 0).is_err());
        assert!(request_bound(usize::MAX, 8).is_err());
    }
    #[test]
    fn loader_filter_is_exact_and_never_applied_to_restored_target_environment() {
        for entry in [
            b"LD_PRELOAD=x".as_slice(),
            b"LD_AUDIT=x",
            b"LD_LIBRARY_PATH=x",
            b"GLIBC_TUNABLES=x",
        ] {
            assert!(loader_entry(entry));
        }
        for entry in [
            b"NORMAL=LD_PRELOAD=x".as_slice(),
            b"GLIBC_TUNABLES_MORE=x",
            b"",
            b"bad",
            b"=LD_PRELOAD=x",
        ] {
            assert!(!loader_entry(entry));
        }
    }
}
