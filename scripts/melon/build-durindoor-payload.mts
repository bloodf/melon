#!/usr/bin/env node

import { createHash } from 'node:crypto'
import { deflateRawSync, gunzipSync } from 'node:zlib'
import {
  chmodSync, closeSync, cpSync, existsSync, fsyncSync, linkSync, lstatSync, mkdirSync, mkdtempSync,
  openSync, readFileSync, readdirSync, readlinkSync, realpathSync, rmSync, statSync, writeFileSync, writeSync,
} from 'node:fs'
import { arch as hostArch, platform as hostPlatform, tmpdir } from 'node:os'
import { basename, delimiter, dirname, isAbsolute, join, relative, resolve, sep } from 'node:path'
import { spawnSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'

export interface PayloadEntry {
  path: string
  data: Uint8Array
  mode: number
  type?: 'file' | 'symlink'
}

export interface InspectedEntry {
  path: string
  mode: number
  modified: string
  type: 'file' | 'symlink'
}
interface Target {
  archiveRoot: string
  nodePath: string
  npmCli: string
  magic: readonly number[]
  architecture: 'linux-x64' | 'darwin-x64' | 'darwin-arm64' | 'windows-x64'
  tray?: string
}
interface PinnedArchive {
  filename: string
  sha256: string
}

interface RuntimePins {
  durindoor: {
    version: string
    packageIntegrity: string
    nodeVersion: string
    nodeHeaders: PinnedArchive
    nodeArchives: Record<string, PinnedArchive>
  }
}
const SCRIPT_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '../..')
const RUNTIME_PINS = readJson(join(SCRIPT_ROOT, 'apps/melon-desktop/runtime/runtime-pins.json')) as unknown as RuntimePins
const VERSION = RUNTIME_PINS.durindoor.nodeVersion
const DURINDOOR_VERSION = RUNTIME_PINS.durindoor.version
const FIXED_DOS_DATE = 0x0021
const TARGETS: Record<string, Target> = {
  'x86_64-unknown-linux-gnu': { archiveRoot: `node-v${VERSION}-linux-x64`, nodePath: 'bin/node', npmCli: 'lib/node_modules/npm/bin/npm-cli.js', magic: [0x7f, 0x45, 0x4c, 0x46], architecture: 'linux-x64', tray: 'tray_linux_release' },
  'x86_64-apple-darwin': { archiveRoot: `node-v${VERSION}-darwin-x64`, nodePath: 'bin/node', npmCli: 'lib/node_modules/npm/bin/npm-cli.js', magic: [0xcf, 0xfa, 0xed, 0xfe], architecture: 'darwin-x64', tray: 'tray_darwin_release' },
  'aarch64-apple-darwin': { archiveRoot: `node-v${VERSION}-darwin-arm64`, nodePath: 'bin/node', npmCli: 'lib/node_modules/npm/bin/npm-cli.js', magic: [0xcf, 0xfa, 0xed, 0xfe], architecture: 'darwin-arm64', tray: 'tray_darwin_release' },
  'x86_64-pc-windows-msvc': { archiveRoot: `node-v${VERSION}-win-x64`, nodePath: 'node.exe', npmCli: 'node_modules/npm/bin/npm-cli.js', magic: [0x4d, 0x5a], architecture: 'windows-x64' },
}
const MAX_ZIP_ENTRIES = 16_384
const MAX_UNCOMPRESSED_BYTES = 4 * 1024 * 1024 * 1024
const UINT32_MAX = 0xffff_ffff
const FORBIDDEN_SEGMENTS = new Set(['.env', '.9router', '.durindoor'])

/** Resolves one supported Rust target and rejects an archive not named by the committed pins. */
export function targetSpec(target: string, archive?: string): Target {
  const spec = TARGETS[target]
  const pin = RUNTIME_PINS.durindoor.nodeArchives[target]
  if (spec === undefined || pin === undefined) throw new Error(`unsupported payload target: ${target}`)
  if (archive !== undefined && basename(archive) !== pin.filename) throw new Error(`Node archive does not match target ${target}`)
  return spec
}

function crc32(data: Uint8Array): number {
  let crc = 0xffffffff
  for (const byte of data) {
    crc ^= byte
    for (let bit = 0; bit < 8; bit += 1) crc = (crc >>> 1) ^ (0xedb88320 & -(crc & 1))
  }
  return (crc ^ 0xffffffff) >>> 0
}

function zipPath(path: string): string {
  const normalized = path.replaceAll('\\', '/')
  const parts = normalized.split('/')
  if (normalized.length === 0 || normalized.startsWith('/') || /^[A-Za-z]:/.test(normalized) || parts.some(part => part === '' || part === '.' || part === '..')) throw new Error(`unsafe ZIP path: ${path}`)
  if (parts.some(part => FORBIDDEN_SEGMENTS.has(part) || part === '.bin')) throw new Error(`forbidden payload path: ${path}`)
  return normalized
}

/** Validates Rust extractor and classic ZIP numeric limits without allocating entry data. */
export function validateCanonicalMetadata(entries: ReadonlyArray<{ path: string; size: number }>): void {
  if (entries.length > MAX_ZIP_ENTRIES) throw new Error(`payload exceeds ${MAX_ZIP_ENTRIES} ZIP entries`)
  let total = 0
  let maximumOffset = 0
  let centralSize = 0
  for (const entry of entries) {
    const path = zipPath(entry.path)
    const nameBytes = Buffer.byteLength(path)
    if (!Number.isSafeInteger(entry.size) || entry.size < 0 || entry.size > UINT32_MAX) throw new Error(`ZIP entry size exceeds UInt32: ${path}`)
    total += entry.size
    if (total > MAX_UNCOMPRESSED_BYTES) throw new Error('payload exceeds 4 GiB uncompressed limit')
    maximumOffset += 30 + nameBytes + entry.size
    centralSize += 46 + nameBytes
    if (maximumOffset > UINT32_MAX || centralSize > UINT32_MAX || maximumOffset + centralSize > UINT32_MAX) throw new Error('ZIP offset exceeds UInt32')
  }
}

