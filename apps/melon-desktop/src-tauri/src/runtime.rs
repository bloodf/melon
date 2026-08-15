use cap_fs_ext::{FollowSymlinks, MetadataExt, OpenOptionsFollowExt};
#[cfg(not(windows))]
use cap_fs_ext::DirExt;
#[cfg(not(windows))]
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
#[cfg(unix)]
use cap_std::fs::Permissions;
#[cfg(unix)]
use cap_std::fs::PermissionsExt;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
#[cfg(windows)]
use std::mem::{offset_of, size_of};
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, FromRawHandle};
#[cfg(windows)]
use std::ptr;
#[cfg(windows)]
use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
#[cfg(windows)]
use windows_sys::Wdk::Storage::FileSystem::{
    NtCreateFile, FILE_CREATE, FILE_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_FOR_BACKUP_INTENT,
    FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
};
#[cfg(windows)]
use windows_sys::Win32::Foundation::{
    CloseHandle, RtlNtStatusToDosError, GENERIC_READ, HANDLE, INVALID_HANDLE_VALUE, UNICODE_STRING,
};
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, GetFileInformationByHandleEx, SetFileInformationByHandle, DELETE,
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO,
    FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_RENAME_INFO, FILE_SHARE_READ, FILE_SHARE_WRITE, FileAttributeTagInfo,
    FileDispositionInfo, FileRenameInfo, OPEN_EXISTING,
};
#[cfg(windows)]
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;
use std::fs;
use std::io::{self, Read, Seek, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use zip::ZipArchive;

const DEFAULT_EXTRACTION_LIMITS: ExtractionLimits = ExtractionLimits {
    max_entries: 16_384,
    max_uncompressed_bytes: 4 * 1024 * 1024 * 1024,
};
const COPY_BUFFER_BYTES: usize = 64 * 1024;
const UNIX_FILE_TYPE_MASK: u32 = 0o170000;
const UNIX_REGULAR_FILE: u32 = 0o100000;
const UNIX_DIRECTORY: u32 = 0o040000;
static NEXT_CANDIDATE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy)]
struct ExtractionLimits {
    max_entries: usize,
    max_uncompressed_bytes: u64,
}

#[derive(Debug)]
pub(crate) enum RuntimeError {
    Io(io::Error),
    Zip(zip::result::ZipError),
    InvalidDigest,
    DigestMismatch,
    StagingNotEmpty,
    UnsafePath(String),
    UnsupportedEntry(String),
    PathCollision(String),
    TooManyEntries,
    TooManyBytes,
    InvalidArchive(&'static str),
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "runtime I/O failed: {error}"),
            Self::Zip(error) => write!(formatter, "invalid runtime ZIP: {error}"),
            Self::InvalidDigest => formatter.write_str("expected SHA-256 must be 64 lowercase hexadecimal characters"),
            Self::DigestMismatch => formatter.write_str("runtime SHA-256 does not match the trusted digest"),
            Self::StagingNotEmpty => formatter.write_str("runtime staging directory must be a real empty directory"),
            Self::UnsafePath(path) => write!(formatter, "unsafe ZIP path: {path}"),
            Self::UnsupportedEntry(path) => write!(formatter, "unsupported ZIP entry: {path}"),
            Self::PathCollision(path) => write!(formatter, "colliding ZIP path: {path}"),
            Self::TooManyEntries => formatter.write_str("runtime ZIP contains too many entries"),
            Self::TooManyBytes => formatter.write_str("runtime ZIP exceeds the uncompressed byte limit"),
            Self::InvalidArchive(reason) => write!(formatter, "invalid runtime ZIP: {reason}"),
        }
    }
}

impl std::error::Error for RuntimeError {}

impl From<io::Error> for RuntimeError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<zip::result::ZipError> for RuntimeError {
    fn from(error: zip::result::ZipError) -> Self {
        Self::Zip(error)
    }
}

/// Streams a runtime candidate through SHA-256 and accepts only an exact canonical trusted digest.
pub(crate) fn verify_sha256(reader: &mut impl Read, expected: &str) -> Result<(), RuntimeError> {
    let expected = parse_sha256(expected)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; COPY_BUFFER_BYTES];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    if hasher.finalize().as_slice() == expected {
        Ok(())
    } else {
        Err(RuntimeError::DigestMismatch)
    }
}

fn parse_sha256(expected: &str) -> Result<[u8; 32], RuntimeError> {
    if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) {
        return Err(RuntimeError::InvalidDigest);
    }
    let mut digest = [0; 32];
    for (index, pair) in expected.as_bytes().chunks_exact(2).enumerate() {
        digest[index] = (hex_nibble(pair[0]) << 4) | hex_nibble(pair[1]);
    }
    Ok(digest)
}

fn hex_nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => unreachable!("digest syntax was validated"),
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum EntryKind {
    Directory,
    File,
}

struct EntryPlan {
    path: PathBuf,
    kind: EntryKind,
    mode: Option<u32>,
}

/// Verifies one candidate before rewinding and extracting it into the caller-owned empty staging directory.
pub(crate) fn verify_and_extract_zip(
    reader: &mut (impl Read + Seek),
    expected_sha256: &str,
    staging: &Path,
) -> Result<(), RuntimeError> {
    verify_sha256(reader, expected_sha256)?;
    reader.seek(io::SeekFrom::Start(0))?;
    extract_zip(reader, staging)
}

/// Extracts into a newly-created empty Tauri app-cache staging directory owned by the current user.
///
/// On Unix, staging and its parent must prevent mutation by other users throughout publication.
pub(crate) fn extract_zip(reader: &mut (impl Read + Seek), staging: &Path) -> Result<(), RuntimeError> {
    extract_zip_with_limits(reader, staging, DEFAULT_EXTRACTION_LIMITS)
}

fn extract_zip_with_limits(
    reader: &mut (impl Read + Seek),
    staging: &Path,
    limits: ExtractionLimits,
) -> Result<(), RuntimeError> {
    let preflight = preflight_archive(reader)?;
    if preflight.declared_entries > limits.max_entries {
        return Err(RuntimeError::TooManyEntries);
    }
    let staging_dir = open_empty_staging(staging)?;
    let mut archive = ZipArchive::new(MaskedReader::new(reader, preflight.false_eocd_offsets))?;
    if preflight.declared_entries != archive.len() {
        return Err(RuntimeError::PathCollision("duplicate central-directory name".into()));
    }
    let plans = validate_entries(&mut archive, limits)?;
    let mut candidate = CandidateDir::create(staging, &staging_dir)?;
    drop(staging_dir);
    let extraction = extract_entries(&mut archive, &plans, candidate.dir(), limits);
    if let Err(error) = extraction {
        candidate.cleanup()?;
        return Err(error);
    }
    candidate.publish(staging)?;
    Ok(())
}
#[cfg(test)]
fn extract_zip_with_hook(
    reader: &mut (impl Read + Seek),
    staging: &Path,
    limits: ExtractionLimits,
    hook: impl FnOnce(&Path),
) -> Result<(), RuntimeError> {
    let preflight = preflight_archive(reader)?;
    let staging_dir = open_empty_staging(staging)?;
    let mut archive = ZipArchive::new(MaskedReader::new(reader, preflight.false_eocd_offsets))?;
    if preflight.declared_entries != archive.len() {
        return Err(RuntimeError::PathCollision("duplicate central-directory name".into()));
    }
    let plans = validate_entries(&mut archive, limits)?;
    let mut candidate = CandidateDir::create(staging, &staging_dir)?;
    drop(staging_dir);
    hook(candidate.path());
    extract_entries(&mut archive, &plans, candidate.dir(), limits)?;
    candidate.publish(staging)
}
#[cfg(test)]
fn extract_zip_with_publish_hooks(
    reader: &mut (impl Read + Seek),
    staging: &Path,
    after_check: impl FnOnce(&Path),
    after_final_rename: impl FnOnce(&Path),
) -> Result<(), RuntimeError> {
    let preflight = preflight_archive(reader)?;
    let staging_dir = open_empty_staging(staging)?;
    let mut archive = ZipArchive::new(MaskedReader::new(reader, preflight.false_eocd_offsets))?;
    let plans = validate_entries(&mut archive, DEFAULT_EXTRACTION_LIMITS)?;
    assert_eq!(preflight.declared_entries, archive.len());
    let mut candidate = CandidateDir::create(staging, &staging_dir)?;
    drop(staging_dir);
    extract_entries(
        &mut archive,
        &plans,
        candidate.dir(),
        DEFAULT_EXTRACTION_LIMITS,
    )?;
    candidate.publish_with_hooks(staging, after_check, after_final_rename)
}


