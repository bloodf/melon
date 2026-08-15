//! Verified, non-destructive installation of target-native runtime seed files.

use cap_fs_ext::{DirExt as _, FollowSymlinks, MetadataExt, OpenOptionsFollowExt};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
#[cfg(unix)]
use cap_std::fs::{Permissions, PermissionsExt};
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{LazyLock, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use unicode_normalization::UnicodeNormalization;

use crate::process_tree::ProcessTree;

const MAX_FILES: usize = 16_384;
const MAX_TOTAL_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_COMPONENT_BYTES: usize = 128;
const COPY_BUFFER_BYTES: usize = 64 * 1024;
const PROBE_OUTPUT_BYTES: u64 = 64 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const PROBE_STOP_GRACE: Duration = Duration::from_millis(100);
const DESTINATION_LOCK: &str = ".melon-runtime-seed-lock-v1";
const PROBE_OK: &str = "melon-runtime-seed-probe-v1-ok";
const DURINDOOR_VERSION: &str = "3.15.2";
const NODE_VERSION: &str = "20.20.2";
const BETTER_SQLITE_VERSION: &str = "12.6.2";
const SQL_JS_VERSION: &str = "1.14.1";
static THREAD_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

const PROBE_V1: &str = r#"
const path = require('node:path');
const [root, betterPackage, sqlPackage, wasmPath] = process.argv.slice(1);
try {
  const Database = require(path.dirname(path.join(root, betterPackage)));
  const native = new Database(':memory:');
  native.exec('CREATE TABLE melon_probe(value INTEGER); INSERT INTO melon_probe VALUES (1)');
  if (native.prepare('SELECT value FROM melon_probe').pluck().get() !== 1) throw new Error('native probe mismatch');
  native.close();
  const initSqlJs = require(path.dirname(path.join(root, sqlPackage)));
  Promise.resolve(initSqlJs({ locateFile: () => path.join(root, wasmPath) })).then(SQL => {
    const wasm = new SQL.Database();
    wasm.run('CREATE TABLE melon_probe(value INTEGER); INSERT INTO melon_probe VALUES (1)');
    if (wasm.exec('SELECT value FROM melon_probe')[0].values[0][0] !== 1) throw new Error('wasm probe mismatch');
    wasm.close();
    process.stdout.write('melon-runtime-seed-probe-v1-ok\n');
  }).catch(() => process.exit(1));
} catch (_) { process.exit(1); }
"#;

/// Redacted runtime-seed installation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SeedError {
    Descriptor,
    Manifest,
    UnsafePath,
    Conflict,
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
            Self::Conflict => "runtime seed conflicts with an existing destination object",
            Self::ProbeFailed => "runtime seed probe failed",
            Self::ProbeTimeout => "runtime seed probe timed out",
            Self::ProbeOutputLimit => "runtime seed probe exceeded its output limit",
            Self::Cleanup => "runtime seed rollback could not safely remove every created object",
            Self::Io => "runtime seed I/O failed",
        })
    }
}

impl std::error::Error for SeedError {}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct SeedInstallOutcome {
    pub(crate) created_files: usize,
    pub(crate) reused_files: usize,
}

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

struct CreatedObject {
    relative: PathBuf,
    identity: Identity,
    directory: bool,
}

struct ValidatedSeed {
    payload_root: PathBuf,
    source: Dir,
    descriptor: PayloadDescriptor,
    files: Vec<SeedFile>,
    node: cap_std::fs::File,
}

/// Merges a validated seed beneath `destination`, probes both native modules, and rolls back only
/// objects created by this call when any later step fails.
pub(crate) fn install_runtime_seed(
    payload_root: &Path,
    destination: &Path,
) -> Result<SeedInstallOutcome, SeedError> {
    install_runtime_seed_with_probe(payload_root, destination, |context| run_probe_v1(context))
}