/** Creates a canonical ZIP containing regular files only, sorted by UTF-8 path. */
export function canonicalZip(source: PayloadEntry[]): Buffer {
  validateCanonicalMetadata(source.map(entry => ({ path: entry.path, size: entry.data.byteLength })))
  const entries = source.map(entry => {
    const path = zipPath(entry.path)
    if (entry.type === 'symlink') throw new Error(`symlink entry forbidden: ${path}`)
    if (entry.mode !== 0o644 && entry.mode !== 0o755) throw new Error(`unsupported mode for ${path}`)
    return { ...entry, path, data: Buffer.from(entry.data) }
  }).sort((left, right) => Buffer.from(left.path).compare(Buffer.from(right.path)))
  const names = new Set<string>()
  const local: Buffer[] = []
  const central: Buffer[] = []
  let offset = 0
  for (const entry of entries) {
    if (names.has(entry.path)) throw new Error(`duplicate ZIP path: ${entry.path}`)
    names.add(entry.path)
    const name = Buffer.from(entry.path)
    const compressed = deflateRawSync(entry.data, { level: 9 })
    const crc = crc32(entry.data)
    const header = Buffer.alloc(30)
    header.writeUInt32LE(0x04034b50, 0); header.writeUInt16LE(20, 4); header.writeUInt16LE(0x800, 6)
    header.writeUInt16LE(8, 8); header.writeUInt16LE(0, 10); header.writeUInt16LE(FIXED_DOS_DATE, 12)
    header.writeUInt32LE(crc, 14); header.writeUInt32LE(compressed.length, 18); header.writeUInt32LE(entry.data.length, 22)
    header.writeUInt16LE(name.length, 26)
    const record = Buffer.concat([header, name, compressed])
    local.push(record)
    const directory = Buffer.alloc(46)
    directory.writeUInt32LE(0x02014b50, 0); directory.writeUInt16LE(0x031e, 4); directory.writeUInt16LE(20, 6)
    directory.writeUInt16LE(0x800, 8); directory.writeUInt16LE(8, 10); directory.writeUInt16LE(0, 12); directory.writeUInt16LE(FIXED_DOS_DATE, 14)
    directory.writeUInt32LE(crc, 16); directory.writeUInt32LE(compressed.length, 20); directory.writeUInt32LE(entry.data.length, 24)
    directory.writeUInt16LE(name.length, 28); directory.writeUInt32LE(((0o100000 | entry.mode) << 16) >>> 0, 38); directory.writeUInt32LE(offset, 42)
    central.push(Buffer.concat([directory, name])); offset += record.length
  }
  const centralBytes = Buffer.concat(central)
  if (centralBytes.length > UINT32_MAX || offset > UINT32_MAX) throw new Error('ZIP directory exceeds UInt32')
  const end = Buffer.alloc(22)
  end.writeUInt32LE(0x06054b50, 0); end.writeUInt16LE(entries.length, 8); end.writeUInt16LE(entries.length, 10)
  end.writeUInt32LE(centralBytes.length, 12); end.writeUInt32LE(offset, 16)
  return Buffer.concat([...local, centralBytes, end])
}

/** Inspects canonical ZIP metadata without extracting files. */
export function inspectZip(bytes: Uint8Array): InspectedEntry[] {
  const input = Buffer.from(bytes)
  const end = input.lastIndexOf(Buffer.from([0x50, 0x4b, 0x05, 0x06]))
  if (end < 0) throw new Error('missing ZIP end record')
  const count = input.readUInt16LE(end + 10)
  let cursor = input.readUInt32LE(end + 16)
  const entries: InspectedEntry[] = []
  for (let index = 0; index < count; index += 1) {
    if (input.readUInt32LE(cursor) !== 0x02014b50) throw new Error('invalid ZIP directory')
    const nameLength = input.readUInt16LE(cursor + 28)
    const extraLength = input.readUInt16LE(cursor + 30)
    const commentLength = input.readUInt16LE(cursor + 32)
    const mode = (input.readUInt32LE(cursor + 38) >>> 16) & 0o7777
    const dosTime = input.readUInt16LE(cursor + 12)
    const dosDate = input.readUInt16LE(cursor + 14)
    const year = 1980 + (dosDate >>> 9)
    const month = (dosDate >>> 5) & 0xf
    const day = dosDate & 0x1f
    const hour = dosTime >>> 11
    const minute = (dosTime >>> 5) & 0x3f
    const second = (dosTime & 0x1f) * 2
    const kind = (input.readUInt32LE(cursor + 38) >>> 16) & 0o170000
    entries.push({ path: input.subarray(cursor + 46, cursor + 46 + nameLength).toString(), mode, modified: new Date(Date.UTC(year, month - 1, day, hour, minute, second)).toISOString(), type: kind === 0o120000 ? 'symlink' : 'file' })
    cursor += 46 + nameLength + extraLength + commentLength
  }
  return entries
}

function hasMagic(entry: PayloadEntry | undefined, magic: readonly number[]): boolean {
  return entry !== undefined && magic.every((byte, index) => entry.data[index] === byte)
}

function hasArchitecture(entry: PayloadEntry | undefined, architecture: Target['architecture']): boolean {
  if (entry === undefined) return false
  const data = entry.data
  if (architecture === 'linux-x64') return data.length > 20 && data[18] === 0x3e && data[19] === 0
  if (architecture === 'darwin-x64') return data.length > 8 && data[4] === 7 && data[7] === 1
  if (architecture === 'darwin-arm64') return data.length > 8 && data[4] === 12 && data[7] === 1
  if (data.length < 64) return false
  const pe = new DataView(data.buffer, data.byteOffset, data.byteLength).getUint32(0x3c, true)
  return pe + 6 <= data.length && data[pe] === 0x50 && data[pe + 1] === 0x45 && data[pe + 4] === 0x64 && data[pe + 5] === 0x86
}


interface PackageNotice {
  name: string
  version: string
  license: string
  licensePath?: string
  licenseSha256?: string
}

function isPackageManifest(path: string): boolean {
  const parts = path.split('/')
  const nodeModules = parts.lastIndexOf('node_modules')
  const packageParts = parts.slice(nodeModules + 1)
  return nodeModules >= 0 && packageParts.at(-1) === 'package.json'
    && (packageParts.length === 2 || (packageParts.length === 3 && packageParts[0]!.startsWith('@')))
}

function packageNotices(entries: PayloadEntry[]): PackageNotice[] {
  const byPath = new Map(entries.map(entry => [entry.path, entry]))
  const notices: PackageNotice[] = []
  for (const manifest of entries.filter(entry => isPackageManifest(entry.path))) {
    const value = JSON.parse(Buffer.from(manifest.data).toString('utf8')) as { name?: unknown; version?: unknown; license?: unknown }
    if (typeof value.name !== 'string' || typeof value.version !== 'string' || typeof value.license !== 'string' || value.license.length === 0) throw new Error(`package license metadata missing: ${manifest.path}`)
    const directory = dirname(manifest.path).replaceAll('\\', '/')
    const licensePath = [...byPath.keys()].filter(path => dirname(path).replaceAll('\\', '/') === directory && /^licen[cs]e(?:\.|$)/i.test(basename(path))).sort()[0]
    notices.push({
      name: value.name,
      version: value.version,
      license: value.license,
      ...(licensePath === undefined ? {} : { licensePath, licenseSha256: createHash('sha256').update(byPath.get(licensePath)!.data).digest('hex') }),
    })
  }
  return notices.sort((left, right) => Buffer.from(`${left.name}\0${left.version}`).compare(Buffer.from(`${right.name}\0${right.version}`)))
}

