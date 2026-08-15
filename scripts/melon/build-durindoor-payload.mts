#!/usr/bin/env node

import { createHash } from 'node:crypto'
import { deflateRawSync } from 'node:zlib'
import {
  chmodSync, cpSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readdirSync,
  renameSync, rmSync, writeFileSync,
} from 'node:fs'
import { tmpdir } from 'node:os'
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
  archive: string
  archiveRoot: string
  nodePath: string
  npmCli: string
  magic: readonly number[]
  architecture: 'linux-x64' | 'darwin-x64' | 'darwin-arm64' | 'windows-x64'
  tray?: string
}
const VERSION = '20.20.2'
const FIXED_DOS_DATE = 0x0021
const TARGETS: Record<string, Target> = {
  'x86_64-unknown-linux-gnu': { archive: `node-v${VERSION}-linux-x64.tar.gz`, archiveRoot: `node-v${VERSION}-linux-x64`, nodePath: 'bin/node', npmCli: 'lib/node_modules/npm/bin/npm-cli.js', magic: [0x7f, 0x45, 0x4c, 0x46], architecture: 'linux-x64', tray: 'tray_linux_release' },
  'x86_64-apple-darwin': { archive: `node-v${VERSION}-darwin-x64.tar.gz`, archiveRoot: `node-v${VERSION}-darwin-x64`, nodePath: 'bin/node', npmCli: 'lib/node_modules/npm/bin/npm-cli.js', magic: [0xcf, 0xfa, 0xed, 0xfe], architecture: 'darwin-x64', tray: 'tray_darwin_release' },
  'aarch64-apple-darwin': { archive: `node-v${VERSION}-darwin-arm64.tar.gz`, archiveRoot: `node-v${VERSION}-darwin-arm64`, nodePath: 'bin/node', npmCli: 'lib/node_modules/npm/bin/npm-cli.js', magic: [0xcf, 0xfa, 0xed, 0xfe], architecture: 'darwin-arm64', tray: 'tray_darwin_release' },
  'x86_64-pc-windows-msvc': { archive: `node-v${VERSION}-win-x64.zip`, archiveRoot: `node-v${VERSION}-win-x64`, nodePath: 'node.exe', npmCli: 'node_modules/npm/bin/npm-cli.js', magic: [0x4d, 0x5a], architecture: 'windows-x64' },
}
const FORBIDDEN_SEGMENTS = new Set(['.env', '.9router', '.durindoor'])

/** Resolves one supported Rust target and rejects a mismatched official Node archive name. */
export function targetSpec(target: string, archive?: string): Target {
  const spec = TARGETS[target]
  if (spec === undefined) throw new Error(`unsupported payload target: ${target}`)
  if (archive !== undefined && basename(archive) !== spec.archive) throw new Error(`Node archive does not match target ${target}`)
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
  if (parts.some(part => FORBIDDEN_SEGMENTS.has(part)) || normalized.includes('node_modules/.bin/')) throw new Error(`forbidden payload path: ${path}`)
  return normalized
}