fn install_runtime_seed_with_probe<F>(
    payload_root: &Path,
    destination: &Path,
    probe: F,
) -> Result<SeedInstallOutcome, SeedError>
where
    F: FnOnce(&ProbeContext<'_>) -> Result<(), SeedError>,
{
    let _thread_lock = THREAD_LOCK.lock().map_err(|_| SeedError::Io)?;
    let validated = validate_seed(payload_root)?;
    let destination_dir = open_private_root(destination)?;
    if roots_overlap(payload_root, destination)? {
        return Err(SeedError::UnsafePath);
    }
    let _process_lock = DestinationLock::acquire(&destination_dir)?;
    let mut created = Vec::new();
    let mut reused = 0;
    let result = merge_files(&validated, &destination_dir, &mut created, &mut reused).and_then(|created_files| {
        let context = ProbeContext { validated: &validated, destination };
        probe(&context)?;
        Ok(SeedInstallOutcome { created_files, reused_files: reused })
    });
    if result.is_err() {
        if rollback(&destination_dir, &created).is_err() {
            return Err(SeedError::Cleanup);
        }
    }
    result
}

struct DestinationLock(fs::File);

impl DestinationLock {
    fn acquire(destination: &Dir) -> Result<Self, SeedError> {
        let mut created = false;
        let file = {
            let mut options = OpenOptions::new();
            options.read(true).write(true).create_new(true).follow(FollowSymlinks::No);
            match destination.open_with(DESTINATION_LOCK, &options) {
                Ok(file) => {
                    created = true;
                    file
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    let mut options = OpenOptions::new();
                    options.read(true).write(true).follow(FollowSymlinks::No);
                    destination.open_with(DESTINATION_LOCK, &options).map_err(|_| SeedError::UnsafePath)?
                }
                Err(_) => return Err(SeedError::Io),
            }
        };
        if !file.metadata().map_err(|_| SeedError::Io)?.is_file() {
            return Err(SeedError::UnsafePath);
        }
        #[cfg(unix)]
        if created {
            file.set_permissions(Permissions::from_mode(0o600)).map_err(|_| SeedError::Io)?;
        }
        #[cfg(unix)]
        {
            let metadata = file.metadata().map_err(|_| SeedError::Io)?;
            if cap_std::fs::MetadataExt::uid(&metadata) != unsafe { libc::geteuid() } || metadata.permissions().mode() & 0o177 != 0 {
                return Err(SeedError::UnsafePath);
            }
        }
        let file = file.into_std();
        file.lock().map_err(|_| SeedError::Io)?;
        Ok(Self(file))
    }
}

impl Drop for DestinationLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn validate_seed(payload_root: &Path) -> Result<ValidatedSeed, SeedError> {
    let payload = open_private_root(payload_root)?;
    let descriptor_bytes = read_cap_file(&payload, "payload.json", MAX_TOTAL_BYTES)?;
    let descriptor: PayloadDescriptor = serde_json::from_slice(&descriptor_bytes).map_err(|_| SeedError::Descriptor)?;
    validate_descriptor(&payload, &descriptor)?;
    let manifest_bytes = read_cap_file(&payload, &descriptor.runtime_seed_manifest.path, MAX_TOTAL_BYTES)?;
    if sha256(&manifest_bytes) != descriptor.runtime_seed_manifest.sha256 {
        return Err(SeedError::Manifest);
    }
    let manifest: SeedManifest = serde_json::from_slice(&manifest_bytes).map_err(|_| SeedError::Manifest)?;
    validate_manifest(&manifest, &descriptor.runtime_seed_manifest)?;
    let node = open_cap_file(&payload, &descriptor.node).map_err(|_| SeedError::Descriptor)?;
    let source = open_cap_directory(&payload, &descriptor.runtime_seed_path)?;
    validate_source_set(&source, &manifest.files)?;
    validate_package(&source, &descriptor.runtime_seed_manifest.modules.better_sqlite3.package_path, "better-sqlite3", BETTER_SQLITE_VERSION)?;
    validate_package(&source, &descriptor.runtime_seed_manifest.modules.sql_js.package_path, "sql.js", SQL_JS_VERSION)?;
    Ok(ValidatedSeed { payload_root: payload_root.to_owned(), source, descriptor, files: manifest.files, node })
}

fn validate_descriptor(payload: &Dir, descriptor: &PayloadDescriptor) -> Result<(), SeedError> {
    if descriptor.schema_version != 1
        || descriptor.durindoor_version != DURINDOOR_VERSION
        || descriptor.node_version != NODE_VERSION
        || descriptor.managed_launch_ready
        || descriptor.runtime_seed_manifest.destination != "data-runtime-root"
        || descriptor.runtime_seed_manifest.probe_version != 1
        || descriptor.runtime_seed_manifest.file_count == 0
        || descriptor.runtime_seed_manifest.file_count > MAX_FILES
        || descriptor.runtime_seed_manifest.total_bytes > MAX_TOTAL_BYTES
        || descriptor.runtime_seed_manifest.modules.better_sqlite3.version != BETTER_SQLITE_VERSION
        || descriptor.runtime_seed_manifest.modules.sql_js.version != SQL_JS_VERSION
        || descriptor.target != current_target()
        || descriptor.node != current_node_path()
        || descriptor.node_abi.is_empty()
        || !descriptor.node_abi.bytes().all(|byte| byte.is_ascii_digit())
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
    for path in [
        &descriptor.cli,
        &descriptor.node,
        &descriptor.runtime_seed_path,
        &descriptor.runtime_seed_manifest.path,
        &descriptor.licenses.durindoor,
        &descriptor.licenses.node,
        &descriptor.licenses.notices,
        &descriptor.runtime_seed_manifest.modules.better_sqlite3.package_path,
        &descriptor.runtime_seed_manifest.modules.better_sqlite3.binary_path,
        &descriptor.runtime_seed_manifest.modules.sql_js.package_path,
        &descriptor.runtime_seed_manifest.modules.sql_js.wasm_path,
    ] {
        safe_relative(path).map_err(|_| SeedError::Descriptor)?;
    }
    let mut module_keys = HashSet::new();
    for path in [
        &descriptor.runtime_seed_manifest.modules.better_sqlite3.package_path,
        &descriptor.runtime_seed_manifest.modules.better_sqlite3.binary_path,
        &descriptor.runtime_seed_manifest.modules.sql_js.package_path,
        &descriptor.runtime_seed_manifest.modules.sql_js.wasm_path,
    ] {
        if !module_keys.insert(path_key(path).map_err(|_| SeedError::Descriptor)?) {
            return Err(SeedError::Descriptor);
        }
    }
    for evidence in [&descriptor.toolchain.python, &descriptor.toolchain.cc, &descriptor.toolchain.cxx] {
        if evidence.name.is_empty() || evidence.version.is_empty() || evidence.version.len() > 256 || evidence.version.contains(['/', '\\']) {
            return Err(SeedError::Descriptor);
        }
    }
    for path in [&descriptor.node, &descriptor.cli] {
        open_cap_file(payload, path).map_err(|_| SeedError::Descriptor)?;
    }
    Ok(())
}
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

fn current_node_path() -> &'static str {
    if cfg!(windows) { "bin/node.exe" } else { "bin/node" }
}


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
    for path in [
        &descriptor.modules.better_sqlite3.package_path,
        &descriptor.modules.better_sqlite3.binary_path,
        &descriptor.modules.sql_js.package_path,
        &descriptor.modules.sql_js.wasm_path,
    ] {
        if !paths.contains(&path_key(path)?) {
            return Err(SeedError::Manifest);
        }
    }
    Ok(())
}