/** Generates canonical notices covering every shipped npm package manifest. */
export function thirdPartyNotices(entries: PayloadEntry[]): PayloadEntry {
  const notices = packageNotices(entries)
  if (notices.length === 0) throw new Error('payload has no npm package notices')
  return { path: 'licenses/THIRD_PARTY_NOTICES.json', data: Buffer.from(`${JSON.stringify(notices, null, 2)}\n`), mode: 0o644 }
}
/** Validates target-specific files, native formats, licenses, and tray policy before ZIP creation. */
export function validatePayloadEntries(entries: PayloadEntry[], target: string): void {
  const spec = targetSpec(target)
  for (const entry of entries) zipPath(entry.path)
  const byPath = new Map(entries.map(entry => [entry.path, entry]))
  const nodePath = `bin/${target.includes('windows') ? 'node.exe' : 'node'}`
  const node = byPath.get(nodePath)
  if (!hasMagic(node, spec.magic) || !hasArchitecture(node, spec.architecture) || (spec.tray !== undefined && node?.mode !== 0o755)) throw new Error('payload has missing, non-executable, or wrong-architecture Node binary')
  const descriptorEntry = byPath.get('payload.json')
  if (descriptorEntry === undefined) throw new Error('payload descriptor missing')
  const descriptor = JSON.parse(Buffer.from(descriptorEntry.data).toString('utf8')) as unknown
  if (descriptor === null || typeof descriptor !== 'object' || !('node' in descriptor) || descriptor.node !== nodePath) throw new Error('payload descriptor Node path mismatch')
  if (!byPath.has('app/node_modules/durindoor/cli.js')) throw new Error('payload is missing DurinDoor CLI')
  if (!byPath.has('licenses/durindoor-LICENSE')) throw new Error('payload is missing DurinDoor license')
  if (!byPath.has('licenses/node-LICENSE')) throw new Error('payload is missing Node license')
  const noticesEntry = byPath.get('licenses/THIRD_PARTY_NOTICES.json')
  if (noticesEntry === undefined) throw new Error('payload is missing third-party notices')
  const expectedNotices = packageNotices(entries)
  const actualNotices = JSON.parse(Buffer.from(noticesEntry.data).toString('utf8')) as PackageNotice[]
  if (JSON.stringify(actualNotices) !== JSON.stringify(expectedNotices)) throw new Error('third-party notices do not cover shipped packages and license hashes')
  const wasm = byPath.get('runtime-seed/node_modules/sql.js/dist/sql-wasm.wasm')
  if (!hasMagic(wasm, [0, 0x61, 0x73, 0x6d])) throw new Error('payload has missing or invalid sql.js WASM')
  const native = byPath.get('runtime-seed/node_modules/better-sqlite3/build/Release/better_sqlite3.node')
  if (!hasMagic(native, spec.magic) || !hasArchitecture(native, spec.architecture)) throw new Error('payload has missing or wrong-architecture better-sqlite3 binary')
  const trayEntries = entries.filter(entry => entry.path.includes('/systray2/'))
  if (spec.tray === undefined && trayEntries.length > 0) throw new Error('Windows payload must exclude systray2')
  if (spec.tray !== undefined) {
    const tray = byPath.get(`runtime-seed/node_modules/systray2/traybin/${spec.tray}`)
    if (tray === undefined || tray.mode !== 0o755 || !hasMagic(tray, spec.magic) || !hasArchitecture(tray, spec.architecture)) throw new Error('payload has missing, non-executable, or wrong-architecture systray2 binary')
  }
  if (entries.some(entry => entry.path.includes('/systray/'))) throw new Error('legacy systray is forbidden')
}

function readJson(path: string): Record<string, unknown> {
  return JSON.parse(readFileSync(path, 'utf8')) as Record<string, unknown>
}

/** Verifies committed npm locks match exact runtime pins and seed roots. */
export function validateLockedManifests(root: string): void {
  const cli = readJson(join(root, 'package-lock.json'))
  const seed = readJson(join(root, 'runtime-seed/package-lock.json'))
  const build = readJson(join(root, 'runtime-seed-build/package-lock.json'))
  const cliPackages = cli.packages as Record<string, { version?: string; integrity?: string }> | undefined
  const seedPackages = seed.packages as Record<string, { version?: string }> | undefined
  const buildPackages = build.packages as Record<string, { version?: string; integrity?: string; hasInstallScript?: boolean }> | undefined
  const durindoor = cliPackages?.['node_modules/durindoor']
  if (durindoor?.version !== DURINDOOR_VERSION) throw new Error(`DurinDoor lock must pin ${DURINDOOR_VERSION}`)
  if (durindoor.integrity !== RUNTIME_PINS.durindoor.packageIntegrity) throw new Error('DurinDoor lock integrity must match runtime pins')
  for (const [name, version] of [['better-sqlite3', '12.6.2'], ['sql.js', '1.14.1'], ['systray2', '2.1.4']]) {
    if (seedPackages?.[`node_modules/${name}`]?.version !== version) throw new Error(`runtime seed lock must pin ${name}@${version}`)
  }
  const nodeGyp = buildPackages?.['node_modules/node-gyp']
  if (nodeGyp?.version !== '10.1.0' || nodeGyp.integrity !== 'sha512-B4J5M1cABxPc5PwfjhbV5hoy2DP9p8lFXASnEN6hugXOa61416tnTZ29x9sSwAd0o99XNIcpvDDy1swAExsVKA==') throw new Error('runtime seed build lock must pin authenticated node-gyp@10.1.0')
  const lifecyclePackages = Object.entries(seedPackages ?? {}).filter(([, value]) => (value as { hasInstallScript?: boolean }).hasInstallScript).map(([path]) => path)
  if (JSON.stringify(lifecyclePackages) !== JSON.stringify(['node_modules/better-sqlite3'])) throw new Error('runtime seed lifecycle set must contain only better-sqlite3')
  if (Object.values(buildPackages ?? {}).some(value => value.hasInstallScript === true)) throw new Error('runtime seed build tool lock must contain no lifecycle scripts')
}
function run(command: string, args: string[], cwd: string, dataDir: string, path = process.env.PATH ?? '', extraEnv: Record<string, string> = {}): string {
  const home = join(dataDir, 'home')
  const cache = join(dataDir, 'npm-cache')
  mkdirSync(home, { recursive: true }); mkdirSync(cache, { recursive: true })
  const env: Record<string, string> = {
    DATA_DIR: dataDir,
    HOME: home,
    USERPROFILE: home,
    npm_config_cache: cache,
    PATH: path,
    ...extraEnv,
  }
  for (const name of ['SYSTEMROOT', 'WINDIR', 'COMSPEC', 'TEMP', 'TMP', 'TMPDIR']) if (process.env[name] !== undefined) env[name] = process.env[name]!
  const result = spawnSync(command, args, { cwd, env, encoding: 'utf8' })
  const output = `${result.stdout}${result.stderr}`
  if (result.status !== 0) throw new Error(`${command} ${args.join(' ')} failed\n${output}`)
  return output
}

