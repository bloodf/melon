//! Fail-closed validation and native probing for target-specific runtime seed payloads.

#[cfg(target_os = "linux")]
use cap_fs_ext::{DirExt as _, FollowSymlinks, OpenOptionsFollowExt as _};
#[cfg(target_os = "linux")]
use cap_std::ambient_authority;
#[cfg(target_os = "linux")]
use cap_std::fs::{Dir, OpenOptions};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
#[cfg(target_os = "linux")]
use std::collections::HashSet;
use std::fmt;
#[cfg(target_os = "linux")]
use std::io::{self, Read};
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
#[cfg(target_os = "linux")]
use cap_std::fs::{MetadataExt as _, PermissionsExt as _};
#[cfg(target_os = "linux")]
use std::path::{Component, Path, PathBuf};
#[cfg(not(target_os = "linux"))]
use std::path::Path;
#[cfg(target_os = "linux")]
use std::process::{Command, ExitStatus, Stdio};
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};
#[cfg(target_os = "linux")]
use unicode_normalization::UnicodeNormalization as _;

#[cfg(target_os = "linux")]
use crate::process_tree::ProcessTree;

const MAX_FILES: usize = 16_384;
const MAX_TOTAL_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_COMPONENT_BYTES: usize = 128;
const COPY_BUFFER_BYTES: usize = 64 * 1024;
const PROBE_OUTPUT_BYTES: usize = 64 * 1024;
#[cfg(target_os = "linux")]
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(target_os = "linux")]
const PROBE_STOP_GRACE: Duration = Duration::from_millis(100);
const PROBE_OK: &[u8] = b"melon-runtime-seed-probe-v1-ok\n";

const DURINDOOR_VERSION: &str = "3.15.2";
const NODE_VERSION: &str = "20.20.2";
const NODE_ABI: &str = "115";
const RUNTIME_SEED_PATH: &str = "runtime-seed";
const MANIFEST_PATH: &str = "metadata/runtime-seed-manifest.json";
const CLI_PATH: &str = "app/node_modules/durindoor/cli.js";
const NODE_PATH_UNIX: &str = "bin/node";
const NODE_PATH_WINDOWS: &str = "bin/node.exe";
const DURINDOOR_LICENSE_PATH: &str = "licenses/durindoor-LICENSE";
const NODE_LICENSE_PATH: &str = "licenses/node-LICENSE";
const NOTICES_PATH: &str = "licenses/THIRD_PARTY_NOTICES.json";
const BETTER_PACKAGE: &str = "node_modules/better-sqlite3/package.json";
const BETTER_ENTRY: &str = "node_modules/better-sqlite3/lib/index.js";
const BETTER_BINARY: &str = "node_modules/better-sqlite3/build/Release/better_sqlite3.node";
const BETTER_SQLITE_VERSION: &str = "12.6.2";
const SQL_PACKAGE: &str = "node_modules/sql.js/package.json";
const SQL_ENTRY: &str = "node_modules/sql.js/dist/sql-wasm.js";
const SQL_WASM: &str = "node_modules/sql.js/dist/sql-wasm.wasm";
const SQL_JS_VERSION: &str = "1.14.1";

const PROBE_V1: &str = r#"
const path = require('node:path');
const root = process.argv[1];
try {
  const Database = require(path.join(root, 'node_modules/better-sqlite3/lib/index.js'));
  const native = new Database(':memory:');
  native.exec('CREATE TABLE melon_probe(value INTEGER); INSERT INTO melon_probe VALUES (1)');
  if (native.prepare('SELECT value FROM melon_probe').pluck().get() !== 1) throw new Error('native probe mismatch');
  native.close();
  const initSqlJs = require(path.join(root, 'node_modules/sql.js/dist/sql-wasm.js'));
  Promise.resolve(initSqlJs({ locateFile: () => path.join(root, 'node_modules/sql.js/dist/sql-wasm.wasm') })).then(SQL => {
    const wasm = new SQL.Database();
    wasm.run('CREATE TABLE melon_probe(value INTEGER); INSERT INTO melon_probe VALUES (1)');
    if (wasm.exec('SELECT value FROM melon_probe')[0].values[0][0] !== 1) throw new Error('wasm probe mismatch');
    wasm.close();
    process.stdout.write('melon-runtime-seed-probe-v1-ok\n');
  }).catch(() => process.exit(1));
} catch (_) { process.exit(1); }
"#;

/// Redacted runtime-seed validation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SeedError {
    Descriptor,
    Manifest,
    UnsafePath,
    NativeProbeRequired,
    UnsupportedPlatform,
    ProbeFailed,
    ProbeTimeout,
    ProbeOutputLimit,
    Cleanup,
    Io,
}

impl fmt::Display for SeedError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Descriptor => "runtime seed descriptor is invalid",
            Self::Manifest => "runtime seed manifest validation failed",
            Self::UnsafePath => "runtime seed contains an unsafe path or object",
            Self::NativeProbeRequired => "runtime seed installation requires target-native release-gate evidence",
            Self::UnsupportedPlatform => "runtime seed verification is unsupported on this platform",
            Self::ProbeFailed => "runtime seed probe failed",
            Self::ProbeTimeout => "runtime seed probe timed out",
            Self::ProbeOutputLimit => "runtime seed probe exceeded its output limit",
            Self::Cleanup => "runtime seed process cleanup failed",
            Self::Io => "runtime seed I/O failed",
        })
    }
}