fn validate_source_set(source: &Dir, files: &[SeedFile]) -> Result<(), SeedError> {
    let mut actual = Vec::new();
    walk_source(source, Path::new(""), &mut actual)?;
    actual.sort();
    let mut expected = files.iter().map(|file| path_key(&file.path)).collect::<Result<Vec<_>, _>>().map_err(|_| SeedError::Manifest)?;
    expected.sort();
    if actual != expected {
        return Err(SeedError::Manifest);
    }
    for file in files {
        validate_exact_file(source, file, SeedError::Manifest)?;
    }
    Ok(())
}

fn walk_source(directory: &Dir, prefix: &Path, files: &mut Vec<String>) -> Result<(), SeedError> {
    let entries = directory.entries().map_err(|_| SeedError::Manifest)?;
    for entry in entries {
        let entry = entry.map_err(|_| SeedError::Manifest)?;
        let name = entry.file_name();
        let name = name.to_str().ok_or(SeedError::Manifest)?;
        let relative = prefix.join(name);
        let metadata = directory.symlink_metadata(name).map_err(|_| SeedError::Manifest)?;
        if metadata.is_symlink() {
            return Err(SeedError::Manifest);
        }
        if metadata.is_dir() {
            let child = open_child_dir(directory, name).map_err(|_| SeedError::Manifest)?;
            walk_source(&child, &relative, files)?;
        } else if metadata.is_file() {
            let path = relative.to_str().ok_or(SeedError::Manifest)?;
            files.push(path_key(path).map_err(|_| SeedError::Manifest)?);
        } else {
            return Err(SeedError::Manifest);
        }
    }
    Ok(())
}

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

fn merge_files(
    validated: &ValidatedSeed,
    destination: &Dir,
    created: &mut Vec<CreatedObject>,
    reused: &mut usize,
) -> Result<usize, SeedError> {
    let mut created_files = 0;
    for expected in &validated.files {
        let components = safe_relative(&expected.path)?;
        let (parent, parent_relative) = ensure_directories(destination, &components[..components.len() - 1], created)?;
        let name = components.last().ok_or(SeedError::Manifest)?;
        let relative = parent_relative.join(name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true).follow(FollowSymlinks::No);
        match parent.open_with(name, &options) {
            Ok(mut output) => {
                let identity = identity(&output.metadata().map_err(|_| SeedError::Io)?);
                created.push(CreatedObject { relative, identity, directory: false });
                let mut input = open_cap_file(&validated.source, &expected.path).map_err(|_| SeedError::Manifest)?;
                copy_exact(&mut input, &mut output, expected.size, &expected.sha256)?;
                #[cfg(unix)]
                output.set_permissions(Permissions::from_mode(if expected.executable { 0o755 } else { 0o644 })).map_err(|_| SeedError::Io)?;
                output.sync_all().map_err(|_| SeedError::Io)?;
                created_files += 1;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                validate_exact_file(destination, expected, SeedError::Conflict)?;
                *reused += 1;
            }
            Err(_) => return Err(SeedError::Io),
        }
    }
    Ok(created_files)
}

fn ensure_directories(
    root: &Dir,
    components: &[String],
    created: &mut Vec<CreatedObject>,
) -> Result<(Dir, PathBuf), SeedError> {
    let mut current = root.try_clone().map_err(|_| SeedError::Io)?;
    let mut relative = PathBuf::new();
    for component in components {
        relative.push(component);
        match current.create_dir(component) {
            Ok(()) => {
                let child = open_child_dir(&current, component).map_err(|_| SeedError::Io)?;
                #[cfg(unix)]
                child.set_permissions(".", Permissions::from_mode(0o700)).map_err(|_| SeedError::Io)?;
                let metadata = child.dir_metadata().map_err(|_| SeedError::Io)?;
                created.push(CreatedObject { relative: relative.clone(), identity: identity(&metadata), directory: true });
                current = child;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                current = open_child_dir(&current, component).map_err(|_| SeedError::Conflict)?;
            }
            Err(_) => return Err(SeedError::Io),
        }
    }
    Ok((current, relative))
}

fn validate_exact_file(root: &Dir, expected: &SeedFile, failure: SeedError) -> Result<(), SeedError> {
    let mut file = open_cap_file(root, &expected.path).map_err(|_| failure)?;
    let before = file.metadata().map_err(|_| failure)?;
    if !before.is_file() || before.len() != expected.size {
        return Err(failure);
    }
    #[cfg(unix)]
    if (before.permissions().mode() & 0o111 != 0) != expected.executable {
        return Err(failure);
    }
    let actual = hash_reader(&mut file).map_err(|_| failure)?;
    let after = file.metadata().map_err(|_| failure)?;
    if identity(&before) != identity(&after) || before.len() != after.len() || actual != expected.sha256 {
        return Err(failure);
    }
    Ok(())
}

fn copy_exact(input: &mut cap_std::fs::File, output: &mut cap_std::fs::File, size: u64, digest: &str) -> Result<(), SeedError> {
    let before = input.metadata().map_err(|_| SeedError::Manifest)?;
    let mut hasher = Sha256::new();
    let mut copied = 0_u64;
    let mut buffer = [0; COPY_BUFFER_BYTES];
    loop {
        let read = input.read(&mut buffer).map_err(|_| SeedError::Manifest)?;
        if read == 0 {
            break;
        }
        copied = copied.checked_add(read as u64).ok_or(SeedError::Manifest)?;
        if copied > size {
            return Err(SeedError::Manifest);
        }
        hasher.update(&buffer[..read]);
        output.write_all(&buffer[..read]).map_err(|_| SeedError::Io)?;
    }
    let after = input.metadata().map_err(|_| SeedError::Manifest)?;
    if copied != size || identity(&before) != identity(&after) || before.len() != after.len() || format!("{:x}", hasher.finalize()) != digest {
        return Err(SeedError::Manifest);
    }
    Ok(())
}

fn rollback(root: &Dir, created: &[CreatedObject]) -> Result<(), SeedError> {
    let mut clean = true;
    for object in created.iter().rev() {
        let result = if object.directory {
            remove_created_directory(root, object)
        } else {
            remove_created_file(root, object)
        };
        clean &= result.is_ok();
    }
    if clean { Ok(()) } else { Err(SeedError::Cleanup) }
}