function runStdout(command: string, args: string[], cwd: string, dataDir: string): string {
  const result = spawnSync(command, args, { cwd, env: { DATA_DIR: dataDir, HOME: join(dataDir, 'home'), USERPROFILE: join(dataDir, 'home'), PATH: process.env.PATH ?? '' }, encoding: 'utf8' })
  if (result.status !== 0) throw new Error(`${command} ${args.join(' ')} failed\n${result.stdout}${result.stderr}`)
  return result.stdout
}

function collectFiles(root: string, prefix: string, omit: (path: string) => boolean): PayloadEntry[] {
  const entries: PayloadEntry[] = []
  const visit = (directory: string) => {
    for (const name of readdirSync(directory).sort()) {
      const path = join(directory, name)
      const relativePath = relative(root, path).split(sep).join('/')
      if (relativePath.split('/').includes('.bin')) continue
      const info = lstatSync(path)
      if (info.isSymbolicLink()) throw new Error(`symlink in staged closure: ${relativePath}`)
      if (info.isDirectory()) visit(path)
      else if (info.isFile()) entries.push({ path: `${prefix}/${relativePath}`, data: readFileSync(path), mode: info.mode & 0o111 ? 0o755 : 0o644 })
      else throw new Error(`unsupported staged entry: ${relativePath}`)
    }
  }
  visit(root)
  return entries
}

/** Verifies supplied checksum metadata and archive bytes against one committed digest.
 * @param archive - Downloaded Node archive.
 * @param checksums - Supplied official checksum document.
 * @param expectedName - Exact committed archive filename.
 * @param pinnedSha256 - Exact committed archive digest. */
export function verifyChecksum(archive: string, checksums: string, expectedName: string, pinnedSha256: string): void {
  const supplied = readFileSync(checksums, 'utf8').split(/\r?\n/).find(line => line.endsWith(`  ${expectedName}`))?.slice(0, 64)
  if (supplied === undefined) throw new Error(`official checksum missing for ${expectedName}`)
  if (supplied !== pinnedSha256) throw new Error(`Node pinned checksum mismatch for ${expectedName}`)
  const actual = createHash('sha256').update(readFileSync(archive)).digest('hex')
  if (actual !== pinnedSha256) throw new Error(`Node archive checksum mismatch for ${expectedName}`)
}

type SandboxProbe = (command: string, args: string[]) => string

/** Selects a native network sandbox after proving a distinct Linux network namespace.
 * @param target - Rust target being packaged.
 * @param runner - Explicit OS sandbox executable.
 * @param host - Injectable native host identity.
 * @param probe - Injectable sandbox behavior probe.
 * @param parentNetworkNamespace - Parent process network namespace identifier.
 * @returns command prefix for sandboxed validation. */
export function nativeSandboxPrefix(
  target: string,
  runner: string | undefined,
  host: { platform: NodeJS.Platform; arch: string } = { platform: hostPlatform(), arch: hostArch() },
  probe: SandboxProbe = (command, args) => {
    const result = spawnSync(command, args, { encoding: 'utf8' })
    if (result.status !== 0) throw new Error(`sandbox runner behavior probe failed\n${result.stdout}\n${result.stderr}`)
    return result.stdout
  },
  parentNetworkNamespace = host.platform === 'linux' ? readlinkSync('/proc/self/ns/net') : '',
): string[] {
  const nativeTarget = host.platform === 'linux' && host.arch === 'x64' ? 'x86_64-unknown-linux-gnu'
    : host.platform === 'darwin' && host.arch === 'x64' ? 'x86_64-apple-darwin'
      : host.platform === 'darwin' && host.arch === 'arm64' ? 'aarch64-apple-darwin'
        : host.platform === 'win32' && host.arch === 'x64' ? 'x86_64-pc-windows-msvc' : undefined
  if (target !== nativeTarget) throw new Error(`payload build requires native target ${nativeTarget ?? `${host.platform}-${host.arch}`}`)
  if (runner === undefined) throw new Error('payload build requires --sandbox-runner')
  if (host.platform !== 'linux') throw new Error(`offline sandbox is not implemented for native ${host.platform} payload builds`)
  if (basename(runner) !== 'unshare') throw new Error('Linux sandbox runner must be unshare')
  const prefix = [runner, '--user', '--map-root-user', '--net', '--']
  const isolated = probe(runner, [...prefix.slice(1), process.execPath, '-e', "process.stdout.write(require('fs').readlinkSync('/proc/self/ns/net'))"])
  if (isolated.trim() === parentNetworkNamespace.trim()) throw new Error('sandbox runner did not isolate network namespace')
  return prefix
}

interface ArchiveEntry { name: string; type: 'file' | 'directory'; linkName?: string }