struct MaskedReader<'a, R> {
    inner: &'a mut R,
    false_eocd_offsets: Vec<u64>,
    position: u64,
}

impl<'a, R> MaskedReader<'a, R> {
    fn new(inner: &'a mut R, false_eocd_offsets: Vec<u64>) -> Self {
        Self {
            inner,
            false_eocd_offsets,
            position: 0,
        }
    }
}

impl<R: Read> Read for MaskedReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buffer)?;
        for (index, byte) in buffer[..read].iter_mut().enumerate() {
            let absolute = self.position + index as u64;
            if self.false_eocd_offsets.iter().any(|offset| absolute >= *offset && absolute < *offset + 4) {
                *byte = 0;
            }
        }
        self.position += read as u64;
        Ok(read)
    }
}

impl<R: Seek> Seek for MaskedReader<'_, R> {
    fn seek(&mut self, position: io::SeekFrom) -> io::Result<u64> {
        self.position = self.inner.seek(position)?;
        Ok(self.position)
    }
}

fn open_empty_staging(staging: &Path) -> Result<Dir, RuntimeError> {
    validate_private_staging(staging)?;
    let staging = open_runtime_directory(staging)?;
    if staging.entries()?.next().is_some() {
        return Err(RuntimeError::StagingNotEmpty);
    }
    Ok(staging)
}

#[cfg(unix)]
fn validate_private_staging(staging: &Path) -> Result<(), RuntimeError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let metadata = fs::symlink_metadata(staging)?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(RuntimeError::StagingNotEmpty);
    }
    let parent = fs::symlink_metadata(
        staging.parent().ok_or_else(|| RuntimeError::UnsafePath(staging.display().to_string()))?,
    )?;
    if !parent.is_dir()
        || parent.file_type().is_symlink()
        || parent.uid() != unsafe { libc::geteuid() }
        || parent.permissions().mode() & 0o077 != 0
    {
        return Err(RuntimeError::StagingNotEmpty);
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_staging(_staging: &Path) -> Result<(), RuntimeError> {
    // Caller supplies a Tauri app-cache directory protected by the current user's ACL.
    Ok(())
}

#[cfg(not(windows))]
fn open_runtime_directory(path: &Path) -> Result<Dir, RuntimeError> {
    Dir::open_ambient_dir(path, ambient_authority()).map_err(RuntimeError::Io)
}

#[cfg(windows)]
fn open_runtime_directory(path: &Path) -> Result<Dir, RuntimeError> {
    let path = wide_path_without_final_resolution(path)?;
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            GENERIC_READ | DELETE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | windows_sys::Win32::Storage::FileSystem::FILE_SHARE_DELETE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error().into());
    }
    if let Err(error) = validate_windows_directory_handle(handle) {
        unsafe { CloseHandle(handle) };
        return Err(error);
    }
    let file = unsafe { std::fs::File::from_raw_handle(handle) };
    Ok(Dir::from_std_file(file))
}

#[cfg(windows)]
fn wide_path_without_final_resolution(path: &Path) -> Result<Vec<u16>, RuntimeError> {
    let parent = path
        .parent()
        .ok_or_else(|| RuntimeError::UnsafePath(path.display().to_string()))?;
    let name = path
        .file_name()
        .ok_or_else(|| RuntimeError::UnsafePath(path.display().to_string()))?;
    let path = fs::canonicalize(parent)?.join(name);
    let mut wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
    if wide.contains(&0) {
        return Err(RuntimeError::UnsafePath(path.display().to_string()));
    }
    wide.push(0);
    Ok(wide)
}

#[cfg(windows)]
fn validate_windows_directory_handle(handle: HANDLE) -> Result<(), RuntimeError> {
    let mut info = FILE_ATTRIBUTE_TAG_INFO::default();
    let success = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileAttributeTagInfo,
            ptr::from_mut(&mut info).cast(),
            size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    };
    if success == 0 {
        return Err(io::Error::last_os_error().into());
    }
    if info.FileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
        || info.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(RuntimeError::StagingNotEmpty);
    }
    Ok(())
}

fn preflight_archive(reader: &mut (impl Read + Seek)) -> Result<ArchivePreflight, RuntimeError> {
    let footer = find_eocd(reader)?;
    if footer.total_entries == u16::MAX {
        return Err(RuntimeError::InvalidArchive("ZIP64 entry tables are unsupported"));
    }
    reader.seek(io::SeekFrom::Start(footer.central_offset.into()))?;
    let mut local_headers = Vec::with_capacity(footer.total_entries as usize);
    for _ in 0..footer.total_entries {
        let fixed = read_exact_array::<46>(reader)?;
        if &fixed[..4] != b"PK\x01\x02" {
            return Err(RuntimeError::InvalidArchive("invalid central-directory entry"));
        }
        let name_length = u16::from_le_bytes([fixed[28], fixed[29]]) as usize;
        let extra_length = u16::from_le_bytes([fixed[30], fixed[31]]) as usize;
        let comment_length = u16::from_le_bytes([fixed[32], fixed[33]]) as usize;
        let local_offset = u32::from_le_bytes(fixed[42..46].try_into().expect("fixed field"));
        reader.seek(io::SeekFrom::Current(name_length as i64))?;
        let mut extra = vec![0; extra_length];
        reader.read_exact(&mut extra)?;
        reject_link_extra(&extra)?;
        reader.seek(io::SeekFrom::Current(comment_length as i64))?;
        local_headers.push(local_offset);
    }
    for local_offset in local_headers {
        reader.seek(io::SeekFrom::Start(local_offset.into()))?;
        let fixed = read_exact_array::<30>(reader)?;
        if &fixed[..4] != b"PK\x03\x04" {
            return Err(RuntimeError::InvalidArchive("invalid local file header"));
        }
        let name_length = u16::from_le_bytes([fixed[26], fixed[27]]) as i64;
        let extra_length = u16::from_le_bytes([fixed[28], fixed[29]]) as usize;
        reader.seek(io::SeekFrom::Current(name_length))?;
        let mut extra = vec![0; extra_length];
        reader.read_exact(&mut extra)?;
        reject_link_extra(&extra)?;
    }
    Ok(ArchivePreflight {
        declared_entries: footer.total_entries as usize,
        false_eocd_offsets: footer.false_eocd_offsets,
    })
}

struct ArchivePreflight {
    declared_entries: usize,
    false_eocd_offsets: Vec<u64>,
}

struct Eocd {
    total_entries: u16,
    central_offset: u32,
    false_eocd_offsets: Vec<u64>,
}