fn remove_created_file(root: &Dir, object: &CreatedObject) -> Result<(), SeedError> {
    let (parent, name) = open_parent(root, &object.relative)?;
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let file = match parent.open_with(&name, &options) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(SeedError::Cleanup),
    };
    if identity(&file.metadata().map_err(|_| SeedError::Cleanup)?) != object.identity {
        return Err(SeedError::Cleanup);
    }
    parent.remove_file(name).map_err(|_| SeedError::Cleanup)
}

fn remove_created_directory(root: &Dir, object: &CreatedObject) -> Result<(), SeedError> {
    let (parent, name) = open_parent(root, &object.relative)?;
    let directory = match open_child_dir(&parent, name.to_str().ok_or(SeedError::Cleanup)?) {
        Ok(directory) => directory,
        Err(SeedError::Io) => return Ok(()),
        Err(_) => return Err(SeedError::Cleanup),
    };
    if identity(&directory.dir_metadata().map_err(|_| SeedError::Cleanup)?) != object.identity {
        return Err(SeedError::Cleanup);
    }
    parent.remove_dir(name).map_err(|_| SeedError::Cleanup)
}

struct ProbeContext<'a> {
    validated: &'a ValidatedSeed,
    destination: &'a Path,
}

/// Runs the fixed native-module probe and tears down its entire owned process tree before returning.
fn run_probe_v1(context: &ProbeContext<'_>) -> Result<(), SeedError> {
    let modules = &context.validated.descriptor.runtime_seed_manifest.modules;
    let (reader, writer) = os_pipe::pipe().map_err(|_| SeedError::Io)?;
    let stderr = writer.try_clone().map_err(|_| SeedError::Io)?;
    let probe_node = probe_node(&context.validated)?;
    let mut command = Command::new(&probe_node.path);
    command
        .arg("-e")
        .arg(PROBE_V1)
        .arg(context.destination)
        .arg(&modules.better_sqlite3.package_path)
        .arg(&modules.sql_js.package_path)
        .arg(&modules.sql_js.wasm_path)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::from(writer))
        .stderr(Stdio::from(stderr))
        .current_dir(context.destination);
    #[cfg(windows)]
    for name in ["SYSTEMROOT", "WINDIR"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let mut capture = Some(thread::spawn(move || read_probe_output(reader)));
    let mut tree = match ProcessTree::spawn(&mut command) {
        Ok(tree) => tree,
        Err(_) => {
            drop(command);
            let _ = capture.take().expect("capture retained").join();
            return Err(SeedError::ProbeFailed);
        }
    };
    drop(probe_node);
    drop(command);
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let mut captured = None;
    let direct = loop {
        match tree.try_wait_direct() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() >= deadline => break Err(SeedError::ProbeTimeout),
            Ok(None) => {}
            Err(_) => break Err(SeedError::ProbeFailed),
        }
        if capture.as_ref().is_some_and(|handle| handle.is_finished()) {
            match capture.take().expect("finished capture retained").join() {
                Ok(result) => {
                    let failure = result.as_ref().err().copied();
                    captured = Some(result);
                    if let Some(error) = failure {
                        break Err(error);
                    }
                }
                Err(_) => break Err(SeedError::ProbeFailed),
            }
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stop = tree.stop(PROBE_STOP_GRACE).map_err(|_| SeedError::ProbeFailed);
    let captured = match captured {
        Some(result) => result,
        None => capture.take().expect("unfinished capture retained").join().map_err(|_| SeedError::ProbeFailed)?,
    };

    let status = direct?;
    if !status.success() {
        return Err(SeedError::ProbeFailed);
    }
    let bytes = captured?;
    stop?;
    let output = std::str::from_utf8(&bytes).map_err(|_| SeedError::ProbeFailed)?;
    if output != format!("{PROBE_OK}\n") {
        return Err(SeedError::ProbeFailed);
    }
    Ok(())
}

fn read_probe_output(mut reader: os_pipe::PipeReader) -> Result<Vec<u8>, SeedError> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(PROBE_OUTPUT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| SeedError::ProbeFailed)?;
    if bytes.len() as u64 > PROBE_OUTPUT_BYTES {
        return Err(SeedError::ProbeOutputLimit);
    }
    Ok(bytes)
}

struct ProbeNode {
    path: PathBuf,
    #[cfg(unix)]
    _descriptor: OwnedFd,
    #[cfg(windows)]
    _lock: fs::File,
}

#[cfg(unix)]
fn probe_node(validated: &ValidatedSeed) -> Result<ProbeNode, SeedError> {
    // SAFETY: F_DUPFD duplicates the retained, validated Node handle and returns a new owned descriptor.
    let descriptor = unsafe { libc::fcntl(validated.node.as_raw_fd(), libc::F_DUPFD, 3) };
    if descriptor < 0 {
        return Err(SeedError::ProbeFailed);
    }
    // SAFETY: successful F_DUPFD returns unique ownership of this descriptor.
    let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
    #[cfg(target_os = "linux")]
    let path = PathBuf::from(format!("/proc/self/fd/{}", descriptor.as_raw_fd()));
    #[cfg(target_os = "macos")]
    let path = PathBuf::from(format!("/dev/fd/{}", descriptor.as_raw_fd()));
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let path = validated.payload_root_path(&validated.descriptor.node)?;
    Ok(ProbeNode { path, _descriptor: descriptor })
}

#[cfg(windows)]
fn probe_node(validated: &ValidatedSeed) -> Result<ProbeNode, SeedError> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;

    let path = validated.payload_root_path(&validated.descriptor.node)?;
    let lock = fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(&path).map_err(|_| SeedError::ProbeFailed)?;
    let file = cap_std::fs::File::from_std(lock.try_clone().map_err(|_| SeedError::ProbeFailed)?);
    if identity(&file.metadata().map_err(|_| SeedError::ProbeFailed)?)
        != identity(&validated.node.metadata().map_err(|_| SeedError::ProbeFailed)?)
    {
        return Err(SeedError::ProbeFailed);
    }
    Ok(ProbeNode { path, _lock: lock })
}


