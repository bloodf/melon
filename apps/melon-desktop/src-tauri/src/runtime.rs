use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt;
use std::fs::{self, OpenOptions};
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

/// Validates every ZIP entry, extracts into an owned sibling candidate, then replaces only the empty staging directory.
pub(crate) fn extract_zip(reader: &mut (impl Read + Seek), staging: &Path) -> Result<(), RuntimeError> {
    extract_zip_with_limits(reader, staging, DEFAULT_EXTRACTION_LIMITS)
}

fn extract_zip_with_limits(
    reader: &mut (impl Read + Seek),
    staging: &Path,
    limits: ExtractionLimits,
) -> Result<(), RuntimeError> {
    let declared_entries = declared_entry_count(reader)?;
    if declared_entries > limits.max_entries {
        return Err(RuntimeError::TooManyEntries);
    }
    require_empty_staging(staging)?;
    let mut archive = ZipArchive::new(reader)?;
    if declared_entries != archive.len() {
        return Err(RuntimeError::PathCollision("duplicate central-directory name".into()));
    }
    let plans = validate_entries(&mut archive, limits)?;
    let mut candidate = CandidateDir::create(staging)?;
    let extraction = extract_entries(&mut archive, &plans, candidate.path(), limits);
    if let Err(error) = extraction {
        candidate.cleanup()?;
        return Err(error);
    }
    candidate.publish(staging)?;
    Ok(())
}

fn require_empty_staging(staging: &Path) -> Result<(), RuntimeError> {
    let metadata = fs::symlink_metadata(staging).map_err(RuntimeError::Io)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() || fs::read_dir(staging)?.next().is_some() {
        return Err(RuntimeError::StagingNotEmpty);
    }
    Ok(())
}

fn declared_entry_count(reader: &mut (impl Read + Seek)) -> Result<usize, RuntimeError> {
    const EOCD_BYTES: u64 = 22;
    const MAX_COMMENT_BYTES: u64 = u16::MAX as u64;
    let length = reader.seek(io::SeekFrom::End(0))?;
    if length < EOCD_BYTES {
        return Err(RuntimeError::InvalidArchive("missing end-of-central-directory record"));
    }
    let tail_length = length.min(EOCD_BYTES + MAX_COMMENT_BYTES) as usize;
    reader.seek(io::SeekFrom::End(-(tail_length as i64)))?;
    let mut tail = vec![0; tail_length];
    reader.read_exact(&mut tail)?;
    let offset = tail
        .windows(4)
        .rposition(|window| window == b"PK\x05\x06")
        .ok_or(RuntimeError::InvalidArchive("missing end-of-central-directory record"))?;
    if tail.len() - offset < EOCD_BYTES as usize {
        return Err(RuntimeError::InvalidArchive("truncated end-of-central-directory record"));
    }
    let disk = u16::from_le_bytes([tail[offset + 4], tail[offset + 5]]);
    let directory_disk = u16::from_le_bytes([tail[offset + 6], tail[offset + 7]]);
    let disk_entries = u16::from_le_bytes([tail[offset + 8], tail[offset + 9]]);
    let total_entries = u16::from_le_bytes([tail[offset + 10], tail[offset + 11]]);
    if disk != 0 || directory_disk != 0 || disk_entries != total_entries {
        return Err(RuntimeError::InvalidArchive("multi-disk ZIPs are unsupported"));
    }
    if total_entries == u16::MAX {
        return Err(RuntimeError::InvalidArchive("ZIP64 entry tables are unsupported"));
    }
    Ok(total_entries as usize)
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
        plans.push(EntryPlan { path, kind });
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
        keys.push(component.to_ascii_lowercase());
    }
    if path.components().any(|component| !matches!(component, Component::Normal(_))) {
        return Err(RuntimeError::UnsafePath(name.into()));
    }
    Ok((path, keys))
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
    if contains_hard_link_metadata(entry.extra_data().unwrap_or_default()) {
        return Err(RuntimeError::UnsupportedEntry(entry.name().into()));
    }
    Ok(kind)
}
fn contains_hard_link_metadata(mut extra: &[u8]) -> bool {
    while extra.len() >= 4 {
        let id = u16::from_le_bytes([extra[0], extra[1]]);
        let length = u16::from_le_bytes([extra[2], extra[3]]) as usize;
        if extra.len() < 4 + length {
            return true;
        }
        if id == 0x756e && length > 14 {
            return true;
        }
        extra = &extra[4 + length..];
    }
    !extra.is_empty()
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
    candidate: &Path,
    limits: ExtractionLimits,
) -> Result<(), RuntimeError> {
    let mut extracted_bytes = 0_u64;
    let mut buffer = [0; COPY_BUFFER_BYTES];
    for (index, plan) in plans.iter().enumerate() {
        let output = candidate.join(&plan.path);
        if plan.kind == EntryKind::Directory {
            create_directory_chain(candidate, &plan.path)?;
            continue;
        }
        if let Some(parent) = plan.path.parent() {
            create_directory_chain(candidate, parent)?;
        }
        let mut entry = archive.by_index(index)?;
        let mut output = OpenOptions::new().write(true).create_new(true).open(output)?;
        loop {
            let read = entry.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            extracted_bytes = extracted_bytes.checked_add(read as u64).ok_or(RuntimeError::TooManyBytes)?;
            if extracted_bytes > limits.max_uncompressed_bytes {
                return Err(RuntimeError::TooManyBytes);
            }
            output.write_all(&buffer[..read])?;
        }
        output.sync_all()?;
    }
    Ok(())
}