fn find_eocd(reader: &mut (impl Read + Seek)) -> Result<Eocd, RuntimeError> {
    const EOCD_BYTES: usize = 22;
    const MAX_COMMENT_BYTES: usize = u16::MAX as usize;
    let length = reader.seek(io::SeekFrom::End(0))?;
    if length < EOCD_BYTES as u64 {
        return Err(RuntimeError::InvalidArchive("missing end-of-central-directory record"));
    }
    let tail_length = usize::try_from(length.min((EOCD_BYTES + MAX_COMMENT_BYTES) as u64))
        .map_err(|_| RuntimeError::InvalidArchive("oversized ZIP footer"))?;
    let tail_start = length - tail_length as u64;
    reader.seek(io::SeekFrom::Start(tail_start))?;
    let mut tail = vec![0; tail_length];
    reader.read_exact(&mut tail)?;
    let mut false_eocd_offsets = Vec::new();
    for (offset, window) in tail.windows(4).enumerate().rev() {
        if window != b"PK\x05\x06" || !valid_eocd_candidate(&tail, offset) {
            continue;
        }
        let disk = u16::from_le_bytes([tail[offset + 4], tail[offset + 5]]);
        let directory_disk = u16::from_le_bytes([tail[offset + 6], tail[offset + 7]]);
        let disk_entries = u16::from_le_bytes([tail[offset + 8], tail[offset + 9]]);
        let total_entries = u16::from_le_bytes([tail[offset + 10], tail[offset + 11]]);
        let directory_size = u32::from_le_bytes(tail[offset + 12..offset + 16].try_into().expect("fixed field"));
        let central_offset = u32::from_le_bytes(tail[offset + 16..offset + 20].try_into().expect("fixed field"));
        let absolute_eocd = tail_start + offset as u64;
        if disk == 0
            && directory_disk == 0
            && disk_entries == total_entries
            && u64::from(central_offset) + u64::from(directory_size) == absolute_eocd
            && (total_entries != 0 || absolute_eocd == 0)
            && (total_entries == 0 || central_signature(reader, central_offset)?)
        {
            return Ok(Eocd {
                total_entries,
                central_offset,
                false_eocd_offsets,
            });
        }
        false_eocd_offsets.push(absolute_eocd);
    }
    Err(RuntimeError::InvalidArchive("missing end-of-central-directory record"))
}

fn central_signature(
    reader: &mut (impl Read + Seek),
    offset: u32,
) -> Result<bool, RuntimeError> {
    reader.seek(io::SeekFrom::Start(offset.into()))?;
    Ok(read_exact_array::<4>(reader)? == *b"PK\x01\x02")
}

fn read_exact_array<const N: usize>(reader: &mut impl Read) -> Result<[u8; N], RuntimeError> {
    let mut bytes = [0; N];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn reject_link_extra(mut extra: &[u8]) -> Result<(), RuntimeError> {
    while !extra.is_empty() {
        if extra.len() < 4 {
            return Err(RuntimeError::InvalidArchive("malformed ZIP extra field"));
        }
        let id = u16::from_le_bytes([extra[0], extra[1]]);
        let length = u16::from_le_bytes([extra[2], extra[3]]) as usize;
        if extra.len() < 4 + length {
            return Err(RuntimeError::InvalidArchive("malformed ZIP extra field"));
        }
        if matches!(id, 0x000d | 0x756e) {
            return Err(RuntimeError::UnsupportedEntry("UNIX link metadata".into()));
        }
        extra = &extra[4 + length..];
    }
    Ok(())
}

fn valid_eocd_candidate(tail: &[u8], offset: usize) -> bool {
    if tail.len().saturating_sub(offset) < 22 {
        return false;
    }
    let comment_length = u16::from_le_bytes([tail[offset + 20], tail[offset + 21]]) as usize;
    offset + 22 + comment_length == tail.len()
}
fn validate_entries<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    limits: ExtractionLimits,
) -> Result<Vec<EntryPlan>, RuntimeError> {
    if archive.len() > limits.max_entries {
        return Err(RuntimeError::TooManyEntries);
    }
    let mut total_bytes = 0_u64;
    let mut paths = HashMap::<String, EntryKind>::with_capacity(archive.len());
    let mut plans = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        let (path, components) = validate_path(entry.name())?;
        let kind = validate_kind(&entry)?;
        total_bytes = total_bytes.checked_add(entry.size()).ok_or(RuntimeError::TooManyBytes)?;
        if total_bytes > limits.max_uncompressed_bytes {
            return Err(RuntimeError::TooManyBytes);
        }
        register_path(&mut paths, &components, kind, entry.name())?;
        plans.push(EntryPlan {
            path,
            kind,
            mode: entry.unix_mode().map(|mode| mode & 0o777),
        });
    }
    Ok(plans)
}

fn validate_path(name: &str) -> Result<(PathBuf, Vec<String>), RuntimeError> {
    if name.is_empty()
        || name.contains('\0')
        || name.contains('\\')
        || name.starts_with('/')
        || name.as_bytes().get(1) == Some(&b':')
    {
        return Err(RuntimeError::UnsafePath(name.into()));
    }
    let trimmed = name.strip_suffix('/').unwrap_or(name);
    if trimmed.is_empty() {
        return Err(RuntimeError::UnsafePath(name.into()));
    }
    let mut path = PathBuf::new();
    let mut keys = Vec::new();
    for component in trimmed.split('/') {
        if component.is_empty() || component == "." || component == ".." || component.contains(':') {
            return Err(RuntimeError::UnsafePath(name.into()));
        }
        path.push(component);
        keys.push(windows_component_key(component)?);
    }
    if path.components().any(|component| !matches!(component, Component::Normal(_))) {
        return Err(RuntimeError::UnsafePath(name.into()));
    }
    Ok((path, keys))
}
fn windows_component_key(component: &str) -> Result<String, RuntimeError> {
    let normalized = component.trim_end_matches([' ', '.']);
    if normalized.len() != component.len() || normalized.is_empty() {
        return Err(RuntimeError::UnsafePath(component.into()));
    }
    let key = normalized.to_ascii_lowercase();
    let stem = key.split('.').next().unwrap_or_default();
    let reserved = matches!(stem, "con" | "prn" | "aux" | "nul")
        || stem
            .strip_prefix("com")
            .or_else(|| stem.strip_prefix("lpt"))
            .is_some_and(|number| {
                matches!(number, "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³")
            });
    if reserved {
        return Err(RuntimeError::UnsafePath(component.into()));
    }
    Ok(key)
}

fn validate_kind<R: Read>(entry: &zip::read::ZipFile<'_, R>) -> Result<EntryKind, RuntimeError> {
    let kind = if entry.is_dir() { EntryKind::Directory } else { EntryKind::File };
    let expected_mode = match kind {
        EntryKind::Directory => UNIX_DIRECTORY,
        EntryKind::File => UNIX_REGULAR_FILE,
    };
    if entry.is_symlink()
        || entry
            .unix_mode()
            .is_some_and(|mode| mode & UNIX_FILE_TYPE_MASK != 0 && mode & UNIX_FILE_TYPE_MASK != expected_mode)
    {
        return Err(RuntimeError::UnsupportedEntry(entry.name().into()));
    }
    Ok(kind)
}

fn register_path(
    paths: &mut HashMap<String, EntryKind>,
    components: &[String],
    kind: EntryKind,
    original: &str,
) -> Result<(), RuntimeError> {
    let mut key = String::new();
    for (index, component) in components.iter().enumerate() {
        if index != 0 {
            key.push('/');
        }
        key.push_str(component);
        let component_kind = if index + 1 == components.len() { kind } else { EntryKind::Directory };
        match paths.get(&key).copied() {
            Some(EntryKind::File) => return Err(RuntimeError::PathCollision(original.into())),
            Some(EntryKind::Directory) if index + 1 == components.len() => {
                return Err(RuntimeError::PathCollision(original.into()));
            }
            Some(EntryKind::Directory) => {}
            None => {
                paths.insert(key.clone(), component_kind);
            }
        }
    }
    Ok(())
}
fn extract_entries<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    plans: &[EntryPlan],
    candidate: &Dir,
    limits: ExtractionLimits,
) -> Result<(), RuntimeError> {
    let mut extracted_bytes = 0_u64;
    let mut buffer = [0; COPY_BUFFER_BYTES];
    for (index, plan) in plans.iter().enumerate() {
        let components = plan.path.components().map(|component| match component {
            Component::Normal(component) => Ok(component),
            _ => Err(RuntimeError::UnsafePath(plan.path.display().to_string())),
        });
        let mut directory = candidate.try_clone()?;
        let mut components = components.peekable();
        while let Some(component) = components.next() {
            let component = component?;
            let last = components.peek().is_none();
            if last && plan.kind == EntryKind::File {
                let mut options = OpenOptions::new();
                options
                    .write(true)
                    .create_new(true)
                    .follow(FollowSymlinks::No);
                let mut output = directory.open_with(component, &options)?;
                let mut entry = archive.by_index(index)?;
                loop {
                    let read = entry.read(&mut buffer)?;
                    if read == 0 {
                        break;
                    }
                    extracted_bytes = extracted_bytes
                        .checked_add(read as u64)
                        .ok_or(RuntimeError::TooManyBytes)?;
                    if extracted_bytes > limits.max_uncompressed_bytes {
                        return Err(RuntimeError::TooManyBytes);
                    }
                    output.write_all(&buffer[..read])?;
                }
                apply_file_mode(&output, plan.mode)?;
                output.sync_all()?;
            } else {
                directory = open_or_create_directory(&directory, component)?;
            }
        }
    }
    Ok(())
}