function safeArchiveEntryName(name: string): string {
  const normalized = name.replaceAll('\\', '/').replace(/^\.\//, '').replace(/\/$/, '')
  if (normalized.length === 0 || isAbsolute(normalized) || /^[A-Za-z]:/.test(normalized) || normalized.split('/').some(part => part === '..')) throw new Error(`unsafe Node archive entry: ${name}`)
  return normalized
}

function tarEntries(bytes: Buffer): ArchiveEntry[] {
  const tar = bytes[0] === 0x1f && bytes[1] === 0x8b ? gunzipSync(bytes) : bytes
  const entries: ArchiveEntry[] = []
  for (let offset = 0; offset + 512 <= tar.length;) {
    const header = tar.subarray(offset, offset + 512)
    if (header.every(byte => byte === 0)) break
    const field = (start: number, length: number) => header.subarray(start, start + length).toString('utf8').replace(/\0.*$/, '')
    const name = safeArchiveEntryName([field(345, 155), field(0, 100)].filter(Boolean).join('/'))
    const linkName = field(157, 100)
    const type = header[156]
    const sizeText = field(124, 12).trim()
    const size = sizeText.length === 0 ? 0 : Number.parseInt(sizeText, 8)
    if (!Number.isSafeInteger(size) || size < 0) throw new Error(`invalid tar entry size: ${name}`)
    const regular = type === 0 || type === 0x30
    entries.push({ name, type: type === 0x35 ? 'directory' : 'file', ...(!regular && type !== 0x35 ? { linkName: linkName || `type:${String.fromCharCode(type)}` } : {}) })
    offset += 512 + Math.ceil(size / 512) * 512
  }
  return entries
}

function zipEntries(bytes: Buffer): ArchiveEntry[] {
  const end = bytes.lastIndexOf(Buffer.from([0x50, 0x4b, 0x05, 0x06]))
  if (end < 0) throw new Error('invalid Node ZIP')
  const count = bytes.readUInt16LE(end + 10)
  let cursor = bytes.readUInt32LE(end + 16)
  const entries: ArchiveEntry[] = []
  for (let index = 0; index < count; index += 1) {
    if (bytes.readUInt32LE(cursor) !== 0x02014b50) throw new Error('invalid Node ZIP directory')
    const nameLength = bytes.readUInt16LE(cursor + 28)
    const extraLength = bytes.readUInt16LE(cursor + 30)
    const commentLength = bytes.readUInt16LE(cursor + 32)
    const rawName = bytes.subarray(cursor + 46, cursor + 46 + nameLength).toString('utf8')
    const name = safeArchiveEntryName(rawName)
    const unixType = (bytes.readUInt32LE(cursor + 38) >>> 16) & 0o170000
    if (unixType !== 0 && unixType !== 0o100000 && unixType !== 0o040000) throw new Error(`unsafe Node ZIP entry type: ${name}`)
    entries.push({ name, type: rawName.endsWith('/') || unixType === 0o040000 ? 'directory' : 'file' })
    cursor += 46 + nameLength + extraLength + commentLength
  }
  return entries
}

/** Parses archive metadata structurally and rejects links, special files, duplicates, and unsafe names.
 *  Returns only the selected subset; validation (duplicate, type) applies only to selected entries. */
export function preflightNodeArchive(archive: string, selectedRoots?: readonly string[]): ArchiveEntry[] {
  const bytes = readFileSync(archive)
  const entries = archive.endsWith('.zip') ? zipEntries(bytes) : tarEntries(bytes)
  const selectedEntries: ArchiveEntry[] = []
  const selectedNames = new Set<string>()
  for (const entry of entries) {
    const selected = selectedRoots === undefined || selectedRoots.some(root => entry.name === root || entry.name.startsWith(`${root}/`))
    if (selected) {
      if (entry.linkName !== undefined) throw new Error(`Node archive link or special entry forbidden: ${entry.name}`)
      if (selectedNames.has(entry.name)) throw new Error(`duplicate Node archive entry: ${entry.name}`)
      selectedNames.add(entry.name)
      selectedEntries.push(entry)
    }
  }
  return selectedEntries
}

function extractNodeRuntime(archive: string, destination: string, spec: Target, dataDir: string): string {
  const selected = [`${spec.archiveRoot}/${spec.nodePath}`, `${spec.archiveRoot}/${spec.npmCli.split('/bin/')[0]}`, `${spec.archiveRoot}/LICENSE`]
  const entries = preflightNodeArchive(archive, selected)
  const names = new Set(entries.map(entry => entry.name))
  const required = [`${spec.archiveRoot}/${spec.nodePath}`, `${spec.archiveRoot}/${spec.npmCli}`, `${spec.archiveRoot}/LICENSE`]
  if (required.some(name => !names.has(name))) throw new Error('Node archive lacks runtime, npm, or license')
  mkdirSync(destination, { recursive: true })
  run('tar', [archive.endsWith('.zip') ? '-xf' : '-xzf', archive, '-C', destination, ...selected], dirname(archive), dataDir)
  const root = join(destination, spec.archiveRoot)
  for (const requiredPath of [spec.nodePath, spec.npmCli, 'LICENSE']) {
    let current = root
    for (const component of requiredPath.split('/')) {
      current = join(current, component)
      const info = lstatSync(current)
      if (info.isSymbolicLink() || (!info.isDirectory() && !info.isFile())) throw new Error(`unsafe materialized Node component: ${requiredPath}`)
    }
  }
  return root
}

/** Structurally validates required regular files in the shared official Node headers archive. */
export function preflightNodeHeadersArchive(archive: string): ArchiveEntry[] {
  const root = `node-v${VERSION}/include/node`
  const entries = preflightNodeArchive(archive, [root])
  const names = new Set(entries.map(entry => entry.name))
  for (const required of [`${root}/node.h`, `${root}/common.gypi`, `${root}/config.gypi`]) {
    if (!names.has(required)) throw new Error(`Node headers archive lacks ${basename(required)}`)
  }
  return entries
}

function extractNodeHeaders(archive: string, destination: string, dataDir: string): string {
  preflightNodeHeadersArchive(archive)
  const archiveRoot = `node-v${VERSION}`
  mkdirSync(destination, { recursive: true })
  run('tar', ['-xzf', archive, '-C', destination, `${archiveRoot}/include/node`], dirname(archive), dataDir)
  const root = join(destination, archiveRoot)
  for (const required of ['include/node/node.h', 'include/node/common.gypi', 'include/node/config.gypi']) {
    if (!lstatSync(join(root, required), { throwIfNoEntry: false })?.isFile()) throw new Error(`materialized Node headers missing ${required}`)
  }
  return root
}
function runNpm(nodeRoot: string, spec: Target, args: string[], cwd: string, dataDir: string): void {
  const node = join(nodeRoot, spec.nodePath)
  const npmCli = join(nodeRoot, spec.npmCli)
  run(node, [npmCli, ...args], cwd, dataDir, `${dirname(node)}${delimiter}${process.env.PATH ?? ''}`)
}

type NativeBuildRunner = (command: string, args: string[], cwd: string, dataDir: string, path: string, env: Record<string, string>) => string

/** Rejects evidence that native build selected a downloaded or prebuilt artifact.
 * @param log - Complete npm rebuild output.
 */
export function validateNativeBuildLog(log: string): void {
  if (/prebuild-install|\bdownload(?:ing|ed)?\b|https?:\/\/github\.com|\bprebuilt\b/i.test(log)) throw new Error('native seed must build from locked source')
}

export interface NativeToolchainOptions { directories: string[]; python: string; cc: string; cxx: string }
interface ToolEvidence { realpath: string; version: string }
export interface NativeToolchainEvidence { path: string; python: ToolEvidence; cc: ToolEvidence; cxx: ToolEvidence }
type ToolProbe = (command: string, args: string[]) => string

/** Resolves an explicit native toolchain and records non-secret executable evidence. */
export function resolveNativeToolchain(options: NativeToolchainOptions, probe: ToolProbe = (command, args) => {
  const result = spawnSync(command, args, { encoding: 'utf8', env: { PATH: options.directories.join(delimiter) } })
  if (result.status !== 0) throw new Error(`native toolchain probe failed for ${command}`)
  return `${result.stdout}${result.stderr}`.trim()
}): NativeToolchainEvidence {
  if (options.directories.length === 0) throw new Error('native toolchain requires at least one directory')
  for (const directory of options.directories) {
    if (!isAbsolute(directory)) throw new Error('native toolchain directories must be absolute')
    const info = statSync(directory)
    if (!info.isDirectory()) throw new Error(`native toolchain directory is not a directory: ${directory}`)
    if (process.platform !== 'win32' && (info.mode & 0o002) !== 0) throw new Error(`native toolchain directory is world-writable: ${directory}`)
  }
  const executable = (path: string, args: string[]): ToolEvidence => {
    if (!isAbsolute(path)) throw new Error('native toolchain executables must be absolute')
    const realpath = realpathSync(path)
    if (!statSync(realpath).isFile()) throw new Error(`native toolchain executable is not a file: ${path}`)
    if (!options.directories.some(directory => realpath === directory || realpath.startsWith(`${realpathSync(directory)}${sep}`))) throw new Error(`native toolchain executable outside allowlisted directories: ${path}`)
    return { realpath, version: probe(realpath, args).slice(0, 512) }
  }
  return { path: options.directories.map(directory => realpathSync(directory)).join(delimiter), python: executable(options.python, ['--version']), cc: executable(options.cc, ['--version']), cxx: executable(options.cxx, ['--version']) }
}

/** Builds better-sqlite3 from locked source under the positively-probed sandbox. */
export function buildNativeSeed(nodeRoot: string, headersRoot: string, buildTools: string, spec: Target, seedProject: string, dataDir: string, sandboxPrefix: string[], toolchain: NativeToolchainEvidence, runner: NativeBuildRunner = run): void {
  if (sandboxPrefix.length === 0) throw new Error('native seed build requires sandbox prefix')
  for (const header of ['include/node/node.h', 'include/node/common.gypi', 'include/node/config.gypi']) {
    if (!lstatSync(join(headersRoot, header), { throwIfNoEntry: false })?.isFile()) throw new Error(`verified Node headers missing ${header}`)
  }
  const node = join(nodeRoot, spec.nodePath)
  const nodeGyp = join(buildTools, 'node_modules/node-gyp/bin/node-gyp.js')
  if (!lstatSync(nodeGyp, { throwIfNoEntry: false })?.isFile()) throw new Error('locked node-gyp build tool missing')
  const nativeProject = join(seedProject, 'node_modules/better-sqlite3')
  const env = { npm_config_build_from_source: 'true', npm_config_nodedir: headersRoot, npm_config_tarball: '', npm_config_python: toolchain.python.realpath, PYTHON: toolchain.python.realpath, CC: toolchain.cc.realpath, CXX: toolchain.cxx.realpath }
  const log = runner(sandboxPrefix[0]!, [...sandboxPrefix.slice(1), node, nodeGyp, 'rebuild', '--release', `--nodedir=${headersRoot}`], nativeProject, dataDir, toolchain.path, env)
  validateNativeBuildLog(log)
}
function copyRuntimeSeed(seed: string, dataDir: string): void {
  const runtime = join(dataDir, 'runtime')
  mkdirSync(runtime, { recursive: true })
  cpSync(join(seed, 'node_modules'), join(runtime, 'node_modules'), { recursive: true, dereference: true })
  cpSync(join(seed, 'package.json'), join(runtime, 'package.json'))
}

/** Writes preload guards for network and process APIs used by validated Node scripts. */
export function writeOfflineGuard(path: string): void {
  writeFileSync(path, [
    "const deny=()=>{throw new Error('offline capability blocked')}",
    "globalThis.fetch=deny",
    "const http=require('http');http.request=deny;http.get=deny;const https=require('https');https.request=deny;https.get=deny",
    "const net=require('net');net.connect=deny;net.createConnection=deny;net.Socket.prototype.connect=deny;const tls=require('tls');tls.connect=deny",
    "const dns=require('dns');for(const key of ['lookup','resolve','resolve4','resolve6','resolveAny','resolveCaa','resolveCname','resolveMx','resolveNaptr','resolveNs','resolvePtr','resolveSoa','resolveSrv','resolveTxt','reverse'])dns[key]=deny;for(const key of Object.keys(dns.promises))if(typeof dns.promises[key]==='function')dns.promises[key]=deny",
    "const dgram=require('dgram');dgram.createSocket=deny",
    "const child=require('child_process');for(const key of ['spawn','spawnSync','exec','execSync','execFile','execFileSync','fork'])child[key]=deny",
  ].join(';'))
}

function runSandboxed(prefix: string[], command: string, args: string[], cwd: string, dataDir: string, path: string, extraEnv: Record<string, string>): string {
  return run(prefix[0]!, [...prefix.slice(1), command, ...args], cwd, dataDir, path, extraEnv)
}

function validateOffline(staging: string, dataDir: string, target: string, sandboxPrefix: string[]): void {
  copyRuntimeSeed(join(staging, 'runtime-seed'), dataDir)
  const node = join(staging, 'bin', target.includes('windows') ? 'node.exe' : 'node')
  const cli = join(staging, 'app/node_modules/durindoor/cli.js')
  const emptyPath = join(dataDir, 'empty-path')
  const runtimeModules = join(dataDir, 'runtime/node_modules')
  mkdirSync(emptyPath)
  const guard = join(dataDir, 'offline-guard.cjs')
  writeOfflineGuard(guard)
  const offlineEnv = { NODE_PATH: runtimeModules, NODE_OPTIONS: `--require=${guard}` }
  const validate = (args: string[]) => runSandboxed(sandboxPrefix, node, args, staging, dataDir, emptyPath, offlineEnv)
  if (!validate([cli, '--version']).includes(DURINDOOR_VERSION)) throw new Error('offline CLI version failed')
  if (!validate([cli, '--help']).includes('--skip-update')) throw new Error('offline CLI help failed')
  const smoke = [
    "const Database=require('better-sqlite3')",
    "const db=new Database(':memory:')",
    "if(db.prepare('select 42 as value').get().value!==42)process.exit(2)",
    'db.close()',
    "const init=require('sql.js')",
    "const wasm=require.resolve('sql.js/dist/sql-wasm.wasm')",
    "init({locateFile:()=>wasm}).then(SQL=>{const db=new SQL.Database();const rows=db.exec('select 42 as value');db.close();if(rows[0].values[0][0]!==42)process.exit(3)}).catch(()=>process.exit(4))",
  ].join(';')
  validate(['-e', smoke])
  const sqliteHook = join(staging, 'app/node_modules/durindoor/hooks/sqliteRuntime.js')
  const trayHook = join(staging, 'app/node_modules/durindoor/hooks/trayRuntime.js')
  const bootstrap = target.includes('windows')
    ? `const s=require(${JSON.stringify(sqliteHook)}).ensureSqliteRuntime({silent:true});if(!s.sqlJs||!s.betterSqlite)process.exit(5)`
    : `const s=require(${JSON.stringify(sqliteHook)}).ensureSqliteRuntime({silent:true});const t=require(${JSON.stringify(trayHook)}).ensureTrayRuntime({silent:true});if(!s.sqlJs||!s.betterSqlite||!t.systray)process.exit(5)`
  validate(['-e', bootstrap])
}

function assertNativeLoadGateRejectsCorruption(staging: string, dataDir: string, target: string, sandboxPrefix: string[]): void {
  const native = join(staging, 'runtime-seed/node_modules/better-sqlite3/build/Release/better_sqlite3.node')
  const magic = targetSpec(target).magic
  writeFileSync(native, Uint8Array.from([...magic, 0, 0, 0, 0]))
  try {
    validateOffline(staging, dataDir, target, sandboxPrefix)
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error)
    if (/better-sqlite3|better_sqlite3\.node|file too short|invalid ELF|not a valid Win32|mach-o/i.test(message)) return
    throw new Error(`wrong-ABI probe failed for an unrelated reason: ${message}`)
  }
  throw new Error('offline native load gate accepted a truncated wrong-ABI module')
}