impl std::error::Error for SeedError {}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PayloadDescriptor {
    schema_version: u32,
    target: String,
    durindoor_version: String,
    node_version: String,
    node_abi: String,
    toolchain: Toolchain,
    cli: String,
    node: String,
    runtime_seed_path: String,
    runtime_seed_manifest: ManifestDescriptor,
    managed_launch_ready: bool,
    licenses: Licenses,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Toolchain {
    python: ToolEvidence,
    cc: ToolEvidence,
    cxx: ToolEvidence,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolEvidence {
    name: String,
    version: String,
    sha256: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Licenses {
    durindoor: String,
    node: String,
    notices: String,
    sha256: LicenseDigests,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LicenseDigests {
    durindoor: String,
    node: String,
    notices: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ManifestDescriptor {
    path: String,
    sha256: String,
    file_count: usize,
    total_bytes: u64,
    destination: String,
    probe_version: u32,
    modules: Modules,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Modules {
    better_sqlite3: BetterSqlite,
    sql_js: SqlJs,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BetterSqlite {
    package_path: String,
    binary_path: String,
    version: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SqlJs {
    package_path: String,
    wasm_path: String,
    version: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SeedManifest {
    schema_version: u32,
    files: Vec<SeedFile>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SeedFile {
    path: String,
    size: u64,
    sha256: String,
    executable: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct Identity(u64, u64);

#[cfg(target_os = "linux")]
struct CriticalFile {
    path: &'static str,
    identity: Identity,
    file: cap_std::fs::File,
}

#[cfg(target_os = "linux")]
struct ValidatedSeed {
    source: Dir,
    node: cap_std::fs::File,
    files: Vec<SeedFile>,
    critical: Vec<CriticalFile>,
}

/// Refuses installation until the exact target-native payload passes [`verify_native_runtime_seed`]
/// on its release runner. No destination path is opened or mutated.
pub(crate) fn install_runtime_seed(_payload_root: &Path, _destination: &Path) -> Result<(), SeedError> {
    if cfg!(target_os = "linux") {
        Err(SeedError::NativeProbeRequired)
    } else {
        Err(SeedError::UnsupportedPlatform)
    }
}

/// Performs the target-native release gate without installing or modifying runtime data.
///
/// Linux is the only implemented verifier because it can execute both Node and the seed root through
/// inherited retained descriptors. Other platforms fail closed until equivalent native mechanisms and
/// tests exist.
pub(crate) fn verify_native_runtime_seed(payload_root: &Path) -> Result<(), SeedError> {
    #[cfg(target_os = "linux")]
    {
        let validated = validate_seed(payload_root)?;
        run_probe_v1(&validated, PROBE_V1)?;
        revalidate_seed(&validated)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = payload_root;
        Err(SeedError::UnsupportedPlatform)
    }
}

#[cfg(target_os = "linux")]
fn validate_seed(payload_root: &Path) -> Result<ValidatedSeed, SeedError> {
    let payload = open_private_root(payload_root)?;
    let descriptor_bytes = read_cap_file(&payload, "payload.json", MAX_TOTAL_BYTES)?;
    let descriptor: PayloadDescriptor = serde_json::from_slice(&descriptor_bytes).map_err(|_| SeedError::Descriptor)?;
    validate_descriptor(&payload, &descriptor)?;
    let manifest_bytes = read_cap_file(&payload, MANIFEST_PATH, MAX_TOTAL_BYTES)?;
    if sha256(&manifest_bytes) != descriptor.runtime_seed_manifest.sha256 {
        return Err(SeedError::Manifest);
    }
    let manifest: SeedManifest = serde_json::from_slice(&manifest_bytes).map_err(|_| SeedError::Manifest)?;
    validate_manifest(&manifest, &descriptor.runtime_seed_manifest)?;
    let source = open_cap_directory(&payload, RUNTIME_SEED_PATH)?;
    validate_source_set(&source, &manifest.files)?;
    validate_package(&source, BETTER_PACKAGE, "better-sqlite3", BETTER_SQLITE_VERSION)?;
    validate_package(&source, SQL_PACKAGE, "sql.js", SQL_JS_VERSION)?;
    let node = open_cap_file(&payload, current_node_path())?;
    validate_file_security(&node, 0o755, SeedError::Descriptor)?;
    let mut critical = Vec::new();
    for path in [BETTER_PACKAGE, BETTER_ENTRY, BETTER_BINARY, SQL_PACKAGE, SQL_ENTRY, SQL_WASM] {
        let file = open_cap_file(&source, path).map_err(|_| SeedError::Manifest)?;
        let metadata = file.metadata().map_err(|_| SeedError::Manifest)?;
        critical.push(CriticalFile { path, identity: identity(&metadata), file });
    }
    Ok(ValidatedSeed { source, node, files: manifest.files, critical })
}

#[cfg(target_os = "linux")]
fn validate_descriptor(payload: &Dir, descriptor: &PayloadDescriptor) -> Result<(), SeedError> {
    let modules = &descriptor.runtime_seed_manifest.modules;
    if descriptor.schema_version != 1
        || descriptor.target != current_target()
        || descriptor.durindoor_version != DURINDOOR_VERSION
        || descriptor.node_version != NODE_VERSION
        || descriptor.node_abi != NODE_ABI
        || descriptor.cli != CLI_PATH
        || descriptor.node != current_node_path()
        || descriptor.runtime_seed_path != RUNTIME_SEED_PATH
        || descriptor.managed_launch_ready
        || descriptor.runtime_seed_manifest.path != MANIFEST_PATH
        || descriptor.runtime_seed_manifest.destination != "data-runtime-root"
        || descriptor.runtime_seed_manifest.probe_version != 1
        || descriptor.runtime_seed_manifest.file_count == 0
        || descriptor.runtime_seed_manifest.file_count > MAX_FILES
        || descriptor.runtime_seed_manifest.total_bytes > MAX_TOTAL_BYTES
        || modules.better_sqlite3.package_path != BETTER_PACKAGE
        || modules.better_sqlite3.binary_path != BETTER_BINARY
        || modules.better_sqlite3.version != BETTER_SQLITE_VERSION
        || modules.sql_js.package_path != SQL_PACKAGE
        || modules.sql_js.wasm_path != SQL_WASM
        || modules.sql_js.version != SQL_JS_VERSION
        || descriptor.licenses.durindoor != DURINDOOR_LICENSE_PATH
        || descriptor.licenses.node != NODE_LICENSE_PATH
        || descriptor.licenses.notices != NOTICES_PATH
    {
        return Err(SeedError::Descriptor);
    }
    for digest in [
        &descriptor.runtime_seed_manifest.sha256,
        &descriptor.toolchain.python.sha256,
        &descriptor.toolchain.cc.sha256,
        &descriptor.toolchain.cxx.sha256,
        &descriptor.licenses.sha256.durindoor,
        &descriptor.licenses.sha256.node,
        &descriptor.licenses.sha256.notices,
    ] {
        validate_digest(digest).map_err(|_| SeedError::Descriptor)?;
    }
    for evidence in [&descriptor.toolchain.python, &descriptor.toolchain.cc, &descriptor.toolchain.cxx] {
        if evidence.name.is_empty()
            || evidence.version.is_empty()
            || evidence.version.len() > 256
            || evidence.version.contains(['/', '\\'])
        {
            return Err(SeedError::Descriptor);
        }
    }
    for (path, mode) in [
        ("payload.json", 0o644),
        (MANIFEST_PATH, 0o644),
        (CLI_PATH, 0o644),
        (current_node_path(), 0o755),
        (DURINDOOR_LICENSE_PATH, 0o644),
        (NODE_LICENSE_PATH, 0o644),
        (NOTICES_PATH, 0o644),
    ] {
        let file = open_cap_file(payload, path).map_err(|_| SeedError::Descriptor)?;
        validate_file_security(&file, mode, SeedError::Descriptor)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn validate_manifest(manifest: &SeedManifest, descriptor: &ManifestDescriptor) -> Result<(), SeedError> {
    if manifest.schema_version != 1
        || manifest.files.is_empty()
        || manifest.files.len() != descriptor.file_count
        || manifest.files.len() > MAX_FILES
    {
        return Err(SeedError::Manifest);
    }
    let mut prior: Option<String> = None;
    let mut total = 0_u64;
    for file in &manifest.files {
        let key = path_key(&file.path).map_err(|_| SeedError::Manifest)?;
        if prior.as_ref().is_some_and(|previous| previous >= &key) {
            return Err(SeedError::Manifest);
        }
        prior = Some(key);
        validate_digest(&file.sha256).map_err(|_| SeedError::Manifest)?;
        total = total.checked_add(file.size).ok_or(SeedError::Manifest)?;
        if total > MAX_TOTAL_BYTES {
            return Err(SeedError::Manifest);
        }
    }
    if total != descriptor.total_bytes {
        return Err(SeedError::Manifest);
    }
    let paths = manifest.files.iter().map(|file| path_key(&file.path)).collect::<Result<HashSet<_>, _>>()?;
    for path in [BETTER_PACKAGE, BETTER_ENTRY, BETTER_BINARY, SQL_PACKAGE, SQL_ENTRY, SQL_WASM] {
        if !paths.contains(&path_key(path)?) {
            return Err(SeedError::Manifest);
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn validate_source_set(source: &Dir, files: &[SeedFile]) -> Result<(), SeedError> {
    let mut actual = Vec::new();
    walk_source(source, Path::new(""), &mut actual)?;
    actual.sort();
    let mut expected = files.iter().map(|file| path_key(&file.path)).collect::<Result<Vec<_>, _>>()?;
    expected.sort();
    if actual != expected {
        return Err(SeedError::Manifest);
    }
    for file in files {
        validate_exact_file(source, file)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn walk_source(directory: &Dir, prefix: &Path, files: &mut Vec<String>) -> Result<(), SeedError> {
    for entry in directory.entries().map_err(|_| SeedError::Manifest)? {
        let entry = entry.map_err(|_| SeedError::Manifest)?;
        let name = entry.file_name();
        let name = name.to_str().ok_or(SeedError::Manifest)?;
        let relative = prefix.join(name);
        let metadata = directory.symlink_metadata(name).map_err(|_| SeedError::Manifest)?;
        if metadata.is_symlink() {
            return Err(SeedError::Manifest);
        }
        if metadata.is_dir() {
            let child = directory.open_dir_nofollow(name).map_err(|_| SeedError::Manifest)?;
            validate_directory_security(&child, SeedError::Manifest)?;
            walk_source(&child, &relative, files)?;
        } else if metadata.is_file() {
            files.push(path_key(relative.to_str().ok_or(SeedError::Manifest)?)?);
        } else {
            return Err(SeedError::Manifest);
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn validate_package(source: &Dir, path: &str, name: &str, version: &str) -> Result<(), SeedError> {
    #[derive(Deserialize)]
    struct Package<'a> {
        name: &'a str,
        version: &'a str,
    }
    let bytes = read_cap_file(source, path, 1024 * 1024).map_err(|_| SeedError::Manifest)?;
    let package: Package<'_> = serde_json::from_slice(&bytes).map_err(|_| SeedError::Manifest)?;
    if package.name != name || package.version != version {
        return Err(SeedError::Manifest);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn validate_exact_file(root: &Dir, expected: &SeedFile) -> Result<(), SeedError> {
    let mut file = open_cap_file(root, &expected.path).map_err(|_| SeedError::Manifest)?;
    let mode = if expected.executable { 0o755 } else { 0o644 };
    validate_file_security(&file, mode, SeedError::Manifest)?;
    let before = file.metadata().map_err(|_| SeedError::Manifest)?;
    if before.len() != expected.size {
        return Err(SeedError::Manifest);
    }
    let actual = hash_reader(&mut file).map_err(|_| SeedError::Manifest)?;
    let after = file.metadata().map_err(|_| SeedError::Manifest)?;
    if identity(&before) != identity(&after)
        || before.len() != after.len()
        || actual != expected.sha256
    {
        return Err(SeedError::Manifest);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn revalidate_seed(validated: &ValidatedSeed) -> Result<(), SeedError> {
    validate_source_set(&validated.source, &validated.files)?;
    for critical in &validated.critical {
        let current = open_cap_file(&validated.source, critical.path).map_err(|_| SeedError::Manifest)?;
        let current_metadata = current.metadata().map_err(|_| SeedError::Manifest)?;
        let retained_metadata = critical.file.metadata().map_err(|_| SeedError::Manifest)?;
        if identity(&current_metadata) != critical.identity || identity(&retained_metadata) != critical.identity {
            return Err(SeedError::Manifest);
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn identity(metadata: &cap_std::fs::Metadata) -> Identity {
    Identity(cap_fs_ext::MetadataExt::dev(metadata), cap_fs_ext::MetadataExt::ino(metadata))
}

#[cfg(target_os = "linux")]
fn validate_file_security(file: &cap_std::fs::File, mode: u32, failure: SeedError) -> Result<(), SeedError> {
    let metadata = file.metadata().map_err(|_| failure)?;
    if !metadata.is_file()
        || cap_fs_ext::MetadataExt::nlink(&metadata) != 1
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o7777 != mode
    {
        return Err(failure);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn validate_directory_security(directory: &Dir, failure: SeedError) -> Result<(), SeedError> {
    let metadata = directory.dir_metadata().map_err(|_| failure)?;
    if metadata.uid() != unsafe { libc::geteuid() } || metadata.permissions().mode() & 0o022 != 0 {
        return Err(failure);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn probe_context(node: cap_std::fs::File, source: Dir) -> ValidatedSeed {
    ValidatedSeed { source, node, files: Vec::new(), critical: Vec::new() }
}

#[cfg(target_os = "linux")]
fn run_probe_v1(validated: &ValidatedSeed, script: &str) -> Result<(), SeedError> {
    let node = inherited_file_path(&validated.node)?;
    let root = inherited_dir_path(&validated.source)?;
    let (mut reader, writer) = os_pipe::pipe().map_err(|_| SeedError::Io)?;
    set_nonblocking(reader.as_raw_fd())?;
    let stderr = writer.try_clone().map_err(|_| SeedError::Io)?;
    let mut command = Command::new(&node.path);
    command
        .arg("-e")
        .arg(script)
        .arg(&root.path)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::from(writer))
        .stderr(Stdio::from(stderr))
        .current_dir(&root.path);
    let mut tree = ProcessTree::spawn(&mut command).map_err(|_| SeedError::ProbeFailed)?;
    drop(command);
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let mut bytes = Vec::new();
    let outcome = loop {
        match tree.try_wait_direct() {
            Ok(Some(status)) => break classify_direct(status),
            Ok(None) if Instant::now() >= deadline => break Err(SeedError::ProbeTimeout),
            Ok(None) => {}
            Err(_) => break Err(SeedError::ProbeFailed),
        }
        match drain_probe_output(&mut reader, &mut bytes) {
            // EOF does not prove whole-tree exit; only ProcessTree teardown owns that decision.
            Ok(_) => {}
            Err(error) => break Err(error),
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let stop = tree.stop(PROBE_STOP_GRACE).map_err(|_| SeedError::Cleanup);
    if stop.is_err() {
        return Err(SeedError::Cleanup);
    }
    let drained = drain_probe_output(&mut reader, &mut bytes);
    let status = outcome?;
    let eof = drained?;
    if !status.success() {
        return Err(SeedError::ProbeFailed);
    }
    if !eof || bytes != PROBE_OK {
        return Err(SeedError::ProbeFailed);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn classify_direct(status: ExitStatus) -> Result<ExitStatus, SeedError> {
    if status.success() { Ok(status) } else { Err(SeedError::ProbeFailed) }
}

#[cfg(target_os = "linux")]
fn drain_probe_output(reader: &mut os_pipe::PipeReader, bytes: &mut Vec<u8>) -> Result<bool, SeedError> {
    let mut buffer = [0_u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(true),
            Ok(read) => {
                if bytes.len().checked_add(read).is_none_or(|length| length > PROBE_OUTPUT_BYTES) {
                    return Err(SeedError::ProbeOutputLimit);
                }
                bytes.extend_from_slice(&buffer[..read]);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(SeedError::ProbeFailed),
        }
    }
}

#[cfg(target_os = "linux")]
fn set_nonblocking(descriptor: libc::c_int) -> Result<(), SeedError> {
    // SAFETY: descriptor is a live pipe owned by this function.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(SeedError::Io);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
struct InheritedPath {
    path: PathBuf,
    _descriptor: OwnedFd,
}

#[cfg(target_os = "linux")]
fn inherited_file_path(file: &cap_std::fs::File) -> Result<InheritedPath, SeedError> {
    inherited_path(file.as_raw_fd())
}

#[cfg(target_os = "linux")]
fn inherited_dir_path(directory: &Dir) -> Result<InheritedPath, SeedError> {
    inherited_path(directory.as_raw_fd())
}

#[cfg(target_os = "linux")]
fn inherited_path(source: libc::c_int) -> Result<InheritedPath, SeedError> {
    // SAFETY: F_DUPFD duplicates the retained descriptor and returns new ownership on success.
    let descriptor = unsafe { libc::fcntl(source, libc::F_DUPFD, 3) };
    if descriptor < 0 {
        return Err(SeedError::ProbeFailed);
    }
    // SAFETY: successful F_DUPFD returns a unique descriptor.
    let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
    Ok(InheritedPath { path: PathBuf::from(format!("/proc/self/fd/{}", descriptor.as_raw_fd())), _descriptor: descriptor })
}

#[cfg(target_os = "linux")]
fn open_private_root(path: &Path) -> Result<Dir, SeedError> {
    if !path.is_absolute() {
        return Err(SeedError::UnsafePath);
    }
    let mut directory = Dir::open_ambient_dir("/", ambient_authority()).map_err(|_| SeedError::UnsafePath)?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                directory = directory.open_dir_nofollow(name).map_err(|_| SeedError::UnsafePath)?;
            }
            _ => return Err(SeedError::UnsafePath),
        }
    }
    let metadata = directory.dir_metadata().map_err(|_| SeedError::UnsafePath)?;
    if metadata.uid() != unsafe { libc::geteuid() } || metadata.permissions().mode() & 0o077 != 0 {
        return Err(SeedError::UnsafePath);
    }
    Ok(directory)
}

#[cfg(target_os = "linux")]
fn read_cap_file(root: &Dir, path: &str, limit: u64) -> Result<Vec<u8>, SeedError> {
    let file = open_cap_file(root, path)?;
    let length = file.metadata().map_err(|_| SeedError::Io)?.len();
    if length > limit {
        return Err(SeedError::Manifest);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(length).map_err(|_| SeedError::Manifest)?);
    file.take(limit + 1).read_to_end(&mut bytes).map_err(|_| SeedError::Io)?;
    if bytes.len() as u64 > limit {
        return Err(SeedError::Manifest);
    }
    Ok(bytes)
}

#[cfg(target_os = "linux")]
fn open_cap_file(root: &Dir, path: &str) -> Result<cap_std::fs::File, SeedError> {
    let components = safe_relative(path)?;
    let (parent, name) = traverse_parent(root, &components)?;
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let file = parent.open_with(name, &options).map_err(|_| SeedError::Io)?;
    if !file.metadata().map_err(|_| SeedError::Io)?.is_file() {
        return Err(SeedError::UnsafePath);
    }
    Ok(file)
}

#[cfg(target_os = "linux")]
fn open_cap_directory(root: &Dir, path: &str) -> Result<Dir, SeedError> {
    let components = safe_relative(path)?;
    let mut current = root.try_clone().map_err(|_| SeedError::Io)?;
    for component in components {
        current = current.open_dir_nofollow(component).map_err(|_| SeedError::UnsafePath)?;
    }
    Ok(current)
}

#[cfg(target_os = "linux")]
fn traverse_parent(root: &Dir, components: &[String]) -> Result<(Dir, String), SeedError> {
    let mut current = root.try_clone().map_err(|_| SeedError::Io)?;
    for component in &components[..components.len().saturating_sub(1)] {
        current = current.open_dir_nofollow(component).map_err(|_| SeedError::UnsafePath)?;
    }
    Ok((current, components.last().ok_or(SeedError::UnsafePath)?.clone()))
}

#[cfg(target_os = "linux")]
fn safe_relative(path: &str) -> Result<Vec<String>, SeedError> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains(['\\', ':'])
        || path.bytes().any(|byte| byte <= 0x1f || byte == 0x7f)
    {
        return Err(SeedError::UnsafePath);
    }
    let mut components = Vec::new();
    for component in path.split('/') {
        let normalized: String = component.nfc().collect();
        if component.is_empty()
            || component == "."
            || component == ".."
            || component.as_bytes().len() > MAX_COMPONENT_BYTES
            || component.trim_end() != component
            || component.ends_with('.')
            || normalized != component
        {
            return Err(SeedError::UnsafePath);
        }
        let key = normalized.to_lowercase();
        let stem = key.split('.').next().unwrap_or_default();
        let reserved = matches!(stem, "con" | "prn" | "aux" | "nul")
            || stem.strip_prefix("com").or_else(|| stem.strip_prefix("lpt")).is_some_and(|number| {
                matches!(number, "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³")
            });
        if reserved || key.starts_with(".melon-runtime-seed-") {
            return Err(SeedError::UnsafePath);
        }
        components.push(component.to_owned());
    }
    Ok(components)
}

#[cfg(target_os = "linux")]
fn path_key(path: &str) -> Result<String, SeedError> {
    Ok(safe_relative(path)?.into_iter().map(|component| component.to_lowercase()).collect::<Vec<_>>().join("/"))
}

#[cfg(target_os = "linux")]
fn validate_digest(digest: &str) -> Result<(), SeedError> {
    if digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) {
        Ok(())
    } else {
        Err(SeedError::Manifest)
    }
}

#[cfg(target_os = "linux")]
fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(target_os = "linux")]
fn hash_reader(reader: &mut impl Read) -> io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = [0; COPY_BUFFER_BYTES];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(target_os = "linux")]
fn current_target() -> &'static str {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    { "x86_64-unknown-linux-gnu" }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    { "x86_64-apple-darwin" }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    { "aarch64-apple-darwin" }
    #[cfg(all(windows, target_arch = "x86_64"))]
    { "x86_64-pc-windows-msvc" }
    #[cfg(not(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(windows, target_arch = "x86_64")
    )))]
    { "unsupported" }
}

#[cfg(target_os = "linux")]
fn current_node_path() -> &'static str {
    if cfg!(windows) { NODE_PATH_WINDOWS } else { NODE_PATH_UNIX }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use serde_json::json;
    use std::fs;
    #[cfg(target_os = "linux")]
    use std::os::unix::fs::PermissionsExt as _;
    #[cfg(target_os = "linux")]
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "melon-runtime-seed-test-{}-{}",
                std::process::id(),
                getrandom::u64().expect("random temp name")
            ));
            fs::create_dir(&path).expect("create temp directory");
            #[cfg(target_os = "linux")]
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).expect("secure temp directory");
            Self(path)
        }

        fn child(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            fs::create_dir(&path).expect("create child directory");
            #[cfg(target_os = "linux")]
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).expect("secure child directory");
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(target_os = "linux")]
    struct Fixture {
        _temp: TempDir,
        payload: PathBuf,
        destination: PathBuf,
    }

    #[cfg(target_os = "linux")]
    impl Fixture {
        fn new(node_body: &str) -> Self {
            let temp = TempDir::new();
            let payload = temp.child("payload");
            let destination = temp.child("destination");
            for directory in [
                "runtime-seed/node_modules/better-sqlite3/build/Release",
                "runtime-seed/node_modules/better-sqlite3/lib",
                "runtime-seed/node_modules/sql.js/dist",
                "metadata",
                "bin",
                "app/node_modules/durindoor",
                "licenses",
            ] {
                fs::create_dir_all(payload.join(directory)).expect("fixture directory");
            }
            let writes: [(&str, &[u8]); 12] = [
                ("runtime-seed/node_modules/better-sqlite3/package.json", br#"{"name":"better-sqlite3","version":"12.6.2"}"#),
                ("runtime-seed/node_modules/better-sqlite3/lib/index.js", b"module.exports = {}"),
                ("runtime-seed/node_modules/better-sqlite3/build/Release/better_sqlite3.node", b"native"),
                ("runtime-seed/node_modules/sql.js/package.json", br#"{"name":"sql.js","version":"1.14.1"}"#),
                ("runtime-seed/node_modules/sql.js/dist/sql-wasm.js", b"module.exports = {}"),
                ("runtime-seed/node_modules/sql.js/dist/sql-wasm.wasm", b"\0asm"),
                (CLI_PATH, b"cli"),
                (DURINDOOR_LICENSE_PATH, b"durindoor license"),
                (NODE_LICENSE_PATH, b"node license"),
                (NOTICES_PATH, b"notices"),
                (current_node_path(), node_body.as_bytes()),
                ("placeholder", b"placeholder"),
            ];
            for (path, bytes) in writes {
                let target = payload.join(path);
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).expect("write parent");
                }
                fs::write(&target, bytes).expect("write fixture file");
                let mode = if path == current_node_path() { 0o755 } else { 0o644 };
                fs::set_permissions(&target, fs::Permissions::from_mode(mode)).expect("fixture mode");
            }
            let fixture = Self { _temp: temp, payload, destination };
            fixture.rebuild_authority();
            fixture
        }

        fn source(&self) -> PathBuf {
            self.payload.join(RUNTIME_SEED_PATH)
        }

        fn rebuild_authority(&self) {
            let mut files = Vec::new();
            collect_manifest_files(&self.source(), &self.source(), &mut files);
            files.sort_by(|left, right| {
                path_key(left["path"].as_str().expect("left path")).expect("left key")
                    .cmp(&path_key(right["path"].as_str().expect("right path")).expect("right key"))
            });
            let manifest = format!("{}\n", serde_json::to_string_pretty(&json!({ "schemaVersion": 1, "files": files })).expect("manifest JSON"));
            fs::write(self.payload.join(MANIFEST_PATH), &manifest).expect("write manifest");
            fs::set_permissions(self.payload.join(MANIFEST_PATH), fs::Permissions::from_mode(0o644)).expect("manifest mode");
            let count = files.len();
            let total: u64 = files.iter().map(|file| file["size"].as_u64().expect("size")).sum();
            let descriptor = json!({
                "schemaVersion": 1,
                "target": current_target(),
                "durindoorVersion": DURINDOOR_VERSION,
                "nodeVersion": NODE_VERSION,
                "nodeAbi": NODE_ABI,
                "toolchain": {
                    "python": { "name": "python", "version": "Python 3", "sha256": digest(b"python") },
                    "cc": { "name": "cc", "version": "cc 1", "sha256": digest(b"cc") },
                    "cxx": { "name": "cxx", "version": "cxx 1", "sha256": digest(b"cxx") }
                },
                "cli": CLI_PATH,
                "node": current_node_path(),
                "runtimeSeedPath": RUNTIME_SEED_PATH,
                "runtimeSeedManifest": {
                    "path": MANIFEST_PATH,
                    "sha256": digest(manifest.as_bytes()),
                    "fileCount": count,
                    "totalBytes": total,
                    "destination": "data-runtime-root",
                    "probeVersion": 1,
                    "modules": {
                        "betterSqlite3": { "packagePath": BETTER_PACKAGE, "binaryPath": BETTER_BINARY, "version": BETTER_SQLITE_VERSION },
                        "sqlJs": { "packagePath": SQL_PACKAGE, "wasmPath": SQL_WASM, "version": SQL_JS_VERSION }
                    }
                },
                "managedLaunchReady": false,
                "licenses": {
                    "durindoor": DURINDOOR_LICENSE_PATH,
                    "node": NODE_LICENSE_PATH,
                    "notices": NOTICES_PATH,
                    "sha256": { "durindoor": digest(b"durindoor license"), "node": digest(b"node license"), "notices": digest(b"notices") }
                }
            });
            fs::write(self.payload.join("payload.json"), format!("{}\n", serde_json::to_string_pretty(&descriptor).expect("descriptor JSON"))).expect("write descriptor");
            fs::set_permissions(self.payload.join("payload.json"), fs::Permissions::from_mode(0o644)).expect("descriptor mode");
        }
    }

    #[cfg(target_os = "linux")]
    fn collect_manifest_files(root: &Path, directory: &Path, files: &mut Vec<serde_json::Value>) {
        for entry in fs::read_dir(directory).expect("read seed directory") {
            let entry = entry.expect("seed entry");
            let metadata = entry.metadata().expect("seed metadata");
            if metadata.is_dir() {
                collect_manifest_files(root, &entry.path(), files);
            } else {
                let path = entry.path().strip_prefix(root).expect("relative seed path").to_string_lossy().replace('\\', "/");
                let bytes = fs::read(entry.path()).expect("seed bytes");
                let executable = metadata.permissions().mode() & 0o111 != 0;
                files.push(json!({ "path": path, "size": bytes.len(), "sha256": digest(&bytes), "executable": executable }));
            }
        }
    }

    fn digest(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    #[test]
    fn install_fails_closed_before_destination_mutation() {
        let temp = TempDir::new();
        let payload = temp.child("payload");
        let destination = temp.child("destination");
        let error = install_runtime_seed(&payload, &destination).expect_err("installation remains blocked");
        let expected = if cfg!(target_os = "linux") { SeedError::NativeProbeRequired } else { SeedError::UnsupportedPlatform };
        assert_eq!(error, expected);
        assert!(fs::read_dir(destination).expect("destination entries").next().is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn exact_descriptor_paths_and_abi_are_required() {
        for (pointer, replacement) in [
            ("/nodeAbi", json!("116")),
            ("/runtimeSeedPath", json!("other")),
            ("/runtimeSeedManifest/path", json!("other.json")),
            ("/runtimeSeedManifest/modules/betterSqlite3/packagePath", json!("node_modules/other/package.json")),
            ("/runtimeSeedManifest/modules/sqlJs/wasmPath", json!("node_modules/other.wasm")),
            ("/cli", json!("other.js")),
            ("/licenses/node", json!("other-license")),
        ] {
            let fixture = Fixture::new("#!/bin/sh\nexit 1\n");
            let path = fixture.payload.join("payload.json");
            let mut descriptor: serde_json::Value = serde_json::from_slice(&fs::read(&path).expect("descriptor")).expect("JSON");
            *descriptor.pointer_mut(pointer).expect("descriptor pointer") = replacement;
            fs::write(&path, serde_json::to_vec(&descriptor).expect("descriptor bytes")).expect("mutate descriptor");
            assert!(matches!(validate_seed(&fixture.payload), Err(SeedError::Descriptor)), "{pointer}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn source_hardlinks_and_unsafe_modes_are_rejected() {
        let fixture = Fixture::new("#!/bin/sh\nexit 1\n");
        let package = fixture.source().join(BETTER_PACKAGE);
        fs::hard_link(&package, fixture.source().join("alias")).expect("hardlink alias");
        fixture.rebuild_authority();
        assert!(matches!(validate_seed(&fixture.payload), Err(SeedError::Manifest)));

        let fixture = Fixture::new("#!/bin/sh\nexit 1\n");
        fs::set_permissions(fixture.source().join(SQL_WASM), fs::Permissions::from_mode(0o666)).expect("unsafe mode");
        fixture.rebuild_authority();
        assert!(matches!(validate_seed(&fixture.payload), Err(SeedError::Manifest)));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn ancestor_symlinks_are_rejected() {
        let fixture = Fixture::new("#!/bin/sh\nexit 1\n");
        let link = fixture._temp.0.join("payload-link");
        symlink(&fixture.payload, &link).expect("payload link");
        assert!(matches!(validate_seed(&link), Err(SeedError::UnsafePath)));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn retained_capabilities_survive_payload_ancestor_replacement() {
        let fixture = Fixture::new("#!/bin/sh\nexit 1\n");
        let payload = open_private_root(&fixture.payload).expect("payload");
        let source = open_cap_directory(&payload, RUNTIME_SEED_PATH).expect("source");
        let node = open_cap_file(&payload, current_node_path()).expect("node");
        let validated = probe_context(node, source);
        fs::write(fixture.payload.join(current_node_path()), "#!/bin/sh\nprintf 'melon-runtime-seed-probe-v1-ok\\n'\n").expect("success node");
        let moved = fixture._temp.0.join("payload-moved");
        fs::rename(&fixture.payload, &moved).expect("move payload namespace");
        fs::create_dir(&fixture.payload).expect("replace payload namespace");

        let retained = inherited_dir_path(&validated.source).expect("retained source path");
        assert_eq!(fs::read(retained.path.join(SQL_WASM)).expect("read retained WASM"), b"\0asm");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn critical_path_replacement_is_detected_after_probe() {
        let fixture = Fixture::new("#!/bin/sh\nexit 1\n");
        let payload = open_private_root(&fixture.payload).expect("payload");
        let source = open_cap_directory(&payload, RUNTIME_SEED_PATH).expect("source");
        let node = open_cap_file(&payload, current_node_path()).expect("node");
        let mut validated = probe_context(node, source);
        fs::write(fixture.payload.join(current_node_path()), "#!/bin/sh\nprintf 'melon-runtime-seed-probe-v1-ok\\n'\n").expect("success node");
        validated.critical = [BETTER_PACKAGE, BETTER_ENTRY, BETTER_BINARY, SQL_PACKAGE, SQL_ENTRY, SQL_WASM]
            .into_iter()
            .map(|path| {
                let file = open_cap_file(&validated.source, path).expect("critical file");
                CriticalFile { path, identity: identity(&file.metadata().expect("critical metadata")), file }
            })
            .collect();
        let wasm = fixture.source().join(SQL_WASM);
        fs::rename(&wasm, wasm.with_extension("validated")).expect("move validated WASM");
        fs::write(&wasm, b"replacement").expect("replace WASM path");
        fs::set_permissions(&wasm, fs::Permissions::from_mode(0o644)).expect("replacement mode");

        run_probe_v1(&validated, PROBE_V1).expect("probe uses retained root");
        assert_eq!(revalidate_seed(&validated).expect_err("replacement rejected"), SeedError::Manifest);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn nonzero_direct_exit_with_descendant_is_bounded_and_absent() {
        let fixture = Fixture::new("#!/bin/sh\nexit 1\n");
        let source = open_cap_directory(&open_private_root(&fixture.payload).expect("payload"), RUNTIME_SEED_PATH).expect("source");
        let marker = fixture._temp.0.join("descendant.pid");
        let node_path = fixture.payload.join(current_node_path());
        fs::write(&node_path, format!("#!/bin/sh\nsleep 10 &\necho $! > '{}'\nexit 7\n", marker.display())).expect("failure node");
        let node = cap_std::fs::File::from_std(fs::File::open(node_path).expect("node handle"));
        let validated = probe_context(node, source);
        let started = Instant::now();

        assert_eq!(run_probe_v1(&validated, PROBE_V1).expect_err("nonzero probe"), SeedError::ProbeFailed);
        assert!(started.elapsed() < Duration::from_secs(5));
        let pid: libc::pid_t = fs::read_to_string(marker).expect("descendant pid").trim().parse().expect("PID");
        let deadline = Instant::now() + Duration::from_secs(1);
        while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1, "owned descendant remains absent");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn direct_child_timeout_is_bounded() {
        let fixture = Fixture::new("#!/bin/sh\nexit 1\n");
        let source = open_cap_directory(&open_private_root(&fixture.payload).expect("payload"), RUNTIME_SEED_PATH).expect("source");
        let node_path = fixture.payload.join(current_node_path());
        fs::write(&node_path, "#!/bin/sh\nsleep 10\n").expect("timeout node");
        let node = cap_std::fs::File::from_std(fs::File::open(node_path).expect("node handle"));
        let validated = probe_context(node, source);
        let started = Instant::now();

        assert_eq!(run_probe_v1(&validated, PROBE_V1).expect_err("timeout probe"), SeedError::ProbeTimeout);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn output_capture_is_bounded_without_reader_threads() {
        let fixture = Fixture::new("#!/bin/sh\nexit 1\n");
        let source = open_cap_directory(&open_private_root(&fixture.payload).expect("payload"), RUNTIME_SEED_PATH).expect("source");
        let node_path = fixture.payload.join(current_node_path());
        fs::write(&node_path, "#!/bin/sh\nwhile :; do printf x; done\n").expect("output node");
        let node = cap_std::fs::File::from_std(fs::File::open(node_path).expect("node handle"));
        let validated = probe_context(node, source);
        let started = Instant::now();

        assert_eq!(run_probe_v1(&validated, PROBE_V1).expect_err("output cap"), SeedError::ProbeOutputLimit);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn inherited_writer_cannot_block_probe_completion() {
        let fixture = Fixture::new("#!/bin/sh\nexit 1\n");
        let source = open_cap_directory(&open_private_root(&fixture.payload).expect("payload"), RUNTIME_SEED_PATH).expect("source");
        let node_path = fixture.payload.join(current_node_path());
        fs::write(&node_path, "#!/bin/sh\n(sleep 10) &\nprintf 'melon-runtime-seed-probe-v1-ok\\n'\n").expect("writer node");
        let node = cap_std::fs::File::from_std(fs::File::open(node_path).expect("node handle"));
        let validated = probe_context(node, source);
        let started = Instant::now();

        run_probe_v1(&validated, PROBE_V1).expect("tree stop closes inherited writer");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires MELON_NATIVE_PAYLOAD_ROOT on a target-native release runner"]
    fn release_gate_exercises_real_native_modules_when_payload_is_supplied() {
        let payload = std::env::var_os("MELON_NATIVE_PAYLOAD_ROOT").expect("MELON_NATIVE_PAYLOAD_ROOT");
        let validated = validate_seed(Path::new(&payload)).expect("validate native payload");
        run_probe_v1(&validated, PROBE_V1).expect("real native probe");

        let native_mutation = PROBE_V1.replacen("!== 1", "=== 1", 1);
        assert_eq!(run_probe_v1(&validated, &native_mutation).expect_err("native query mutation must fail"), SeedError::ProbeFailed);
        let (prefix, suffix) = PROBE_V1.rsplit_once("!== 1").expect("WASM predicate");
        let wasm_mutation = format!("{prefix}=== 1{suffix}");
        assert_eq!(run_probe_v1(&validated, &wasm_mutation).expect_err("WASM query mutation must fail"), SeedError::ProbeFailed);
    }
}