/** Creates a canonical ZIP containing regular files only, sorted by UTF-8 path. */
export function canonicalZip(source: PayloadEntry[]): Buffer {
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

/** Verifies committed npm locks pin exact CLI and runtime seed roots. */
export function validateLockedManifests(root: string): void {
  const cli = readJson(join(root, 'package-lock.json'))
  const seed = readJson(join(root, 'runtime-seed/package-lock.json'))
  const cliPackages = cli.packages as Record<string, { version?: string }> | undefined
  const seedPackages = seed.packages as Record<string, { version?: string }> | undefined
  if (cliPackages?.['node_modules/durindoor']?.version !== '3.15.2') throw new Error('DurinDoor lock must pin 3.15.2')
  for (const [name, version] of [['better-sqlite3', '12.6.2'], ['sql.js', '1.14.1'], ['systray2', '2.1.4']]) {
    if (seedPackages?.[`node_modules/${name}`]?.version !== version) throw new Error(`runtime seed lock must pin ${name}@${version}`)
  }
  const lifecyclePackages = Object.entries(seedPackages ?? {}).filter(([, value]) => (value as { hasInstallScript?: boolean }).hasInstallScript).map(([path]) => path)
  if (JSON.stringify(lifecyclePackages) !== JSON.stringify(['node_modules/better-sqlite3'])) throw new Error('runtime seed lifecycle set must contain only better-sqlite3')
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
  if (result.status !== 0) throw new Error(`${command} ${args.join(' ')} failed\n${result.stdout}\n${result.stderr}`)
  return result.stdout
}

function collectFiles(root: string, prefix: string, omit: (path: string) => boolean): PayloadEntry[] {
  const entries: PayloadEntry[] = []
  const visit = (directory: string) => {
    for (const name of readdirSync(directory).sort()) {
      const path = join(directory, name)
      const relativePath = relative(root, path).split(sep).join('/')
      if (omit(relativePath)) continue
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

export function verifyChecksum(archive: string, checksums: string, expectedName: string): void {
  const expected = readFileSync(checksums, 'utf8').split(/\r?\n/).find(line => line.endsWith(`  ${expectedName}`))?.slice(0, 64)
  if (expected === undefined) throw new Error(`official checksum missing for ${expectedName}`)
  const actual = createHash('sha256').update(readFileSync(archive)).digest('hex')
  if (actual !== expected) throw new Error(`Node archive checksum mismatch for ${expectedName}`)
}

function safeArchiveNames(archive: string): string[] {
  const args = archive.endsWith('.zip') ? ['-tf', archive] : ['-tzf', archive]
  const names = run('tar', args, dirname(archive), join(dirname(archive), '.data')).split(/\r?\n/).filter(Boolean)
  for (const name of names) {
    const normalized = name.replaceAll('\\', '/')
    if (isAbsolute(normalized) || normalized.split('/').some(part => part === '..')) throw new Error(`unsafe Node archive entry: ${name}`)
  }
  return names
}

function extractNodeRuntime(archive: string, destination: string, spec: Target, dataDir: string): string {
  const names = safeArchiveNames(archive)
  const required = [`${spec.archiveRoot}/${spec.nodePath}`, `${spec.archiveRoot}/${spec.npmCli}`, `${spec.archiveRoot}/LICENSE`]
  if (required.some(name => !names.includes(name))) throw new Error('Node archive lacks runtime, npm, or license')
  const listingArgs = archive.endsWith('.zip') ? ['-tvf', archive] : ['-tvzf', archive]
  const listing = run('tar', listingArgs, dirname(archive), join(dirname(archive), '.data')).split(/\r?\n/).filter(Boolean)
  const selected = [`${spec.archiveRoot}/${spec.nodePath}`, `${spec.archiveRoot}/${spec.npmCli.split('/bin/')[0]}`, `${spec.archiveRoot}/LICENSE`]
  const selectedNames = new Set<string>()
  for (const line of listing) {
    const name = line.trim().split(/\s+/).at(-1)!
    if (!selected.some(root => name === root || name.startsWith(`${root}/`))) continue
    if (!['-', 'd'].includes(line[0]!)) throw new Error(`unsafe selected Node archive entry type: ${name}`)
    if (selectedNames.has(name)) throw new Error(`duplicate selected Node archive entry: ${name}`)
    selectedNames.add(name)
  }
  mkdirSync(destination, { recursive: true })
  if (archive.endsWith('.zip')) run('tar', ['-xf', archive, '-C', destination, `${spec.archiveRoot}/${spec.nodePath}`, `${spec.archiveRoot}/node_modules/npm`, `${spec.archiveRoot}/LICENSE`], dirname(archive), dataDir)
  else run('tar', ['-xzf', archive, '-C', destination, `${spec.archiveRoot}/${spec.nodePath}`, `${spec.archiveRoot}/${spec.npmCli.split('/bin/')[0]}`, `${spec.archiveRoot}/LICENSE`], dirname(archive), dataDir)
  return join(destination, spec.archiveRoot)
}
function runNpm(nodeRoot: string, spec: Target, args: string[], cwd: string, dataDir: string): void {
  const node = join(nodeRoot, spec.nodePath)
  const npmCli = join(nodeRoot, spec.npmCli)
  run(node, [npmCli, ...args], cwd, dataDir, `${dirname(node)}${delimiter}${process.env.PATH ?? ''}`)
}

function copyRuntimeSeed(seed: string, dataDir: string): void {
  const runtime = join(dataDir, 'runtime')
  mkdirSync(runtime, { recursive: true })
  cpSync(join(seed, 'node_modules'), join(runtime, 'node_modules'), { recursive: true, dereference: true })
  cpSync(join(seed, 'package.json'), join(runtime, 'package.json'))
}

function validateOffline(staging: string, dataDir: string, target: string): void {
  copyRuntimeSeed(join(staging, 'runtime-seed'), dataDir)
  const node = join(staging, 'bin', target.includes('windows') ? 'node.exe' : 'node')
  const cli = join(staging, 'app/node_modules/durindoor/cli.js')
  const emptyPath = join(dataDir, 'empty-path')
  const runtimeModules = join(dataDir, 'runtime/node_modules')
  mkdirSync(emptyPath)
  const guard = join(dataDir, 'offline-guard.cjs')
  writeFileSync(guard, [
    "globalThis.fetch=()=>Promise.reject(new Error('offline network blocked'))",
    "for(const name of ['net','tls','dgram','dns','http','https','http2']){const mod=require(name);for(const key of Object.keys(mod)){if(typeof mod[key]==='function'&&/connect|request|resolve|lookup|createConnection|createSocket/.test(key))mod[key]=()=>{throw new Error('offline network blocked')}}}",
    "const child=require('child_process');for(const key of ['spawn','spawnSync','exec','execSync','execFile','execFileSync','fork'])child[key]=()=>{throw new Error('offline child process blocked')}",
  ].join(';'))
  const offlineEnv = { NODE_PATH: runtimeModules, NODE_OPTIONS: `--require=${guard}` }
  if (!run(node, [cli, '--version'], staging, dataDir, emptyPath, offlineEnv).includes('3.15.2')) throw new Error('offline CLI version failed')
  if (!run(node, [cli, '--help'], staging, dataDir, emptyPath, offlineEnv).includes('--skip-update')) throw new Error('offline CLI help failed')
  const smoke = [
    "const Database=require('better-sqlite3')",
    "const db=new Database(':memory:')",
    "if(db.prepare('select 42 as value').get().value!==42)process.exit(2)",
    'db.close()',
    "const init=require('sql.js')",
    "const wasm=require.resolve('sql.js/dist/sql-wasm.wasm')",
    "init({locateFile:()=>wasm}).then(SQL=>{const db=new SQL.Database();const rows=db.exec('select 42 as value');db.close();if(rows[0].values[0][0]!==42)process.exit(3)}).catch(()=>process.exit(4))",
  ].join(';')
  run(node, ['-e', smoke], staging, dataDir, emptyPath, offlineEnv)
  const sqliteHook = join(staging, 'app/node_modules/durindoor/hooks/sqliteRuntime.js')
  const trayHook = join(staging, 'app/node_modules/durindoor/hooks/trayRuntime.js')
  const bootstrap = target.includes('windows')
    ? `const s=require(${JSON.stringify(sqliteHook)}).ensureSqliteRuntime({silent:true});if(!s.sqlJs||!s.betterSqlite)process.exit(5)`
    : `const s=require(${JSON.stringify(sqliteHook)}).ensureSqliteRuntime({silent:true});const t=require(${JSON.stringify(trayHook)}).ensureTrayRuntime({silent:true});if(!s.sqlJs||!s.betterSqlite||!t.systray)process.exit(5)`
  run(node, ['-e', bootstrap], staging, dataDir, emptyPath, offlineEnv)
}

function assertNativeLoadGateRejectsCorruption(staging: string, dataDir: string, target: string): void {
  const native = join(staging, 'runtime-seed/node_modules/better-sqlite3/build/Release/better_sqlite3.node')
  const magic = targetSpec(target).magic
  writeFileSync(native, Uint8Array.from([...magic, 0, 0, 0, 0]))
  try {
    validateOffline(staging, dataDir, target)
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error)
    if (/better-sqlite3|better_sqlite3\.node|file too short|invalid ELF|not a valid Win32|mach-o/i.test(message)) return
    throw new Error(`wrong-ABI probe failed for an unrelated reason: ${message}`)
  }
  throw new Error('offline native load gate accepted a truncated wrong-ABI module')
}

interface BuildOptions { target: string; nodeArchive: string; checksums: string; output: string }

/** Builds one locked, verified, deterministic DurinDoor payload on its native target runner. */
export function buildPayload(options: BuildOptions): { archive: string; sha256: string } {
  const scriptRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../..')
  const manifests = join(scriptRoot, 'apps/melon-desktop/runtime/durindoor')
  validateLockedManifests(manifests)
  const spec = targetSpec(options.target, options.nodeArchive)
  verifyChecksum(options.nodeArchive, options.checksums, spec.archive)
  const work = mkdtempSync(join(tmpdir(), 'melon-durindoor-build-'))
  const dataDir = join(work, 'data')
  try {
    const cliProject = join(work, 'cli')
    const seedProject = join(work, 'seed')
    const nodeRoot = extractNodeRuntime(options.nodeArchive, join(work, 'node'), spec, dataDir)
    mkdirSync(cliProject); mkdirSync(seedProject); mkdirSync(dataDir, { recursive: true })
    for (const name of ['package.json', 'package-lock.json']) cpSync(join(manifests, name), join(cliProject, name))
    for (const name of ['package.json', 'package-lock.json']) cpSync(join(manifests, 'runtime-seed', name), join(seedProject, name))
    runNpm(nodeRoot, spec, ['ci', '--ignore-scripts', '--omit=dev', '--no-audit', '--no-fund'], cliProject, dataDir)
    runNpm(nodeRoot, spec, ['ci', '--ignore-scripts', '--omit=dev', '--no-audit', '--no-fund'], seedProject, dataDir)
    runNpm(nodeRoot, spec, ['rebuild', 'better-sqlite3', '--foreground-scripts', '--no-audit', '--no-fund'], seedProject, dataDir)
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
    const nodeAbi = run(join(nodeRoot, spec.nodePath), ['-p', 'process.versions.modules'], work, dataDir).trim()
    if (!/^\d+$/.test(nodeAbi)) throw new Error('verified Node did not report a native module ABI')
    const licenseHashes = {
      durindoor: createHash('sha256').update(entries.find(entry => entry.path === 'licenses/durindoor-LICENSE')!.data).digest('hex'),
      node: createHash('sha256').update(entries.find(entry => entry.path === 'licenses/node-LICENSE')!.data).digest('hex'),
      notices: createHash('sha256').update(entries.find(entry => entry.path === 'licenses/THIRD_PARTY_NOTICES.json')!.data).digest('hex'),
    }
    const descriptor = { schemaVersion: 1, target: options.target, durindoorVersion: '3.15.2', nodeVersion: VERSION, nodeAbi, cli: 'app/node_modules/durindoor/cli.js', node: `bin/${options.target.includes('windows') ? 'node.exe' : 'node'}`, runtimeSeedPath: 'runtime-seed', managedLaunchReady: false, licenses: { durindoor: 'licenses/durindoor-LICENSE', node: 'licenses/node-LICENSE', notices: 'licenses/THIRD_PARTY_NOTICES.json', sha256: licenseHashes } }
    entries.push({ path: 'payload.json', data: Buffer.from(`${JSON.stringify(descriptor, null, 2)}\n`), mode: 0o644 })
    validatePayloadEntries(entries, options.target)
    const bytes = canonicalZip(entries)
    const staging = join(work, 'offline')
    mkdirSync(staging)
    for (const entry of entries) { const path = join(staging, entry.path); mkdirSync(dirname(path), { recursive: true }); writeFileSync(path, entry.data, { mode: entry.mode }) }
    validateOffline(staging, join(work, 'offline-data'), options.target)
    const corruptStaging = join(work, 'offline-corrupt')
    cpSync(staging, corruptStaging, { recursive: true })
    assertNativeLoadGateRejectsCorruption(corruptStaging, join(work, 'offline-corrupt-data'), options.target)
    mkdirSync(dirname(options.output), { recursive: true })
    if (existsSync(options.output)) throw new Error(`refusing to replace existing payload: ${options.output}`)
    const temporaryOutput = `${options.output}.part-${process.pid}`
    try {
      writeFileSync(temporaryOutput, bytes, { flag: 'wx' })
      renameSync(temporaryOutput, options.output)
    } finally {
      rmSync(temporaryOutput, { force: true })
    }
    return { archive: options.output, sha256: createHash('sha256').update(bytes).digest('hex') }
  } finally {
    rmSync(work, { recursive: true, force: true })
  }
}

function main(): void {
  const args = process.argv.slice(2)
  const value = (flag: string) => { const index = args.indexOf(flag); if (index < 0 || args[index + 1] === undefined) throw new Error(`missing ${flag}`); return args[index + 1] }
  const result = buildPayload({ target: value('--target'), nodeArchive: resolve(value('--node-archive')), checksums: resolve(value('--checksums')), output: resolve(value('--output')) })
  process.stdout.write(`${JSON.stringify(result)}\n`)
}

if (process.argv[1] !== undefined && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main()