impl ValidatedSeed {
    fn payload_root_path(&self, relative: &str) -> Result<PathBuf, SeedError> {
        safe_relative(relative)?;
        let canonical = self.payload_root.join(relative).canonicalize().map_err(|_| SeedError::UnsafePath)?;
        if !canonical.starts_with(&self.payload_root) {
            return Err(SeedError::UnsafePath);
        }
        Ok(canonical)
    }
}

fn open_private_root(path: &Path) -> Result<Dir, SeedError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| SeedError::UnsafePath)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(SeedError::UnsafePath);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.permissions().mode() & 0o077 != 0 {
            return Err(SeedError::UnsafePath);
        }
    }
    Dir::open_ambient_dir(path, ambient_authority()).map_err(|_| SeedError::UnsafePath)
}

fn roots_overlap(payload_root: &Path, destination: &Path) -> Result<bool, SeedError> {
    let payload = payload_root.canonicalize().map_err(|_| SeedError::UnsafePath)?;
    let destination = destination.canonicalize().map_err(|_| SeedError::UnsafePath)?;
    Ok(payload.starts_with(&destination) || destination.starts_with(&payload))
}

fn safe_relative(path: &str) -> Result<Vec<String>, SeedError> {
    if path.is_empty() || path.starts_with('/') || path.contains(['\\', ':']) || path.bytes().any(|byte| byte <= 0x1f || byte == 0x7f) {
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
            || stem.strip_prefix("com").or_else(|| stem.strip_prefix("lpt")).is_some_and(|number| matches!(number, "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"));
        if reserved || key.starts_with(".melon-runtime-seed-") {
            return Err(SeedError::UnsafePath);
        }
        components.push(component.to_owned());
    }
    Ok(components)
}

fn path_key(path: &str) -> Result<String, SeedError> {
    Ok(safe_relative(path)?.into_iter().map(|component| component.to_lowercase()).collect::<Vec<_>>().join("/"))
}

fn validate_digest(digest: &str) -> Result<(), SeedError> {
    if digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) {
        Ok(())
    } else {
        Err(SeedError::Manifest)
    }
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

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

fn read_cap_file(root: &Dir, path: &str, limit: u64) -> Result<Vec<u8>, SeedError> {
    let file = open_cap_file(root, path)?;
    let length = file.metadata().map_err(|_| SeedError::Io)?.len();
    if length > limit {
        return Err(SeedError::Manifest);
    }
    let capacity = usize::try_from(length).map_err(|_| SeedError::Manifest)?;
    let mut bytes = Vec::with_capacity(capacity);
    file.take(limit + 1).read_to_end(&mut bytes).map_err(|_| SeedError::Io)?;
    if bytes.len() as u64 > limit {
        return Err(SeedError::Manifest);
    }
    Ok(bytes)
}

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

fn open_cap_directory(root: &Dir, path: &str) -> Result<Dir, SeedError> {
    let components = safe_relative(path)?;
    let mut current = root.try_clone().map_err(|_| SeedError::Io)?;
    for component in components {
        current = open_child_dir(&current, &component)?;
    }
    Ok(current)
}

fn traverse_parent(root: &Dir, components: &[String]) -> Result<(Dir, String), SeedError> {
    let mut current = root.try_clone().map_err(|_| SeedError::Io)?;
    for component in &components[..components.len().saturating_sub(1)] {
        current = open_child_dir(&current, component)?;
    }
    Ok((current, components.last().ok_or(SeedError::UnsafePath)?.clone()))
}

fn open_parent(root: &Dir, relative: &Path) -> Result<(Dir, std::ffi::OsString), SeedError> {
    let path = relative.to_str().ok_or(SeedError::Cleanup)?;
    let components = safe_relative(path).map_err(|_| SeedError::Cleanup)?;
    let (parent, name) = traverse_parent(root, &components).map_err(|_| SeedError::Cleanup)?;
    Ok((parent, name.into()))
}

fn open_child_dir(parent: &Dir, component: &str) -> Result<Dir, SeedError> {
    parent.open_dir_nofollow(component).map_err(|_| SeedError::Io)
}