fn open_or_create_directory(parent: &Dir, component: &OsStr) -> Result<Dir, RuntimeError> {
    match parent.create_dir(component) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    open_child_directory(parent, component)
        .map_err(|_| RuntimeError::PathCollision(component.to_string_lossy().into_owned()))
}

#[cfg(not(windows))]
fn open_child_directory(parent: &Dir, component: &OsStr) -> Result<Dir, RuntimeError> {
    parent.open_dir_nofollow(component).map_err(RuntimeError::Io)
}

#[cfg(windows)]
fn open_child_directory(parent: &Dir, component: &OsStr) -> Result<Dir, RuntimeError> {
    nt_create_directory(parent, component, FILE_OPEN)
}

#[cfg(windows)]
fn create_candidate_directory(parent: &Dir, component: &OsStr) -> Result<Dir, RuntimeError> {
    nt_create_directory(parent, component, FILE_CREATE)
}

#[cfg(windows)]
fn nt_create_directory(parent: &Dir, component: &OsStr, disposition: u32) -> Result<Dir, RuntimeError> {
    let name = component.encode_wide().collect::<Vec<_>>();
    let byte_length = u16::try_from(
        name.len()
            .checked_mul(2)
            .ok_or_else(|| RuntimeError::UnsafePath(component.to_string_lossy().into_owned()))?,
    )
    .map_err(|_| RuntimeError::UnsafePath(component.to_string_lossy().into_owned()))?;
    let mut unicode = UNICODE_STRING {
        Length: byte_length,
        MaximumLength: byte_length,
        Buffer: name.as_ptr().cast_mut(),
    };
    let mut attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_raw_handle() as HANDLE,
        ObjectName: &mut unicode,
        Attributes: 0,
        SecurityDescriptor: ptr::null_mut(),
        SecurityQualityOfService: ptr::null_mut(),
    };
    let mut handle = INVALID_HANDLE_VALUE;
    let mut status_block: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            GENERIC_READ | DELETE,
            &mut attributes,
            &mut status_block,
            ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | windows_sys::Win32::Storage::FileSystem::FILE_SHARE_DELETE,
            disposition,
            FILE_DIRECTORY_FILE | FILE_OPEN_FOR_BACKUP_INTENT | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            ptr::null(),
            0,
        )
    };
    if status < 0 {
        return Err(io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(status) } as i32).into());
    }
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::other("NtCreateFile returned an invalid directory handle").into());
    }
    if let Err(error) = validate_windows_directory_handle(handle) {
        unsafe { CloseHandle(handle) };
        return Err(error);
    }
    let file = unsafe { std::fs::File::from_raw_handle(handle) };
    Ok(Dir::from_std_file(file))
}
#[cfg(unix)]
fn apply_file_mode(file: &cap_std::fs::File, mode: Option<u32>) -> Result<(), RuntimeError> {
    if let Some(mode) = mode {
        file.set_permissions(Permissions::from_mode(mode & 0o777))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn apply_file_mode(_file: &cap_std::fs::File, _mode: Option<u32>) -> Result<(), RuntimeError> {
    Ok(())
}

struct CandidateDir {
    path: PathBuf,
    dir: Option<Dir>,
    identity: (u64, u64),
    armed: bool,
}

#[cfg(not(windows))]
fn create_candidate_directory(parent: &Dir, component: &OsStr) -> Result<Dir, RuntimeError> {
    parent.create_dir(component)?;
    match open_child_directory(parent, component) {
        Ok(dir) => Ok(dir),
        Err(error) => {
            let _ = parent.remove_dir(component);
            Err(error)
        }
    }
}
impl CandidateDir {
    fn create(staging: &Path, staging_dir: &Dir) -> Result<Self, RuntimeError> {
        loop {
            let id = NEXT_CANDIDATE.fetch_add(1, Ordering::Relaxed);
            let mut nonce = [0; 16];
            getrandom::fill(&mut nonce).map_err(|error| {
                io::Error::other(format!("OS randomness failed while naming runtime staging: {error}"))
            })?;
            let name = OsString::from(format!(
                ".melon-candidate-{}-{id}-{}",
                std::process::id(),
                nonce.iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
            ));
            match create_candidate_directory(staging_dir, &name) {
                Ok(dir) => {
                    let metadata = dir.dir_metadata()?;
                    return Ok(Self {
                        path: staging.join(&name),
                        dir: Some(dir),
                        identity: (metadata.dev(), metadata.ino()),
                        armed: true,
                    });
                }
                Err(RuntimeError::Io(error)) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn dir(&self) -> &Dir {
        self.dir.as_ref().expect("candidate handle remains live")
    }

    fn publish(&mut self, staging: &Path) -> Result<(), RuntimeError> {
        self.publish_impl(staging, |_| {}, |_| {})
    }

    fn cleanup(&mut self) -> Result<(), RuntimeError> {
        if self.armed {
            cleanup_candidate(self.dir.take().expect("candidate handle remains live"))?;
            self.armed = false;
        }
        Ok(())
    }

    #[cfg(test)]
    fn publish_with_hooks(
        &mut self,
        staging: &Path,
        after_check: impl FnOnce(&Path),
        after_final_rename: impl FnOnce(&Path),
    ) -> Result<(), RuntimeError> {
        self.publish_impl(staging, after_check, after_final_rename)
    }

    #[cfg(not(windows))]
    fn publish_impl(
        &mut self,
        staging: &Path,
        after_check: impl FnOnce(&Path),
        after_final_rename: impl FnOnce(&Path),
    ) -> Result<(), RuntimeError> {
        require_owned_candidate(staging, &self.path, self.identity)?;
        after_check(&self.path);
        let parent = staging.parent().ok_or_else(|| RuntimeError::UnsafePath(staging.display().to_string()))?;
        let name = self.path.file_name().ok_or_else(|| RuntimeError::UnsafePath(self.path.display().to_string()))?;
        let lifted = parent.join(name);
        fs::rename(&self.path, &lifted)?;
        self.path = lifted;
        if candidate_identity(&self.path)? != self.identity {
            self.cleanup()?;
            return Err(RuntimeError::StagingNotEmpty);
        }
        drop(open_empty_staging(staging)?);
        fs::remove_dir(staging)?;
        if let Err(error) = fs::rename(&self.path, staging) {
            fs::create_dir(staging)?;
            fs::set_permissions(
                staging,
                <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
            )?;
            self.cleanup()?;
            return Err(error.into());
        }
        self.path = staging.to_path_buf();
        after_final_rename(staging);
        if candidate_identity(staging)? != self.identity {
            self.cleanup()?;
            return Err(RuntimeError::StagingNotEmpty);
        }
        self.armed = false;
        Ok(())
    }

    #[cfg(windows)]
    fn publish_impl(
        &mut self,
        staging: &Path,
        after_check: impl FnOnce(&Path),
        _after_final_rename: impl FnOnce(&Path),
    ) -> Result<(), RuntimeError> {
        require_owned_candidate(staging, &self.path, self.identity)?;
        after_check(&self.path);
        windows_publish_candidate(
            self.dir.as_ref().expect("candidate handle remains live"),
            &self.path,
            staging,
        )?;
        self.path = staging.to_path_buf();
        self.armed = false;
        Ok(())
    }
}

fn candidate_identity(path: &Path) -> Result<(u64, u64), RuntimeError> {
    let dir = open_runtime_directory(path)?;
    let metadata = dir.dir_metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(not(windows))]
fn cleanup_candidate(dir: Dir) -> Result<(), RuntimeError> {
    dir.remove_open_dir_all()?;
    Ok(())
}

#[cfg(windows)]
fn cleanup_candidate(dir: Dir) -> Result<(), RuntimeError> {
    windows_delete_candidate(dir)
}
#[cfg(windows)]
fn windows_publish_candidate(
    candidate: &Dir,
    candidate_path: &Path,
    staging: &Path,
) -> Result<(), RuntimeError> {
    let parent = staging
        .parent()
        .ok_or_else(|| RuntimeError::UnsafePath(staging.display().to_string()))?;
    let verified_sibling = parent.join(
        candidate_path
            .file_name()
            .ok_or_else(|| RuntimeError::UnsafePath(candidate_path.display().to_string()))?,
    );
    windows_rename_handle(candidate.as_raw_handle() as HANDLE, &verified_sibling)?;
    if candidate_identity(&verified_sibling)? != candidate_identity_from_dir(candidate)? {
        return Err(RuntimeError::StagingNotEmpty);
    }
    let staging_handle = open_runtime_directory(staging)?;
    windows_mark_delete(staging_handle.as_raw_handle() as HANDLE)?;
    drop(staging_handle);
    if let Err(error) = windows_rename_handle(candidate.as_raw_handle() as HANDLE, staging) {
        fs::create_dir(staging)?;
        return Err(error);
    }
    Ok(())
}

#[cfg(windows)]
fn candidate_identity_from_dir(dir: &Dir) -> Result<(u64, u64), RuntimeError> {
    let metadata = dir.dir_metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(windows)]
fn windows_delete_candidate(dir: Dir) -> Result<(), RuntimeError> {
    remove_directory_contents(&dir)?;
    windows_mark_delete(dir.as_raw_handle() as HANDLE)?;
    drop(dir);
    Ok(())
}

#[cfg(windows)]
fn remove_directory_contents(dir: &Dir) -> Result<(), RuntimeError> {
    let entries = dir.entries()?.collect::<Result<Vec<_>, _>>()?;
    for entry in entries {
        let name = entry.file_name();
        let metadata = dir.symlink_metadata(&name)?;
        if metadata.is_dir() && !metadata.is_symlink() {
            let child = open_child_directory(dir, &name)?;
            remove_directory_contents(&child)?;
            windows_mark_delete(child.as_raw_handle() as HANDLE)?;
            drop(child);
        } else {
            dir.remove_file(&name)?;
        }
    }
    Ok(())
}

#[cfg(windows)]
fn windows_mark_delete(handle: HANDLE) -> Result<(), RuntimeError> {
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    let success = unsafe {
        SetFileInformationByHandle(
            handle,
            FileDispositionInfo,
            ptr::from_ref(&disposition).cast(),
            size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    };
    if success == 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(windows)]
fn windows_rename_handle(handle: HANDLE, destination: &Path) -> Result<(), RuntimeError> {
    let destination = fs::canonicalize(
        destination
            .parent()
            .ok_or_else(|| RuntimeError::UnsafePath(destination.display().to_string()))?,
    )?
    .join(
        destination
            .file_name()
            .ok_or_else(|| RuntimeError::UnsafePath(destination.display().to_string()))?,
    );
    let name = destination.as_os_str().encode_wide().collect::<Vec<_>>();
    let name_bytes = name.len().checked_mul(2).ok_or(RuntimeError::InvalidArchive("runtime path is too long"))?;
    let bytes = offset_of!(FILE_RENAME_INFO, FileName) + name_bytes;
    let words = bytes.div_ceil(size_of::<usize>());
    let mut storage = vec![0_usize; words];
    let info = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    unsafe {
        (*info).Anonymous.ReplaceIfExists = false;
        (*info).RootDirectory = ptr::null_mut();
        (*info).FileNameLength = u32::try_from(name_bytes)
            .map_err(|_| RuntimeError::InvalidArchive("runtime path is too long"))?;
        ptr::copy_nonoverlapping(
            name.as_ptr(),
            ptr::addr_of_mut!((*info).FileName).cast::<u16>(),
            name.len(),
        );
    }
    let success = unsafe {
        SetFileInformationByHandle(
            handle,
            FileRenameInfo,
            info.cast(),
            u32::try_from(bytes).map_err(|_| RuntimeError::InvalidArchive("runtime path is too long"))?,
        )
    };
    if success == 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}


fn require_owned_candidate(
    staging: &Path,
    candidate: &Path,
    identity: (u64, u64),
) -> Result<(), RuntimeError> {
    let mut entries = fs::read_dir(staging)?;
    let only = entries.next().transpose()?.ok_or(RuntimeError::StagingNotEmpty)?.path();
    if only != candidate || entries.next().is_some() {
        return Err(RuntimeError::StagingNotEmpty);
    }
    let candidate_dir = open_runtime_directory(candidate)?;
    let metadata = candidate_dir.dir_metadata()?;
    if (metadata.dev(), metadata.ino()) != identity {
        return Err(RuntimeError::StagingNotEmpty);
    }
    Ok(())
}

impl Drop for CandidateDir {
    fn drop(&mut self) {
        if self.armed
            && let Some(dir) = self.dir.take()
        {
            let _ = cleanup_candidate(dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{Cursor, Write};
    use std::path::{Path, PathBuf};
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "melon-runtime-test-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).expect("create test directory");
            #[cfg(unix)]
            fs::set_permissions(
                &path,
                <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
            )
            .expect("secure test cache directory");
            Self(path)
        }

        fn staging(&self) -> PathBuf {
            let path = self.0.join("staging");
            fs::create_dir(&path).expect("create staging directory");
            #[cfg(unix)]
            fs::set_permissions(&path, <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700))
                .expect("secure staging permissions");
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("remove test directory");
        }
    }
    fn archive_with_mode(name: &str, contents: &[u8], mode: u32) -> Vec<u8> {
        let mut bytes = archive(&[(name, contents)]);
        let central = bytes
            .windows(4)
            .position(|window| window == b"PK\x01\x02")
            .expect("central directory");
        bytes[central + 5] = 3;
        bytes[central + 38..central + 42].copy_from_slice(&(mode << 16).to_le_bytes());
        bytes
    }

    fn matching_false_eocd_comment() -> Vec<u8> {
        let mut comment = vec![0; 40];
        comment[..4].copy_from_slice(b"PK\x05\x06");
        comment[4..6].copy_from_slice(&0_u16.to_le_bytes());
        comment[6..8].copy_from_slice(&0_u16.to_le_bytes());
        comment[8..10].copy_from_slice(&1_u16.to_le_bytes());
        comment[10..12].copy_from_slice(&1_u16.to_le_bytes());
        comment[12..16].copy_from_slice(&1_u32.to_le_bytes());
        comment[16..20].copy_from_slice(&1_u32.to_le_bytes());
        comment[20..22].copy_from_slice(&18_u16.to_le_bytes());
        commented_archive(&comment)
    }
    fn matching_zero_entry_false_eocd_comment() -> Vec<u8> {
        let mut comment = vec![0; 40];
        comment[..4].copy_from_slice(b"PK\x05\x06");
        comment[20..22].copy_from_slice(&18_u16.to_le_bytes());
        let provisional = commented_archive(&comment);
        let real_eocd = provisional
            .windows(4)
            .rposition(|window| window == b"PK\x05\x06")
            .expect("real ZIP footer");
        comment[16..20].copy_from_slice(&((real_eocd + 22) as u32).to_le_bytes());
        commented_archive(&comment)
    }

    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for (name, contents) in entries {
            writer.start_file(name, options).expect("start ZIP entry");
            writer.write_all(contents).expect("write ZIP entry");
        }
        writer.finish().expect("finish ZIP").into_inner()
    }
    fn commented_archive(comment: &[u8]) -> Vec<u8> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        writer.set_raw_comment(comment.into()).expect("set ZIP comment");
        writer
            .start_file("node", SimpleFileOptions::default())
            .expect("start ZIP entry");
        writer.write_all(b"node").expect("write ZIP entry");
        writer.finish().expect("finish ZIP").into_inner()
    }

    fn archive_with_unix_link_extra(id: u16, local: bool, central: bool) -> Vec<u8> {
        let mut bytes = archive(&[("link", b"")]);
        let data = if id == 0x756e {
            vec![0; 15]
        } else {
            vec![0; 13]
        };
        let mut field = Vec::with_capacity(data.len() + 4);
        field.extend_from_slice(&id.to_le_bytes());
        field.extend_from_slice(&(data.len() as u16).to_le_bytes());
        field.extend_from_slice(&data);
        let delta = field.len() as u32;
        if local {
            let name_length = u16::from_le_bytes([bytes[26], bytes[27]]) as usize;
            let extra_length = u16::from_le_bytes([bytes[28], bytes[29]]) as usize;
            let insert = 30 + name_length + extra_length;
            bytes.splice(insert..insert, field.iter().copied());
            bytes[28..30].copy_from_slice(&((extra_length + field.len()) as u16).to_le_bytes());
        }
        let central_offset_in_bytes = bytes
            .windows(4)
            .position(|window| window == b"PK\x01\x02")
            .expect("central directory");
        if central {
            let name_length = u16::from_le_bytes([
                bytes[central_offset_in_bytes + 28],
                bytes[central_offset_in_bytes + 29],
            ]) as usize;
            let extra_length = u16::from_le_bytes([
                bytes[central_offset_in_bytes + 30],
                bytes[central_offset_in_bytes + 31],
            ]) as usize;
            let insert = central_offset_in_bytes + 46 + name_length + extra_length;
            bytes.splice(insert..insert, field.iter().copied());
            bytes[central_offset_in_bytes + 30..central_offset_in_bytes + 32]
                .copy_from_slice(&((extra_length + field.len()) as u16).to_le_bytes());
        }
        let eocd = bytes
            .windows(4)
            .position(|window| window == b"PK\x05\x06")
            .expect("ZIP footer");
        let central_size = u32::from_le_bytes(bytes[eocd + 12..eocd + 16].try_into().expect("central size"));
        let central_offset = u32::from_le_bytes(bytes[eocd + 16..eocd + 20].try_into().expect("central offset"));
        if central {
            bytes[eocd + 12..eocd + 16].copy_from_slice(&(central_size + delta).to_le_bytes());
        }
        if local {
            bytes[eocd + 16..eocd + 20].copy_from_slice(&(central_offset + delta).to_le_bytes());
        }
        bytes
    }

    fn symlink_archive() -> Vec<u8> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .add_symlink("link", "outside", SimpleFileOptions::default())
            .expect("add ZIP symlink");
        writer.finish().expect("finish ZIP").into_inner()
    }

    fn non_regular_archive(mode: u32) -> Vec<u8> {
        let mut bytes = archive(&[("special", b"")]);
        let central = bytes
            .windows(4)
            .position(|window| window == b"PK\x01\x02")
            .expect("central directory");
        bytes[central + 5] = 3;
        bytes[central + 38..central + 42].copy_from_slice(&(mode << 16).to_le_bytes());
        bytes
    }

    fn duplicate_archive() -> Vec<u8> {
        vec![
            80, 75, 3, 4, 20, 0, 0, 0, 0, 0, 25, 185, 14, 93, 241, 134, 108, 122, 3, 0, 0, 0,
            3, 0, 0, 0, 6, 0, 0, 0, 115, 97, 109, 101, 45, 97, 111, 110, 101, 80, 75, 3, 4,
            20, 0, 0, 0, 0, 0, 25, 185, 14, 93, 102, 138, 202, 17, 3, 0, 0, 0, 3, 0, 0, 0,
            6, 0, 0, 0, 115, 97, 109, 101, 45, 97, 116, 119, 111, 80, 75, 1, 2, 20, 3, 20, 0,
            0, 0, 0, 0, 25, 185, 14, 93, 241, 134, 108, 122, 3, 0, 0, 0, 3, 0, 0, 0, 6, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 128, 1, 0, 0, 0, 0, 115, 97, 109, 101, 45, 97,
            80, 75, 1, 2, 20, 3, 20, 0, 0, 0, 0, 0, 25, 185, 14, 93, 102, 138, 202, 17, 3, 0,
            0, 0, 3, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 128, 1, 39, 0, 0, 0,
            115, 97, 109, 101, 45, 97, 80, 75, 5, 6, 0, 0, 0, 0, 2, 0, 2, 0, 104, 0, 0, 0,
            78, 0, 0, 0, 0, 0,
        ]
    }
    fn underreported_archive() -> Vec<u8> {
        let mut bytes = archive(&[("one", b"12"), ("two", b"34")]);
        let mut offset = 0;
        while let Some(relative) = bytes[offset..]
            .windows(4)
            .position(|window| window == b"PK\x01\x02")
        {
            let central = offset + relative;
            bytes[central + 24..central + 28].copy_from_slice(&1_u32.to_le_bytes());
            offset = central + 4;
        }
        bytes
    }
    fn hard_link_archive() -> Vec<u8> {
        vec![
            80, 75, 3, 4, 20, 0, 0, 0, 0, 0, 0, 0, 33, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 8, 0, 24, 0, 104, 97, 114, 100, 108, 105, 110, 107, 110, 117, 20, 0, 87, 52,
            153, 115, 164, 129, 0, 0, 0, 0, 0, 0, 0, 0, 116, 97, 114, 103, 101, 116, 80, 75,
            1, 2, 20, 3, 20, 0, 0, 0, 0, 0, 0, 0, 33, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 8, 0, 24, 0, 0, 0, 0, 0, 0, 0, 0, 0, 128, 1, 0, 0, 0, 0, 104, 97, 114,
            100, 108, 105, 110, 107, 110, 117, 20, 0, 87, 52, 153, 115, 164, 129, 0, 0, 0, 0,
            0, 0, 0, 0, 116, 97, 114, 103, 101, 116, 80, 75, 5, 6, 0, 0, 0, 0, 1, 0, 1, 0,
            78, 0, 0, 0, 62, 0, 0, 0, 0, 0,
        ]
    }

    fn assert_empty(path: &Path) {
        assert_eq!(fs::read_dir(path).expect("read staging").count(), 0);
    }

    #[test]
    fn verifies_known_sha256_digest() {
        verify_sha256(
            &mut Cursor::new(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        )
        .expect("known digest");
    }

    #[test]
    fn rejects_sha256_mismatch() {
        let error = verify_sha256(
            &mut Cursor::new(b"abd"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        )
        .expect_err("changed bytes must fail");
        assert!(matches!(error, RuntimeError::DigestMismatch));
    }

    #[test]
    fn rejects_noncanonical_expected_sha256() {
        for expected in [
            "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD",
            "ba7816bf",
            "ga7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            " ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ] {
            let error = verify_sha256(&mut Cursor::new(b"abc"), expected)
                .expect_err("noncanonical digest must fail");
            assert!(matches!(error, RuntimeError::InvalidDigest));
        }
    }

    #[test]
    fn extracts_valid_archive() {
        let temp = TempDir::new();
        let staging = temp.staging();
        extract_zip(
            &mut Cursor::new(archive(&[("bin/node", b"node"), ("cli.js", b"cli")])),
            &staging,
        )
        .expect("extract archive");
        assert_eq!(fs::read(staging.join("bin/node")).expect("node"), b"node");
        assert_eq!(fs::read(staging.join("cli.js")).expect("cli"), b"cli");
    }

    #[test]
    fn verify_and_extract_publishes_valid_digest_without_candidate_residue() {
        let temp = TempDir::new();
        let staging = temp.staging();
        let bytes = archive(&[("bin/node", b"node")]);
        let digest = format!("{:x}", Sha256::digest(&bytes));
        verify_and_extract_zip(&mut Cursor::new(bytes), &digest, &staging)
            .expect("verify and extract archive");
        let entries = fs::read_dir(&staging)
            .expect("read published staging")
            .map(|entry| entry.expect("published entry").file_name())
            .collect::<Vec<_>>();
        assert_eq!(entries, ["bin"]);
        assert_eq!(fs::read(staging.join("bin/node")).expect("published node"), b"node");
    }

    #[test]
    fn accepts_false_eocd_signature_inside_legal_comment() {
        let temp = TempDir::new();
        let staging = temp.staging();
        let mut comment = b"legal comment PK\x05\x06".to_vec();
        comment.extend_from_slice(&[0xff; 18]);
        extract_zip(&mut Cursor::new(commented_archive(&comment)), &staging)
            .expect("false footer signature in comment");
        assert_eq!(fs::read(staging.join("node")).expect("node"), b"node");
    }
    #[test]
    fn accepts_matching_length_false_eocd_inside_comment() {
        for bytes in [matching_false_eocd_comment(), matching_zero_entry_false_eocd_comment()] {
            let temp = TempDir::new();
            let staging = temp.staging();
            extract_zip(&mut Cursor::new(bytes), &staging)
                .expect("structurally false footer in comment");
            assert_eq!(fs::read(staging.join("node")).expect("node"), b"node");
        }
    }

    #[test]
    fn rejects_extended_windows_device_stems() {
        for name in ["COM0", "LPT0.txt", "COM¹.log", "COM²", "COM³", "LPT¹", "LPT²", "LPT³"] {
            assert_rejected_path(name);
        }
    }

    #[cfg(unix)]
    #[test]
    fn preserves_sanitized_unix_executable_permissions() {
        use std::os::unix::fs::PermissionsExt;

        for (mode, expected) in [(0o100755, 0o755), (0o107755, 0o755)] {
            let temp = TempDir::new();
            let staging = temp.staging();
            extract_zip(&mut Cursor::new(archive_with_mode("node", b"node", mode)), &staging)
                .expect("extract executable");
            assert_eq!(fs::metadata(staging.join("node")).expect("node metadata").permissions().mode() & 0o7777, expected);
        }
    }

    #[cfg(unix)]
    #[test]
    fn post_identity_replacement_never_publishes_attacker_directory() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new();
        let staging = temp.staging();
        let attacker = temp.0.join("attacker");
        let moved = temp.0.join("owned-moved");
        fs::create_dir(&attacker).expect("attacker directory");
        fs::write(attacker.join("marker"), b"attacker").expect("attacker marker");
        let error = extract_zip_with_publish_hooks(
            &mut Cursor::new(archive(&[("node", b"owned")])),
            &staging,
            |candidate| {
                fs::rename(candidate, &moved).expect("move verified candidate");
                symlink(&attacker, candidate).expect("replace verified candidate");
            },
            |_| {},
        )
        .expect_err("post-identity replacement must fail closed");
        assert!(!staging.join("marker").exists(), "attacker directory must not publish");
        assert!(!moved.exists(), "owned original must be cleaned by handle");
        assert_eq!(fs::read(attacker.join("marker")).expect("attacker untouched"), b"attacker");
        drop(error);
    }
    #[cfg(unix)]
    #[test]
    fn final_transition_replacement_never_disarms_owned_candidate() {
        let temp = TempDir::new();
        let staging = temp.staging();
        let attacker = temp.0.join("attacker");
        let moved = temp.0.join("owned-after-final-rename");
        fs::create_dir(&attacker).expect("attacker directory");
        fs::write(attacker.join("marker"), b"attacker").expect("attacker marker");
        let error = extract_zip_with_publish_hooks(
            &mut Cursor::new(archive(&[("node", b"owned")])),
            &staging,
            |_| {},
            |published| {
                fs::rename(published, &moved).expect("move published owned candidate");
                fs::rename(&attacker, published).expect("replace final path");
            },
        )
        .expect_err("final transition replacement must fail closed");
        assert!(matches!(error, RuntimeError::StagingNotEmpty), "{error:?}");
        assert!(!moved.exists(), "owned original must be cleaned by handle");
        assert_eq!(fs::read(staging.join("marker")).expect("attacker replacement remains"), b"attacker");
    }

    #[test]
    fn rejects_windows_reserved_names_and_alias_collisions() {
        for name in ["NUL", "con.txt", "COM1.bin", "lpt9"] {
            assert_rejected_path(name);
        }
        for name in ["node.", "node ", "dir /file"] {
            assert_rejected_path(name);
        }
    }

    #[test]
    fn rejects_local_and_central_unix_link_metadata() {
        for (id, local, central) in [
            (0x756e, true, false),
            (0x756e, false, true),
            (0x000d, true, false),
            (0x000d, false, true),
        ] {
            let temp = TempDir::new();
            let staging = temp.staging();
            let error = extract_zip(
                &mut Cursor::new(archive_with_unix_link_extra(id, local, central)),
                &staging,
            )
            .expect_err("UNIX link metadata must fail");
            assert!(matches!(error, RuntimeError::UnsupportedEntry(_)), "{error:?}");
            assert_empty(&staging);
        }
    }

    #[cfg(unix)]
    #[test]
    fn candidate_root_replacement_cannot_redirect_writes() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new();
        let staging = temp.staging();
        let outside = temp.0.join("outside");
        let moved = temp.0.join("moved-candidate");
        fs::create_dir(&outside).expect("outside directory");
        let error = extract_zip_with_hook(
            &mut Cursor::new(archive(&[("bin/node", b"owned")])),
            &staging,
            DEFAULT_EXTRACTION_LIMITS,
            |candidate| {
                fs::rename(candidate, &moved).expect("move candidate");
                symlink(&outside, candidate).expect("replace candidate path");
            },
        )
        .expect_err("replacement race must fail closed");
        assert!(!outside.join("bin/node").exists());
        assert!(!moved.exists(), "owned candidate must be removed by handle");
        assert!(candidate_symlink_in(&staging), "attacker replacement remains caller-owned");
        drop(error);
    }

    #[cfg(unix)]
    fn candidate_symlink_in(staging: &Path) -> bool {
        fs::read_dir(staging).expect("read staging").any(|entry| {
            entry
                .expect("staging entry")
                .file_type()
                .expect("entry type")
                .is_symlink()
        })
    }

    #[test]
    fn digest_mismatch_never_publishes_archive() {
        let temp = TempDir::new();
        let staging = temp.staging();
        let outside = temp.0.join("outside");
        fs::write(&outside, b"sentinel").expect("outside sentinel");
        let error = verify_and_extract_zip(
            &mut Cursor::new(archive(&[("node", b"owned")])),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            &staging,
        )
        .expect_err("digest mismatch must fail before extraction");
        assert!(matches!(error, RuntimeError::DigestMismatch));
        assert_empty(&staging);
        assert_eq!(fs::read(outside).expect("outside untouched"), b"sentinel");
    }

    #[test]
    fn rejects_parent_traversal_without_touching_outside() {
        assert_rejected_path("../escape");
    }

    #[test]
    fn rejects_absolute_path_without_touching_outside() {
        assert_rejected_path("/tmp/melon-runtime-escape");
    }

    #[test]
    fn rejects_windows_drive_and_backslash_paths() {
        for name in ["C:/escape", "C:\\escape", "dir\\..\\escape", "\\\\server\\share"] {
            assert_rejected_path(name);
        }
    }

    fn assert_rejected_path(name: &str) {
        let temp = TempDir::new();
        let staging = temp.staging();
        let outside = temp.0.join("escape");
        fs::write(&outside, b"sentinel").expect("outside sentinel");
        let error = extract_zip(&mut Cursor::new(archive(&[(name, b"owned")])), &staging)
            .expect_err("unsafe path must fail");
        assert!(matches!(error, RuntimeError::UnsafePath(_)), "{error:?}");
        assert_empty(&staging);
        assert_eq!(fs::read(outside).expect("outside untouched"), b"sentinel");
    }

    #[test]
    fn rejects_symlink_and_non_regular_entries() {
        for (label, bytes) in [
            ("symlink", symlink_archive()),
            ("hard link", hard_link_archive()),
            ("FIFO", non_regular_archive(0o010644)),
            ("device", non_regular_archive(0o060644)),
            ("socket", non_regular_archive(0o140644)),
        ] {
            let temp = TempDir::new();
            let staging = temp.staging();
            let error = extract_zip(&mut Cursor::new(bytes), &staging)
                .expect_err("non-regular entry must fail");
            assert!(matches!(error, RuntimeError::UnsupportedEntry(_)), "{label}: {error:?}");
            assert_empty(&staging);
        }
    }

    #[test]
    fn rejects_duplicate_paths() {
        let temp = TempDir::new();
        let staging = temp.staging();
        let error = extract_zip(&mut Cursor::new(duplicate_archive()), &staging)
            .expect_err("duplicate path must fail");
        assert!(matches!(error, RuntimeError::PathCollision(_)), "{error:?}");
        assert_empty(&staging);
    }

    #[test]
    fn rejects_file_directory_collisions() {
        let temp = TempDir::new();
        let staging = temp.staging();
        let error = extract_zip(
            &mut Cursor::new(archive(&[("node", b"file"), ("node/bin", b"child")])),
            &staging,
        )
        .expect_err("file/directory collision must fail");
        assert!(matches!(error, RuntimeError::PathCollision(_)), "{error:?}");
        assert_empty(&staging);
    }

    #[test]
    fn enforces_entry_count_limit() {
        let temp = TempDir::new();
        let staging = temp.staging();
        let limits = ExtractionLimits { max_entries: 1, max_uncompressed_bytes: 100 };
        let error = extract_zip_with_limits(
            &mut Cursor::new(archive(&[("one", b"1"), ("two", b"2")])),
            &staging,
            limits,
        )
        .expect_err("entry count must be bounded");
        assert!(matches!(error, RuntimeError::TooManyEntries));
        assert_empty(&staging);
    }

    #[test]
    fn enforces_actual_extracted_byte_limit() {
        let temp = TempDir::new();
        let staging = temp.staging();
        let limits = ExtractionLimits { max_entries: 2, max_uncompressed_bytes: 3 };
        let error = extract_zip_with_limits(
            &mut Cursor::new(underreported_archive()),
            &staging,
            limits,
        )
        .expect_err("uncompressed bytes must be bounded");
        assert!(matches!(error, RuntimeError::TooManyBytes));
        assert_empty(&staging);
    }

    #[test]
    fn late_invalid_entry_leaves_staging_empty_and_outside_untouched() {
        let temp = TempDir::new();
        let staging = temp.staging();
        let outside = temp.0.join("outside");
        fs::write(&outside, b"sentinel").expect("outside sentinel");
        let error = extract_zip(
            &mut Cursor::new(archive(&[("valid", b"partial"), ("../outside", b"bad")])),
            &staging,
        )
        .expect_err("late invalid entry must fail before extraction");
        assert!(matches!(error, RuntimeError::UnsafePath(_)));
        assert_empty(&staging);
        assert_eq!(fs::read(outside).expect("outside untouched"), b"sentinel");
    }
    #[cfg(unix)]
    #[test]
    fn rejects_nonprivate_staging_and_unsafe_parent() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new();
        let staging = temp.staging();
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o755)).expect("loosen staging");
        let error = extract_zip(&mut Cursor::new(archive(&[("node", b"node")])), &staging)
            .expect_err("nonprivate staging must fail");
        assert!(matches!(error, RuntimeError::StagingNotEmpty), "{error:?}");

        let unsafe_parent = temp.0.join("unsafe-parent");
        fs::create_dir(&unsafe_parent).expect("unsafe parent");
        fs::set_permissions(&unsafe_parent, fs::Permissions::from_mode(0o777)).expect("loosen parent");
        let unsafe_staging = unsafe_parent.join("staging");
        fs::create_dir(&unsafe_staging).expect("unsafe staging");
        fs::set_permissions(&unsafe_staging, fs::Permissions::from_mode(0o700)).expect("private child");
        let error = extract_zip(&mut Cursor::new(archive(&[("node", b"node")])), &unsafe_staging)
            .expect_err("peer-writable parent without sticky bit must fail");
        assert!(matches!(error, RuntimeError::StagingNotEmpty), "{error:?}");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_preexisting_symlink_component() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new();
        let staging = temp.staging();
        let outside = temp.0.join("outside");
        fs::create_dir(&outside).expect("outside directory");
        symlink(&outside, staging.join("bin")).expect("staging symlink");
        let error = extract_zip(&mut Cursor::new(archive(&[("bin/node", b"bad")])), &staging)
            .expect_err("pre-existing symlink must fail");
        assert!(matches!(error, RuntimeError::StagingNotEmpty));
        assert!(!outside.join("node").exists());
        assert!(staging.join("bin").symlink_metadata().expect("symlink remains").file_type().is_symlink());
    }
    #[cfg(windows)]
    #[test]
    fn windows_rejects_staging_junction_without_touching_target() {
        let temp = TempDir::new();
        let staging = temp.staging();
        let outside = temp.0.join("outside");
        fs::create_dir(&outside).expect("outside target");
        fs::remove_dir(&staging).expect("replace staging with junction");
        let output = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&staging)
            .arg(&outside)
            .output()
            .expect("create staging junction");
        assert!(output.status.success(), "mklink failed: {}", String::from_utf8_lossy(&output.stderr));

        let error = extract_zip(&mut Cursor::new(archive(&[("node", b"owned")])), &staging)
            .expect_err("staging junction must fail closed");
        assert!(matches!(error, RuntimeError::StagingNotEmpty), "{error:?}");
        assert_empty(&outside);
    }

    #[cfg(windows)]
    #[test]
    fn windows_handle_publication_succeeds() {
        let temp = TempDir::new();
        let staging = temp.staging();
        extract_zip(&mut Cursor::new(archive(&[("node", b"node")])), &staging)
            .expect("publish through Windows directory handle");
        assert_eq!(fs::read(staging.join("node")).expect("published node"), b"node");
    }

    #[cfg(windows)]
    #[test]
    fn windows_nt_create_file_failure_never_adopts_invalid_handle() {
        let temp = TempDir::new();
        let staging = temp.staging();
        let staging_dir = open_empty_staging(&staging).expect("staging handle");
        let error = nt_create_directory(&staging_dir, OsStr::new("missing"), FILE_OPEN)
            .expect_err("opening a missing directory must fail");
        assert!(matches!(error, RuntimeError::Io(_)), "{error:?}");
    }

    #[cfg(windows)]
    #[test]
    fn windows_handle_cleanup_removes_owned_candidate() {
        let temp = TempDir::new();
        let staging = temp.staging();
        let staging_dir = open_empty_staging(&staging).expect("staging handle");
        let mut candidate = CandidateDir::create(&staging, &staging_dir).expect("candidate");
        candidate.dir().create_dir("nested").expect("nested candidate content");
        let nested = open_child_directory(candidate.dir(), OsStr::new("nested")).expect("nested handle");
        nested.create_dir("deeper").expect("deeper candidate content");
        drop(nested);
        let path = candidate.path().to_path_buf();
        candidate.cleanup().expect("handle cleanup");
        assert!(!path.exists());
    }

}