fn create_directory_chain(root: &Path, relative: &Path) -> Result<(), RuntimeError> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(RuntimeError::UnsafePath(relative.display().to_string()));
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => return Err(RuntimeError::PathCollision(relative.display().to_string())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(&current)?,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

struct CandidateDir {
    path: PathBuf,
    armed: bool,
}

impl CandidateDir {
    fn create(staging: &Path) -> Result<Self, RuntimeError> {
        loop {
            let id = NEXT_CANDIDATE.fetch_add(1, Ordering::Relaxed);
            let path = staging.join(format!(".melon-candidate-{}-{id}", std::process::id()));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path, armed: true }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn publish(&mut self, staging: &Path) -> Result<(), RuntimeError> {
        require_owned_candidate(staging, &self.path)?;
        let parent = staging.parent().ok_or_else(|| RuntimeError::UnsafePath(staging.display().to_string()))?;
        let name = self.path.file_name().ok_or_else(|| RuntimeError::UnsafePath(self.path.display().to_string()))?;
        let lifted = parent.join(name);
        fs::rename(&self.path, &lifted)?;
        self.path = lifted;
        require_empty_staging(staging)?;
        fs::remove_dir(staging)?;
        if let Err(error) = fs::rename(&self.path, staging) {
            fs::create_dir(staging)?;
            self.cleanup()?;
            return Err(error.into());
        }
        self.armed = false;
        Ok(())
    }

    fn cleanup(&mut self) -> Result<(), RuntimeError> {
        if self.armed {
            fs::remove_dir_all(&self.path)?;
            self.armed = false;
        }
        Ok(())
    }
}


fn require_owned_candidate(staging: &Path, candidate: &Path) -> Result<(), RuntimeError> {
    let mut entries = fs::read_dir(staging)?;
    let only = entries.next().transpose()?.ok_or(RuntimeError::StagingNotEmpty)?.path();
    if only != candidate || entries.next().is_some() {
        return Err(RuntimeError::StagingNotEmpty);
    }
    let metadata = fs::symlink_metadata(candidate)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(RuntimeError::StagingNotEmpty);
    }
    Ok(())
}

impl Drop for CandidateDir {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{Cursor, Write};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
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
            Self(path)
        }

        fn staging(&self) -> PathBuf {
            let path = self.0.join("staging");
            fs::create_dir(&path).expect("create staging directory");
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("remove test directory");
        }
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
}