fn identity(metadata: &cap_std::fs::Metadata) -> Identity {
    Identity(metadata.dev(), metadata.ino())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{Duration, Instant};

    const BETTER_PACKAGE: &str = "node_modules/better-sqlite3/package.json";
    const BETTER_BINARY: &str = "node_modules/better-sqlite3/build/Release/better_sqlite3.node";
    const SQL_PACKAGE: &str = "node_modules/sql.js/package.json";
    const SQL_WASM: &str = "node_modules/sql.js/dist/sql-wasm.wasm";

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "melon-runtime-seed-test-{}-{}",
                std::process::id(),
                getrandom::u64().expect("random temp name")
            ));
            fs::create_dir(&path).expect("create temp directory");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).expect("secure temp directory");
            }
            Self(path)
        }

        fn child(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            fs::create_dir(&path).expect("create child directory");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).expect("secure child directory");
            }
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct Fixture {
        _temp: TempDir,
        payload: PathBuf,
        destination: PathBuf,
    }

    impl Fixture {
        fn new(node_body: &str) -> Self {
            let temp = TempDir::new();
            let payload = temp.child("payload");
            let destination = temp.child("destination");
            let source = payload.join("runtime-seed");
            fs::create_dir_all(source.join("node_modules/better-sqlite3/build/Release")).expect("better directories");
            fs::create_dir_all(source.join("node_modules/sql.js/dist")).expect("sql directories");
            fs::create_dir_all(payload.join("metadata")).expect("metadata directory");
            fs::create_dir_all(payload.join("bin")).expect("bin directory");
            fs::create_dir_all(payload.join("app/node_modules/durindoor")).expect("CLI directory");
            fs::write(payload.join("app/node_modules/durindoor/cli.js"), b"cli").expect("CLI entrypoint");
            fs::write(source.join(BETTER_PACKAGE), br#"{"name":"better-sqlite3","version":"12.6.2"}"#).expect("better package");
            fs::write(source.join(BETTER_BINARY), b"native").expect("better binary");
            fs::write(source.join(SQL_PACKAGE), br#"{"name":"sql.js","version":"1.14.1"}"#).expect("sql package");
            fs::write(source.join(SQL_WASM), b"\0asm").expect("sql wasm");
            let node = payload.join(current_node_path());
            fs::write(&node, node_body).expect("fake node");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&node, fs::Permissions::from_mode(0o755)).expect("executable fake node");
            }
            let fixture = Self { _temp: temp, payload, destination };
            fixture.rebuild_authority();
            fixture
        }

        fn source(&self) -> PathBuf {
            self.payload.join("runtime-seed")
        }

        fn rebuild_authority(&self) {
            let mut files = Vec::new();
            collect_manifest_files(&self.source(), &self.source(), &mut files);
            files.sort_by(|left, right| collision_key(&left["path"].as_str().expect("path")).cmp(&collision_key(right["path"].as_str().expect("path"))));
            let manifest = format!("{}\n", serde_json::to_string_pretty(&json!({ "schemaVersion": 1, "files": files })).expect("manifest JSON"));
            fs::write(self.payload.join("metadata/runtime-seed-manifest.json"), &manifest).expect("write manifest");
            let count = files.len();
            let total: u64 = files.iter().map(|file| file["size"].as_u64().expect("size")).sum();
            let descriptor = json!({
                "schemaVersion": 1,
                "target": current_target(),
                "durindoorVersion": "3.15.2",
                "nodeVersion": "20.20.2",
                "nodeAbi": "115",
                "toolchain": {
                    "python": { "name": "python", "version": "Python 3", "sha256": digest(b"python") },
                    "cc": { "name": "cc", "version": "cc 1", "sha256": digest(b"cc") },
                    "cxx": { "name": "cxx", "version": "cxx 1", "sha256": digest(b"cxx") }
                },
                "cli": "app/node_modules/durindoor/cli.js",
                "node": current_node_path(),
                "runtimeSeedPath": "runtime-seed",
                "runtimeSeedManifest": {
                    "path": "metadata/runtime-seed-manifest.json",
                    "sha256": digest(manifest.as_bytes()),
                    "fileCount": count,
                    "totalBytes": total,
                    "destination": "data-runtime-root",
                    "probeVersion": 1,
                    "modules": {
                        "betterSqlite3": { "packagePath": BETTER_PACKAGE, "binaryPath": BETTER_BINARY, "version": "12.6.2" },
                        "sqlJs": { "packagePath": SQL_PACKAGE, "wasmPath": SQL_WASM, "version": "1.14.1" }
                    }
                },
                "managedLaunchReady": false,
                "licenses": {
                    "durindoor": "licenses/durindoor-LICENSE",
                    "node": "licenses/node-LICENSE",
                    "notices": "licenses/THIRD_PARTY_NOTICES.json",
                    "sha256": { "durindoor": digest(b"d"), "node": digest(b"n"), "notices": digest(b"l") }
                }
            });
            fs::write(self.payload.join("payload.json"), format!("{}\n", serde_json::to_string_pretty(&descriptor).expect("descriptor JSON"))).expect("write descriptor");
        }
    }

    fn collect_manifest_files(root: &Path, directory: &Path, files: &mut Vec<serde_json::Value>) {
        for entry in fs::read_dir(directory).expect("read seed directory") {
            let entry = entry.expect("seed entry");
            let metadata = entry.metadata().expect("seed metadata");
            if metadata.is_dir() {
                collect_manifest_files(root, &entry.path(), files);
            } else {
                let path = entry.path().strip_prefix(root).expect("relative seed path").to_string_lossy().replace('\\', "/");
                let bytes = fs::read(entry.path()).expect("seed bytes");
                #[cfg(unix)]
                let executable = {
                    use std::os::unix::fs::PermissionsExt;
                    metadata.permissions().mode() & 0o111 != 0
                };
                #[cfg(not(unix))]
                let executable = false;
                files.push(json!({ "path": path, "size": bytes.len(), "sha256": digest(&bytes), "executable": executable }));
            }
        }
    }

    fn digest(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    fn collision_key(path: &str) -> String {
        path.to_lowercase()
    }

    #[cfg(unix)]
    const SUCCESS_NODE: &str = "#!/bin/sh\n[ -z \"$MELON_SECRET_TOKEN\" ] || exit 9\nprintf 'melon-runtime-seed-probe-v1-ok\\n'\n";
    #[cfg(windows)]
    const SUCCESS_NODE: &str = "";

    #[test]
    fn installs_into_empty_destination_and_reuses_exact_partial_files() {
        let fixture = Fixture::new(SUCCESS_NODE);
        fs::create_dir_all(fixture.destination.join("node_modules/sql.js/dist")).expect("partial directories");
        fs::copy(fixture.source().join(SQL_WASM), fixture.destination.join(SQL_WASM)).expect("partial exact file");
        let before = fs::metadata(fixture.destination.join(SQL_WASM)).expect("partial metadata");

        let outcome = install_runtime_seed(&fixture.payload, &fixture.destination).expect("install seed");

        assert_eq!(outcome.created_files, 3);
        assert_eq!(outcome.reused_files, 1);
        assert_eq!(fs::metadata(fixture.destination.join(SQL_WASM)).expect("reused metadata").modified().ok(), before.modified().ok());
        for path in [BETTER_PACKAGE, BETTER_BINARY, SQL_PACKAGE, SQL_WASM] {
            assert_eq!(fs::read(fixture.destination.join(path)).expect("installed file"), fs::read(fixture.source().join(path)).expect("source file"));
        }
    }

    #[test]
    fn conflicting_existing_file_is_preserved_and_new_files_roll_back() {
        let fixture = Fixture::new(SUCCESS_NODE);
        let conflict = fixture.destination.join(SQL_WASM);
        fs::create_dir_all(conflict.parent().expect("conflict parent")).expect("conflict directories");
        fs::write(&conflict, b"user-owned").expect("conflict file");

        let error = install_runtime_seed(&fixture.payload, &fixture.destination).expect_err("conflict must fail");

        assert!(matches!(error, SeedError::Conflict));
        assert_eq!(fs::read(conflict).expect("preserved conflict"), b"user-owned");
        assert!(!fixture.destination.join(BETTER_PACKAGE).exists(), "new files rolled back");
    }

    #[test]
    fn manifest_tamper_missing_and_extra_source_files_fail_closed() {
        for mutation in ["tamper", "missing", "extra"] {
            let fixture = Fixture::new(SUCCESS_NODE);
            match mutation {
                "tamper" => fs::write(fixture.payload.join("metadata/runtime-seed-manifest.json"), b"{}\n").expect("tamper manifest"),
                "missing" => fs::remove_file(fixture.source().join(SQL_WASM)).expect("remove source"),
                "extra" => fs::write(fixture.source().join("extra"), b"extra").expect("extra source"),
                _ => unreachable!(),
            }
            assert!(install_runtime_seed(&fixture.payload, &fixture.destination).is_err(), "{mutation} rejected");
            assert!(fs::read_dir(&fixture.destination).expect("destination entries").next().is_none());
        }
    }

    #[test]
    fn strict_descriptor_and_package_versions_are_enforced() {
        let fixture = Fixture::new(SUCCESS_NODE);
        let descriptor_path = fixture.payload.join("payload.json");
        let mut descriptor: serde_json::Value = serde_json::from_slice(&fs::read(&descriptor_path).expect("descriptor")).expect("descriptor JSON");
        descriptor["unexpected"] = json!(true);
        fs::write(&descriptor_path, serde_json::to_vec(&descriptor).expect("descriptor bytes")).expect("unknown field");
        assert!(matches!(install_runtime_seed(&fixture.payload, &fixture.destination), Err(SeedError::Descriptor)));

        let fixture = Fixture::new(SUCCESS_NODE);
        fs::write(fixture.source().join(BETTER_PACKAGE), br#"{"name":"better-sqlite3","version":"0.0.0"}"#).expect("wrong version");
        fixture.rebuild_authority();
        assert!(matches!(install_runtime_seed(&fixture.payload, &fixture.destination), Err(SeedError::Manifest)));
    }
    #[cfg(windows)]
    #[test]
    fn windows_descriptor_uses_builder_node_path() {
        assert_eq!(current_node_path(), "bin/node.exe");
    }


    #[cfg(unix)]
    #[test]
    fn unsafe_source_destination_links_and_special_files_are_rejected() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let fixture = Fixture::new(SUCCESS_NODE);
        let outside = fixture._temp.child("outside");

        fs::remove_dir(&fixture.destination).expect("remove destination");
        symlink(&outside, &fixture.destination).expect("destination symlink");
        assert!(install_runtime_seed(&fixture.payload, &fixture.destination).is_err());

        let fixture = Fixture::new(SUCCESS_NODE);
        let wasm = fixture.source().join(SQL_WASM);
        fs::remove_file(&wasm).expect("remove wasm");
        symlink("elsewhere", &wasm).expect("source symlink");
        assert!(install_runtime_seed(&fixture.payload, &fixture.destination).is_err());

        let fixture = Fixture::new(SUCCESS_NODE);
        let special = fixture.source().join(SQL_WASM);
        fs::remove_file(&special).expect("remove wasm");
        let special_c = std::ffi::CString::new(special.as_os_str().as_encoded_bytes()).expect("FIFO path");
        assert_eq!(unsafe { libc::mkfifo(special_c.as_ptr(), 0o600) }, 0);
        assert!(install_runtime_seed(&fixture.payload, &fixture.destination).is_err());

        let fixture = Fixture::new(SUCCESS_NODE);
        fs::set_permissions(&fixture.destination, fs::Permissions::from_mode(0o755)).expect("unsafe destination mode");
        assert!(install_runtime_seed(&fixture.payload, &fixture.destination).is_err());
    }

    #[test]
    fn probe_failure_rolls_back_only_new_identity_retained_objects() {
        let fixture = Fixture::new(SUCCESS_NODE);
        fs::create_dir_all(fixture.destination.join("node_modules/sql.js/dist")).expect("existing directories");
        fs::copy(fixture.source().join(SQL_WASM), fixture.destination.join(SQL_WASM)).expect("existing exact file");

        let error = install_runtime_seed_with_probe(&fixture.payload, &fixture.destination, |_| Err(SeedError::ProbeFailed))
            .expect_err("probe failure");

        assert!(matches!(error, SeedError::ProbeFailed));
        assert_eq!(fs::read(fixture.destination.join(SQL_WASM)).expect("pre-existing file remains"), b"\0asm");
        assert!(!fixture.destination.join(BETTER_PACKAGE).exists());
    }

    #[cfg(unix)]
    #[test]
    fn validated_node_handle_survives_path_replacement() {
        let fixture = Fixture::new(SUCCESS_NODE);
        let validated = validate_seed(&fixture.payload).expect("validate seed");
        let node = fixture.payload.join(current_node_path());
        fs::rename(&node, node.with_extension("validated")).expect("move validated node path");
        fs::write(&node, "#!/bin/sh\nexit 91\n").expect("replace node path");
        let probe = probe_node(&validated).expect("resolve retained node handle");

        let status = Command::new(&probe.path).env_clear().status().expect("run retained node");

        assert!(status.success(), "replacement path was not executed");
    }

    #[test]
    fn rollback_cleanup_failure_is_reported_without_unlinking_replacement() {
        let fixture = Fixture::new(SUCCESS_NODE);
        let replacement = fixture.destination.join(BETTER_PACKAGE);
        let error = install_runtime_seed_with_probe(&fixture.payload, &fixture.destination, |_| {
            fs::remove_file(&replacement).expect("remove newly-created file");
            fs::create_dir(&replacement).expect("replace file with directory identity");
            Err(SeedError::ProbeFailed)
        }).expect_err("cleanup failure");

        assert!(matches!(error, SeedError::Cleanup), "{error:?}");
        assert!(replacement.is_dir(), "replacement preserved");
    }

    #[cfg(unix)]
    #[test]
    fn fixed_probe_succeeds_with_scrubbed_environment_and_retry() {
        let fixture = Fixture::new(SUCCESS_NODE);
        unsafe { std::env::set_var("MELON_SECRET_TOKEN", "must-not-leak") };
        let first = install_runtime_seed(&fixture.payload, &fixture.destination).expect("first install");
        unsafe { std::env::remove_var("MELON_SECRET_TOKEN") };
        let second = install_runtime_seed(&fixture.payload, &fixture.destination).expect("retry exact install");
        assert_eq!(first.created_files, 4);
        assert_eq!(second.reused_files, 4);
    }

    #[cfg(unix)]
    #[test]
    fn fixed_probe_failure_is_redacted_and_preserves_exact_preexisting_files() {
        let fixture = Fixture::new("#!/bin/sh\nsleep 10 &\nprintf 'plaintext secret' >&2\nexit 7\n");
        for path in [BETTER_PACKAGE, BETTER_BINARY, SQL_PACKAGE, SQL_WASM] {
            let destination = fixture.destination.join(path);
            fs::create_dir_all(destination.parent().expect("destination parent")).expect("destination directories");
            fs::copy(fixture.source().join(path), destination).expect("pre-existing exact file");
        }
        let started = Instant::now();
        let error = install_runtime_seed(&fixture.payload, &fixture.destination).expect_err("probe failure");
        assert!(matches!(error, SeedError::ProbeFailed), "{error:?}");
        assert!(!error.to_string().contains("secret"));
        assert!(started.elapsed() < Duration::from_secs(5), "nonzero probe and descendants are bounded");
        for path in [BETTER_PACKAGE, BETTER_BINARY, SQL_PACKAGE, SQL_WASM] {
            assert!(fixture.destination.join(path).exists(), "pre-existing {path} remains");
        }
    }

    #[cfg(unix)]
    #[test]
    fn fixed_probe_timeout_is_bounded_and_reaped() {
        let fixture = Fixture::new("#!/bin/sh\nsleep 10\n");
        let started = Instant::now();
        let error = install_runtime_seed(&fixture.payload, &fixture.destination).expect_err("probe must time out");
        assert!(matches!(error, SeedError::ProbeTimeout), "{error:?}");
        assert!(started.elapsed() < Duration::from_secs(5), "probe and reap are bounded");
        assert!(fs::read_dir(&fixture.destination).expect("destination entries").all(|entry| entry.expect("destination entry").file_name() == DESTINATION_LOCK), "failed probe rolled back");
    }

    #[cfg(unix)]
    #[test]
    fn fixed_probe_output_cap_is_bounded_and_reaped() {
        let fixture = Fixture::new("#!/bin/sh\ntrap '' PIPE\nwhile :; do printf 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx' || :; done\n");
        let started = Instant::now();
        let error = install_runtime_seed(&fixture.payload, &fixture.destination).expect_err("probe output must exceed cap");
        assert!(matches!(error, SeedError::ProbeOutputLimit), "{error:?}");
        assert!(started.elapsed() < Duration::from_secs(5), "probe and reap are bounded");
    }

    #[cfg(unix)]
    #[test]
    fn same_destination_is_serialized_across_processes() {
        if let Ok(root) = std::env::var("MELON_SEED_CHILD_ROOT") {
            let payload = PathBuf::from(&root).join("payload");
            let destination = PathBuf::from(&root).join("destination");
            let result = install_runtime_seed(&payload, &destination);
            fs::write(PathBuf::from(root).join(format!("done-{}", std::process::id())), format!("{result:?}")).expect("child result");
            assert!(result.is_ok());
            return;
        }
        let fixture = Fixture::new("#!/bin/sh\nsleep 1\nprintf 'melon-runtime-seed-probe-v1-ok\\n'\n");
        let spawn = || Command::new(std::env::current_exe().expect("test binary"))
            .arg("runtime_seed::tests::same_destination_is_serialized_across_processes")
            .arg("--exact")
            .env("MELON_SEED_CHILD_ROOT", &fixture._temp.0)
            .spawn()
            .expect("seed child");
        let mut first = spawn();
        std::thread::sleep(Duration::from_millis(100));
        let mut second = spawn();
        std::thread::sleep(Duration::from_millis(300));
        assert!(second.try_wait().expect("second state").is_none(), "second install waits for destination lock");
        assert!(first.wait().expect("first exit").success());
        assert!(second.wait().expect("second exit").success());
    }

    #[test]
    #[ignore = "requires an unpacked target-native payload matching this host; native runner evidence only"]
    fn real_native_payload_fixture_runs_probe_when_explicitly_supplied() {
        let payload = PathBuf::from(std::env::var_os("MELON_NATIVE_PAYLOAD_ROOT").expect("MELON_NATIVE_PAYLOAD_ROOT"));
        let temp = TempDir::new();
        let destination = temp.child("destination");
        install_runtime_seed(&payload, &destination).expect("native payload seed and probe");
    }
}