/** Writes validated bytes, atomically claims an absent final path, and durably syncs its directory on Unix. */
export function publishExclusive(
  output: string,
  bytes: Uint8Array,
  beforeClaim?: (output: string) => void,
  link = linkSync,
  syncDirectory: (directory: string) => void = directory => {
    if (process.platform === 'win32') return
    const descriptor = openSync(directory, 'r')
    try { fsyncSync(descriptor) } finally { closeSync(descriptor) }
  },
): void {
  const temporary = `${output}.part-${process.pid}-${createHash('sha256').update(bytes).digest('hex').slice(0, 12)}`
  let descriptor: number | undefined
  try {
    descriptor = openSync(temporary, 'wx', 0o600)
    writeSync(descriptor, bytes)
    fsyncSync(descriptor)
    closeSync(descriptor)
    descriptor = undefined
    beforeClaim?.(output)
    link(temporary, output)
    syncDirectory(dirname(output))
  } finally {
    if (descriptor !== undefined) closeSync(descriptor)
    rmSync(temporary, { force: true })
  }
}

type RustGateRunner = (descriptor: string) => void

/** Passes actual payload bytes through the production Rust extractor before publication. */
/** Requires positive cargo output naming the ignored production fixture. */
export function validateRustGateOutput(output: string): void {
  if (!/test runtime::tests::builder_payload_fixture_is_accepted \.\.\. ok/.test(output)) throw new Error('Rust payload gate did not execute builder_payload_fixture_is_accepted')
}

export function runRustPayloadGate(bytes: Uint8Array, target: string, work: string, runner: RustGateRunner = descriptor => {
  const result = spawnSync('cargo', ['test', '--manifest-path', 'apps/melon-desktop/src-tauri/Cargo.toml', 'builder_payload_fixture_is_accepted', '--', '--ignored', '--nocapture'], {
    cwd: SCRIPT_ROOT,
    env: { ...process.env, MELON_PAYLOAD_FIXTURE: descriptor },
    encoding: 'utf8',
  })
  const output = `${result.stdout}${result.stderr}`
  if (result.status !== 0) throw new Error(`Rust payload gate failed\n${output}`)
  validateRustGateOutput(output)
}): void {
  const gate = join(work, 'rust-gate')
  const cache = join(gate, 'cache')
  const finalParent = join(gate, 'final')
  try {
    mkdirSync(cache, { recursive: true, mode: 0o700 })
    mkdirSync(finalParent, { recursive: true, mode: 0o700 })
    const archive = 'payload.zip'
    writeFileSync(join(cache, archive), bytes, { mode: 0o600 })
    const descriptor = join(gate, 'fixture.json')
    writeFileSync(descriptor, JSON.stringify({ root: gate, cache, archive, targetId: target, sha256: createHash('sha256').update(bytes).digest('hex'), finalDir: join(finalParent, target) }), { mode: 0o600 })
    runner(descriptor)
  } finally {
    rmSync(gate, { recursive: true, force: true })
  }
}
/** Inputs for one target-native authenticated payload build. */
export interface BuildOptions { target: string; nodeArchive: string; headersArchive: string; checksums: string; output: string; sandboxRunner?: string; sandboxProbe?: SandboxProbe; parentNetworkNamespace?: string; toolchain: NativeToolchainOptions; toolProbe?: ToolProbe; nativeBuildRunner?: NativeBuildRunner; rustGateRunner?: RustGateRunner }

/** Requires Rust activation acceptance before creating the final payload path. */
export function publishValidatedPayload(bytes: Uint8Array, target: string, work: string, output: string, runner?: RustGateRunner): void {
  runRustPayloadGate(bytes, target, work, runner)
  mkdirSync(dirname(output), { recursive: true })
  publishExclusive(output, bytes)
}

/** Builds one locked, verified, deterministic DurinDoor payload on its native target runner. */
export function buildPayload(options: BuildOptions): { archive: string; sha256: string } {
  const manifests = join(SCRIPT_ROOT, 'apps/melon-desktop/runtime/durindoor')
  validateLockedManifests(manifests)
  const spec = targetSpec(options.target, options.nodeArchive)
  const pin = RUNTIME_PINS.durindoor.nodeArchives[options.target]!
  verifyChecksum(options.nodeArchive, options.checksums, pin.filename, pin.sha256)
  const headersPin = RUNTIME_PINS.durindoor.nodeHeaders
  if (basename(options.headersArchive) !== headersPin.filename) throw new Error('Node headers archive does not match runtime pins')
  verifyChecksum(options.headersArchive, options.checksums, headersPin.filename, headersPin.sha256)
  const sandboxPrefix = nativeSandboxPrefix(options.target, options.sandboxRunner, undefined, options.sandboxProbe, options.parentNetworkNamespace)
  const toolchain = resolveNativeToolchain(options.toolchain, options.toolProbe)
  const work = mkdtempSync(join(tmpdir(), 'melon-durindoor-build-'))
  const dataDir = join(work, 'data')
  try {
    const cliProject = join(work, 'cli')
    const seedProject = join(work, 'seed')
    const buildProject = join(work, 'seed-build')
    const nodeRoot = extractNodeRuntime(options.nodeArchive, join(work, 'node'), spec, dataDir)
    const headersRoot = extractNodeHeaders(options.headersArchive, join(work, 'headers'), dataDir)
    mkdirSync(cliProject); mkdirSync(seedProject); mkdirSync(buildProject); mkdirSync(dataDir, { recursive: true })
    for (const name of ['package.json', 'package-lock.json']) cpSync(join(manifests, name), join(cliProject, name))
    for (const name of ['package.json', 'package-lock.json']) cpSync(join(manifests, 'runtime-seed', name), join(seedProject, name))
    for (const name of ['package.json', 'package-lock.json']) cpSync(join(manifests, 'runtime-seed-build', name), join(buildProject, name))
    runNpm(nodeRoot, spec, ['ci', '--ignore-scripts', '--omit=dev', '--no-audit', '--no-fund'], cliProject, dataDir)
    runNpm(nodeRoot, spec, ['ci', '--ignore-scripts', '--omit=dev', '--no-audit', '--no-fund'], seedProject, dataDir)
    runNpm(nodeRoot, spec, ['ci', '--ignore-scripts', '--omit=dev', '--no-audit', '--no-fund'], buildProject, dataDir)
    buildNativeSeed(nodeRoot, headersRoot, buildProject, spec, seedProject, dataDir, sandboxPrefix, toolchain, options.nativeBuildRunner)
    if (spec.tray === undefined) rmSync(join(seedProject, 'node_modules/systray2'), { recursive: true, force: true })
    else {
      const tray = join(seedProject, 'node_modules/systray2/traybin', spec.tray)
      if (!existsSync(tray)) throw new Error(`systray2 lacks ${spec.tray} on native runner`)
      chmodSync(tray, 0o755)
    }
    const entries: PayloadEntry[] = [

      { path: `bin/${options.target.includes('windows') ? 'node.exe' : 'node'}`, data: readFileSync(join(nodeRoot, spec.nodePath)), mode: options.target.includes('windows') ? 0o644 : 0o755 },
      ...collectFiles(join(cliProject, 'node_modules'), 'app/node_modules', path => path === '.bin' || path.startsWith('.bin/')),
      ...collectFiles(join(seedProject, 'node_modules'), 'runtime-seed/node_modules', path => path === '.bin' || path.startsWith('.bin/')),
      { path: 'licenses/durindoor-LICENSE', data: readFileSync(join(cliProject, 'node_modules/durindoor/LICENSE')), mode: 0o644 },
      { path: 'licenses/node-LICENSE', data: readFileSync(join(nodeRoot, 'LICENSE')), mode: 0o644 },
      { path: 'runtime-seed/package.json', data: readFileSync(join(seedProject, 'package.json')), mode: 0o644 },
      { path: 'runtime-seed/package-lock.json', data: readFileSync(join(seedProject, 'package-lock.json')), mode: 0o644 },
    ]
    for (const entry of entries.filter(entry => /(^|\/)LICENSE(?:\.|$)/i.test(entry.path) && !entry.path.startsWith('licenses/'))) entries.push({ ...entry, path: `licenses/${entry.path.replaceAll('/', '__')}` })
    entries.push(thirdPartyNotices(entries))
    const nodeAbi = runStdout(join(nodeRoot, spec.nodePath), ['-p', 'process.versions.modules'], work, dataDir).trim()
    if (!/^\d+$/.test(nodeAbi)) throw new Error('verified Node did not report a native module ABI')
    const licenseHashes = {
      durindoor: createHash('sha256').update(entries.find(entry => entry.path === 'licenses/durindoor-LICENSE')!.data).digest('hex'),
      node: createHash('sha256').update(entries.find(entry => entry.path === 'licenses/node-LICENSE')!.data).digest('hex'),
      notices: createHash('sha256').update(entries.find(entry => entry.path === 'licenses/THIRD_PARTY_NOTICES.json')!.data).digest('hex'),
    }
    const descriptor = { schemaVersion: 1, target: options.target, durindoorVersion: DURINDOOR_VERSION, nodeVersion: VERSION, nodeAbi, toolchain, cli: 'app/node_modules/durindoor/cli.js', node: `bin/${options.target.includes('windows') ? 'node.exe' : 'node'}`, runtimeSeedPath: 'runtime-seed', managedLaunchReady: false, licenses: { durindoor: 'licenses/durindoor-LICENSE', node: 'licenses/node-LICENSE', notices: 'licenses/THIRD_PARTY_NOTICES.json', sha256: licenseHashes } }
    entries.push({ path: 'payload.json', data: Buffer.from(`${JSON.stringify(descriptor, null, 2)}\n`), mode: 0o644 })
    validatePayloadEntries(entries, options.target)
    const bytes = canonicalZip(entries)
    const staging = join(work, 'offline')
    mkdirSync(staging)
    for (const entry of entries) { const path = join(staging, entry.path); mkdirSync(dirname(path), { recursive: true }); writeFileSync(path, entry.data, { mode: entry.mode }) }
    validateOffline(staging, join(work, 'offline-data'), options.target, sandboxPrefix)
    const corruptStaging = join(work, 'offline-corrupt')
    cpSync(staging, corruptStaging, { recursive: true })
    assertNativeLoadGateRejectsCorruption(corruptStaging, join(work, 'offline-corrupt-data'), options.target, sandboxPrefix)
    publishValidatedPayload(bytes, options.target, work, options.output, options.rustGateRunner)
    return { archive: options.output, sha256: createHash('sha256').update(bytes).digest('hex') }
  } finally {
    rmSync(work, { recursive: true, force: true })
  }
}

function main(): void {
  const args = process.argv.slice(2)
  const value = (flag: string) => { const index = args.indexOf(flag); if (index < 0 || args[index + 1] === undefined) throw new Error(`missing ${flag}`); return args[index + 1] }
  const toolchain = { directories: value('--toolchain-dir').split(delimiter).map(directory => resolve(directory)), python: resolve(value('--python')), cc: resolve(value('--cc')), cxx: resolve(value('--cxx')) }
  const result = buildPayload({ target: value('--target'), nodeArchive: resolve(value('--node-archive')), headersArchive: resolve(value('--headers-archive')), checksums: resolve(value('--checksums')), output: resolve(value('--output')), sandboxRunner: resolve(value('--sandbox-runner')), toolchain })
  process.stdout.write(`${JSON.stringify(result)}\n`)
}

if (process.argv[1] !== undefined && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main()
