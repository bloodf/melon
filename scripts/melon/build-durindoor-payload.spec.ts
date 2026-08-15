import { cpSync, existsSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { createHash } from 'node:crypto'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import {
  buildNativeSeed,
  buildPayload,
  canonicalZip,
  inspectZip,
  nativeSandboxPrefix,
  preflightNodeArchive,
  publishExclusive,
  preflightNodeHeadersArchive,
  publishValidatedPayload,
  thirdPartyNotices,
  targetSpec,
  validateLockedManifests,
  validateRustGateOutput,
  validateNativeBuildLog,
  validatePayloadEntries,
  verifyChecksum,
  validateCanonicalMetadata,
  type PayloadEntry,
} from './build-durindoor-payload.mts'

const roots: string[] = []
const root = () => { const path = mkdtempSync(join(tmpdir(), 'melon-payload-test-')); roots.push(path); return path }
afterEach(() => { for (const path of roots.splice(0)) rmSync(path, { recursive: true, force: true }) })

const elf = Uint8Array.from(Array(24).fill(0)); elf.set([0x7f, 0x45, 0x4c, 0x46]); elf[18] = 0x3e
const machX64 = Uint8Array.from([0xcf, 0xfa, 0xed, 0xfe, 7, 0, 0, 1, 0])
const machArm64 = Uint8Array.from([0xcf, 0xfa, 0xed, 0xfe, 12, 0, 0, 1, 0])

function tarLink(type: '1' | '2', name: string, linkName: string): Buffer {
  const header = Buffer.alloc(512)
  header.write(name, 0, 100, 'utf8')
  header.write('0000777\x00', 100, 'ascii'); header.write('0000000\x00', 108, 'ascii'); header.write('0000000\x00', 116, 'ascii')
  header.write('00000000000\x00', 124, 'ascii'); header[156] = type.charCodeAt(0)
  header.write(linkName, 157, 100, 'utf8')
  header.fill(0x20, 148, 156)
  let sum = 0; for (const byte of header.subarray(0, 512)) sum += byte
  header.write(`${sum.toString(8).padStart(6, '0')}\0 `, 148, 'ascii')
  return header
}
function tarRegular(name: string, data: Buffer): Buffer {
  const header = Buffer.alloc(512)
  header.write(name, 0, 100, 'utf8')
  header.write('0000644\x00', 100, 'ascii'); header.write('0000000\x00', 108, 'ascii'); header.write('0000000\x00', 116, 'ascii')
  header.write(`${data.length.toString(8).padStart(11, '0')}\x00`, 124, 'ascii'); header[156] = 0x30
  header.fill(0x20, 148, 156)
  let sum = 0; for (const byte of header) sum += byte
  header.write(`${sum.toString(8).padStart(6, '0')}\0 `, 148, 'ascii')
  const padding = Buffer.alloc((512 - (data.length % 512)) % 512)
  return Buffer.concat([header, data, padding])
}
const pe = Uint8Array.from(Array(80).fill(0)); pe.set([0x4d, 0x5a]); pe[0x3c] = 64; pe.set([0x50, 0x45, 0, 0, 0x64, 0x86], 64)
const wasm = Uint8Array.from([0, 0x61, 0x73, 0x6d, 1])
const text = (value: string) => new TextEncoder().encode(value)
function fixture(target = 'x86_64-unknown-linux-gnu'): PayloadEntry[] {
  const unix = !target.includes('windows')
  const entries: PayloadEntry[] = [
    { path: unix ? 'bin/node' : 'bin/node.exe', data: target.includes('apple') ? (target.startsWith('aarch64') ? machArm64 : machX64) : (unix ? elf : pe), mode: unix ? 0o755 : 0o644 },
    { path: 'app/node_modules/durindoor/cli.js', data: text('#!/usr/bin/env node\n'), mode: 0o644 },
    { path: 'app/node_modules/durindoor/package.json', data: text('{"name":"durindoor","version":"3.15.2","license":"MIT"}'), mode: 0o644 },
    { path: 'app/node_modules/durindoor/LICENSE', data: text('MIT'), mode: 0o644 },
    { path: 'runtime-seed/node_modules/sql.js/package.json', data: text('{"name":"sql.js","version":"1.14.1","license":"MIT"}'), mode: 0o644 },
    { path: 'runtime-seed/node_modules/sql.js/LICENSE', data: text('MIT sql.js'), mode: 0o644 },
    { path: 'runtime-seed/node_modules/sql.js/dist/sql-wasm.wasm', data: wasm, mode: 0o644 },
    { path: 'runtime-seed/node_modules/better-sqlite3/package.json', data: text('{"name":"better-sqlite3","version":"12.6.2","license":"MIT"}'), mode: 0o644 },
    { path: 'runtime-seed/node_modules/better-sqlite3/LICENSE', data: text('MIT better-sqlite3'), mode: 0o644 },
    { path: 'runtime-seed/node_modules/better-sqlite3/build/Release/better_sqlite3.node', data: target.includes('apple') ? (target.startsWith('aarch64') ? machArm64 : machX64) : (unix ? elf : pe), mode: 0o644 },
    ...(target.includes('windows') ? [] : [
      { path: 'runtime-seed/node_modules/systray2/package.json', data: text('{"name":"systray2","version":"2.1.4","license":"MIT"}'), mode: 0o644 },
      { path: 'runtime-seed/node_modules/systray2/LICENSE', data: text('MIT systray2'), mode: 0o644 },
      { path: `runtime-seed/node_modules/systray2/traybin/${target.includes('apple') ? 'tray_darwin_release' : 'tray_linux_release'}`, data: target.includes('apple') ? (target.startsWith('aarch64') ? machArm64 : machX64) : elf, mode: 0o755 },
    ]),
    { path: 'licenses/node-LICENSE', data: text('Node license'), mode: 0o644 },
    { path: 'licenses/durindoor-LICENSE', data: text('MIT'), mode: 0o644 },
    { path: 'payload.json', data: text(JSON.stringify({ schemaVersion: 1, target, durindoorVersion: '3.15.2', nodeVersion: '20.20.2', node: unix ? 'bin/node' : 'bin/node.exe', runtimeSeedPath: 'runtime-seed', managedLaunchReady: false })), mode: 0o644 },
  ]
  entries.push(thirdPartyNotices(entries))
  return entries
}

describe('canonical DurinDoor payload', () => {
  it('keeps the canonical serializer byte-stable for synthetic entries', () => {
    const entries = fixture()
    const first = canonicalZip(entries)
    const second = canonicalZip([...entries].reverse())
    expect(Buffer.compare(first, second)).toBe(0)
    const inspected = inspectZip(first)
    expect(inspected.map(entry => entry.path)).toEqual([...entries].map(entry => entry.path).sort())
    expect(inspected.every(entry => entry.type === 'file' && entry.modified === '1980-01-01T00:00:00.000Z')).toBe(true)
    expect(inspected.find(entry => entry.path === 'bin/node')?.mode).toBe(0o755)
    expect(inspected.find(entry => entry.path.includes('/traybin/'))?.mode).toBe(0o755)
    expect(inspected.some(entry => entry.path.includes('/.bin/'))).toBe(false)
  })

  it.each([
    ['traversal', [{ path: '../escape', data: text('x'), mode: 0o644 }]],
    ['absolute path', [{ path: '/escape', data: text('x'), mode: 0o644 }]],
    ['symlink', [{ path: 'link', data: text('target'), mode: 0o777, type: 'symlink' as const }]],
    ['npm shim', [{ path: 'app/node_modules/.bin/durindoor', data: text('x'), mode: 0o755 }]],
    ['secret path', [{ path: '.env', data: text('SECRET=x'), mode: 0o600 }]],
    ['user path', [{ path: 'home/user/.9router/data', data: text('x'), mode: 0o600 }]],
  ])('rejects %s entries', (_name, bad) => {
    expect(() => canonicalZip(bad)).toThrow()
  })

  it('rejects exact and nested .bin path segments', () => {
    for (const path of ['app/node_modules/.bin', 'app/node_modules/.bin/durindoor', 'nested/.bin/tool']) {
      expect(() => canonicalZip([{ path, data: text('x'), mode: 0o644 }])).toThrow(/forbidden payload path/)
    }
  })

  it('enforces Rust entry and aggregate size limits without allocating payload bytes', () => {
    expect(() => validateCanonicalMetadata(Array.from({ length: 16_385 }, (_, index) => ({ path: `f${index}`, size: 0 })))).toThrow(/16384/)
    expect(() => validateCanonicalMetadata([{ path: 'one', size: 0xffff_ffff }, { path: 'two', size: 2 }])).toThrow(/4 GiB|UInt32/)
  })

  it('publishes through an exclusive claim and removes only its failed claim', () => {
    const work = root()
    const output = join(work, 'payload.zip')
    expect(() => publishExclusive(output, text('ours'), () => { throw new Error('fail after claim') })).toThrow(/fail after claim/)
    expect(existsSync(output)).toBe(false)
    writeFileSync(output, 'racer')
    expect(() => publishExclusive(output, text('ours'))).toThrow()
    expect(readFileSync(output, 'utf8')).toBe('racer')
  })

  it('reports parent-directory durability failures after publication', () => {
    const work = root()
    const output = join(work, 'payload.zip')
    expect(() => publishExclusive(output, text('ours'), undefined, undefined, () => { throw new Error('directory fsync failed') })).toThrow(/directory fsync failed/)
    expect(readFileSync(output, 'utf8')).toBe('ours')
  })

  it.each([['symlink', '2'], ['hardlink', '1']] as const)('structurally rejects selected %s entries with spaces', (_label, type) => {
    const work = root()
    const archive = join(work, 'node links.tar')
    const selected = 'node-v20.20.2-linux-x64/bin/node with space'
    writeFileSync(archive, tarLink(type, selected, '../target with space'))
    expect(() => preflightNodeArchive(archive, [selected])).toThrow(/link|special/i)
  })

  it('ignores a non-selected official npm symlink and never selects it', () => {
    const work = root()
    const archive = join(work, 'official links.tar')
    const binNode = 'node-v20.20.2-linux-x64/bin/node'; const binNpm = 'node-v20.20.2-linux-x64/bin/npm'
    const nodeHeader = Buffer.alloc(512); nodeHeader.write(binNode, 0, 100, 'utf8')
    nodeHeader.write('0000777\x00', 100, 'ascii'); nodeHeader.write('0000000\x00', 108, 'ascii'); nodeHeader.write('0000000\x00', 116, 'ascii')
    nodeHeader.write('00000000004\x00', 124, 'ascii'); nodeHeader[156] = 0x30
    let sum = 0; for (const b of nodeHeader.subarray(0, 512)) sum += b
    nodeHeader.write(`${sum.toString(8).padStart(6, '0')}\0 `, 148, 'ascii')
    const npmHeader = Buffer.alloc(512); npmHeader.write(binNpm, 0, 100, 'utf8')
    npmHeader.write('0000777\x00', 100, 'ascii'); npmHeader.write('0000000\x00', 108, 'ascii'); npmHeader.write('0000000\x00', 116, 'ascii')
    npmHeader.write('00000000000\x00', 124, 'ascii'); npmHeader[156] = 0x32
    npmHeader.write('../lib/node_modules/npm/bin/npm-cli.js', 157, 100, 'utf8')
    sum = 0; for (const b of npmHeader.subarray(0, 512)) sum += b
    npmHeader.write(`${sum.toString(8).padStart(6, '0')}\0 `, 148, 'ascii')
    writeFileSync(archive, Buffer.concat([nodeHeader, Buffer.from('node'), npmHeader]))
    // Select only bin/node; bin/npm is a sibling, not selected
    expect(preflightNodeArchive(archive, [binNode])).toHaveLength(1)
  })
  it('rejects duplicate regular file entries sharing the same selected name', () => {
    const archive = join(root(), 'node duplicate.tar')
    const selected = 'node-v20.20.2-linux-x64/LICENSE'
    const record = tarRegular(selected, Buffer.from('ISC\n'))
    writeFileSync(archive, Buffer.concat([record, record, Buffer.alloc(1024)]))
    expect(() => preflightNodeArchive(archive, [selected])).toThrow(/duplicate/)
  })
  it.each([
    ['missing license', (entries: PayloadEntry[]) => entries.filter(entry => !entry.path.startsWith('licenses/'))],
    ['missing WASM', (entries: PayloadEntry[]) => entries.filter(entry => !entry.path.endsWith('sql-wasm.wasm'))],
    ['bad native magic', (entries: PayloadEntry[]) => entries.map(entry => entry.path.endsWith('.node') ? { ...entry, data: text('bad') } : entry)],
    ['missing tray', (entries: PayloadEntry[]) => entries.filter(entry => !entry.path.includes('systray2'))],
  ])('rejects %s', (_name, mutate) => {
    expect(() => validatePayloadEntries(mutate(fixture()), 'x86_64-unknown-linux-gnu')).toThrow()
  })

  it.each([
    ['missing Node', (entries: PayloadEntry[]) => entries.filter(entry => entry.path !== 'bin/node'), 'x86_64-unknown-linux-gnu'],
    ['bad Node magic', (entries: PayloadEntry[]) => entries.map(entry => entry.path === 'bin/node' ? { ...entry, data: text('bad') } : entry), 'x86_64-unknown-linux-gnu'],
    ['non-executable Node', (entries: PayloadEntry[]) => entries.map(entry => entry.path === 'bin/node' ? { ...entry, mode: 0o644 } : entry), 'x86_64-unknown-linux-gnu'],
    ['wrong macOS architecture', (entries: PayloadEntry[]) => entries.map(entry => entry.path === 'bin/node' ? { ...entry, data: machArm64 } : entry), 'x86_64-apple-darwin'],
  ])('rejects %s', (_name, mutate, target) => {
    const source = target === 'x86_64-apple-darwin' ? fixture(target) : fixture()
    expect(() => validatePayloadEntries(mutate(source), target)).toThrow(/Node|architecture|executable/i)
  })

  it('rejects tray packages on Windows and the wrong Node archive target', () => {
    const windows = fixture('x86_64-pc-windows-msvc')
    expect(() => validatePayloadEntries([...windows, { path: 'runtime-seed/node_modules/systray2/package.json', data: text('{}'), mode: 0o644 }], 'x86_64-pc-windows-msvc')).toThrow(/systray/i)
    expect(() => targetSpec('x86_64-unknown-linux-gnu', 'node-v20.20.2-win-x64.zip')).toThrow(/target/i)
  })

  it('rejects a Node archive checksum mismatch', () => {
    const work = root()
    const archive = join(work, 'node-v20.20.2-linux-x64.tar.gz')
    const checksums = join(work, 'SHASUMS256.txt')
    writeFileSync(archive, 'wrong bytes')
    writeFileSync(checksums, `${'0'.repeat(64)}  node-v20.20.2-linux-x64.tar.gz\n`)
    expect(() => verifyChecksum(archive, checksums, 'node-v20.20.2-linux-x64.tar.gz', '19e56f0825510207dd904f087fe52faa0a4eb6b2aab5f0ea7a33830d04888b8b')).toThrow(/pinned checksum mismatch|archive checksum mismatch/i)
  })

  it('rejects a forged archive even when its forged checksum file agrees', () => {
    const work = root()
    const archive = join(work, 'node-v20.20.2-linux-x64.tar.gz')
    const checksums = join(work, 'SHASUMS256.txt')
    const forged = Buffer.from('attacker-controlled archive')
    const forgedHash = createHash('sha256').update(forged).digest('hex')
    writeFileSync(archive, forged)
    writeFileSync(checksums, `${forgedHash}  node-v20.20.2-linux-x64.tar.gz\n`)
    expect(() => verifyChecksum(archive, checksums, 'node-v20.20.2-linux-x64.tar.gz', '19e56f0825510207dd904f087fe52faa0a4eb6b2aab5f0ea7a33830d04888b8b')).toThrow(/pinned checksum mismatch/i)
  })

  it('requires a positively probed Linux network namespace sandbox', () => {
    expect(() => nativeSandboxPrefix('x86_64-unknown-linux-gnu', undefined, { platform: 'linux', arch: 'x64' }, () => 'net:[2]', 'net:[1]')).toThrow(/sandbox-runner/i)
    expect(nativeSandboxPrefix('x86_64-unknown-linux-gnu', '/usr/bin/unshare', { platform: 'linux', arch: 'x64' }, () => 'net:[2]', 'net:[1]')).toEqual(['/usr/bin/unshare', '--user', '--map-root-user', '--net', '--'])
    expect(() => nativeSandboxPrefix('x86_64-unknown-linux-gnu', '/usr/bin/unshare', { platform: 'linux', arch: 'x64' }, () => 'net:[1]', 'net:[1]')).toThrow(/did not isolate network/i)
  })

  it('fails closed on cross-target and unsupported native sandbox builds', () => {
    expect(() => nativeSandboxPrefix('aarch64-apple-darwin', '/usr/bin/unshare', { platform: 'linux', arch: 'x64' }, () => 'net:[2]', 'net:[1]')).toThrow(/native target/i)
    expect(() => nativeSandboxPrefix('x86_64-apple-darwin', '/usr/bin/sandbox-exec', { platform: 'darwin', arch: 'x64' }, () => '', '')).toThrow(/not implemented/i)
    expect(() => nativeSandboxPrefix('x86_64-pc-windows-msvc', 'sandbox.exe', { platform: 'win32', arch: 'x64' }, () => '', '')).toThrow(/not implemented/i)
  })

  it('commits exact official Node archives and digests for every release target', () => {
    const pins = JSON.parse(readFileSync(join(process.cwd(), 'apps/melon-desktop/runtime/runtime-pins.json'), 'utf8')) as { durindoor: { nodeArchives: Record<string, { filename: string; sha256: string }> } }
    expect(pins.durindoor.nodeArchives).toEqual({
      'x86_64-unknown-linux-gnu': { filename: 'node-v20.20.2-linux-x64.tar.gz', sha256: '19e56f0825510207dd904f087fe52faa0a4eb6b2aab5f0ea7a33830d04888b8b' },
      'x86_64-apple-darwin': { filename: 'node-v20.20.2-darwin-x64.tar.gz', sha256: '8be6f5e4bb128c82774f8a0b8d7a1cc1365a7977d9657cece0ca647b3fe04e61' },
      'aarch64-apple-darwin': { filename: 'node-v20.20.2-darwin-arm64.tar.gz', sha256: '466e05f3477c20dfb723054dfebffe55bc74660ee77f612166fca121dacb65b6' },
      'x86_64-pc-windows-msvc': { filename: 'node-v20.20.2-win-x64.zip', sha256: 'dc3700fdd57a63eedb8fd7e3c7baaa32e6a740a1b904167ff4204bc68ed8bf77' },
    })
  })

  it('commits the exact official shared Node headers archive and digest', () => {
    const pins = JSON.parse(readFileSync(join(process.cwd(), 'apps/melon-desktop/runtime/runtime-pins.json'), 'utf8')) as { durindoor: { nodeHeaders: { filename: string; sha256: string } } }
    expect(pins.durindoor.nodeHeaders).toEqual({
      filename: 'node-v20.20.2-headers.tar.gz',
      sha256: '6de0e836efa9f32512e61db3dfd08b3d97a015b7e828d1a5efdf281a56a692d9',
    })
  })

  it('rejects missing, linked, and traversing Node headers entries', () => {
    const work = root()
    const required = 'node-v20.20.2/include/node/node.h'
    const common = 'node-v20.20.2/include/node/common.gypi'
    const config = 'node-v20.20.2/include/node/config.gypi'
    const missing = join(work, 'missing.tar')
    writeFileSync(missing, Buffer.concat([tarRegular(required, Buffer.from('node')), tarRegular(common, Buffer.from('common'))]))
    expect(() => preflightNodeHeadersArchive(missing)).toThrow(/config\.gypi/i)
    const linked = join(work, 'linked.tar')
    writeFileSync(linked, Buffer.concat([tarRegular(required, Buffer.from('node')), tarRegular(common, Buffer.from('common')), tarLink('2', config, '../config')]))
    expect(() => preflightNodeHeadersArchive(linked)).toThrow(/link|special/i)
    const traversal = join(work, 'traversal.tar')
    writeFileSync(traversal, tarRegular('../node-v20.20.2/include/node/node.h', Buffer.from('node')))
    expect(() => preflightNodeHeadersArchive(traversal)).toThrow(/unsafe/i)
  })

  it('rejects forged Node headers even when their forged checksum file agrees', () => {
    const work = root()
    const archive = join(work, 'node-v20.20.2-headers.tar.gz')
    const checksums = join(work, 'SHASUMS256.txt')
    const forged = Buffer.from('forged headers')
    writeFileSync(archive, forged)
    writeFileSync(checksums, `${createHash('sha256').update(forged).digest('hex')}  node-v20.20.2-headers.tar.gz\n`)
    expect(() => verifyChecksum(archive, checksums, 'node-v20.20.2-headers.tar.gz', '6de0e836efa9f32512e61db3dfd08b3d97a015b7e828d1a5efdf281a56a692d9')).toThrow(/pinned checksum mismatch/i)
  })

  it('runs native rebuild under the probed sandbox with verified source headers', () => {
    const work = root()
    const nodeRoot = join(work, 'node')
    const seed = join(work, 'seed')
    for (const path of ['include/node/node.h', 'include/node/common.gypi', 'include/node/config.gypi', 'bin/node', 'lib/node_modules/npm/bin/npm-cli.js']) {
      mkdirSync(join(nodeRoot, path, '..'), { recursive: true })
      writeFileSync(join(nodeRoot, path), path)
    }
    mkdirSync(join(seed, 'node_modules/better-sqlite3'), { recursive: true })
    writeFileSync(join(seed, 'node_modules/better-sqlite3/package.json'), JSON.stringify({ scripts: { install: 'prebuild-install || node-gyp rebuild --release' } }))
    const buildTools = join(work, 'build-tools')
    mkdirSync(join(buildTools, 'node_modules/node-gyp/bin'), { recursive: true })
    writeFileSync(join(buildTools, 'node_modules/node-gyp/bin/node-gyp.js'), 'node-gyp')
    const calls: Array<{ command: string; args: string[]; env: Record<string, string> }> = []
    buildNativeSeed(nodeRoot, nodeRoot, buildTools, targetSpec('x86_64-unknown-linux-gnu'), seed, work, ['/usr/bin/unshare', '--net', '--'], (command, args, _cwd, _dataDir, path, env) => {
      expect(path).toBe('')
      calls.push({ command, args, env }); return 'gyp info ok\n'
    })
    expect(calls).toHaveLength(1)
    expect(calls[0]?.command).toBe('/usr/bin/unshare')
    expect(calls[0]?.args.slice(0, 3)).toEqual(['--net', '--', join(nodeRoot, 'bin/node')])
    expect(calls[0]?.args).toContain(join(buildTools, 'node_modules/node-gyp/bin/node-gyp.js'))
    expect(calls[0]?.args).toContain(`--nodedir=${nodeRoot}`)
    expect(calls[0]?.env.npm_config_build_from_source).toBe('true')
    expect(calls[0]?.env.npm_config_nodedir).toBe(nodeRoot)
    expect(() => buildNativeSeed(nodeRoot, nodeRoot, buildTools, targetSpec('x86_64-unknown-linux-gnu'), seed, work, [], () => '')).toThrow(/sandbox/i)
  })

  it('rejects missing verified headers and prebuilt/download build logs', () => {
    const work = root()
    expect(() => buildNativeSeed(work, work, work, targetSpec('x86_64-unknown-linux-gnu'), work, work, ['/usr/bin/unshare', '--net', '--'], () => '')).toThrow(/headers/i)
    for (const log of ['prebuild-install info begin', 'download https://github.com/example/prebuilt.tar.gz', 'using prebuilt binary']) {
      expect(() => validateNativeBuildLog(log)).toThrow(/locked source/i)
    }
  })

  it('requires the Rust gate before publication and leaves no final or gate cache on failure', () => {
    const work = root()
    const output = join(work, 'out', 'payload.zip')
    const bytes = canonicalZip(fixture())
    expect(() => publishValidatedPayload(bytes, 'x86_64-unknown-linux-gnu', work, output, () => { throw new Error('Rust rejected payload') })).toThrow(/Rust rejected payload/)
    expect(existsSync(output)).toBe(false)
    expect(existsSync(join(work, 'rust-gate'))).toBe(false)
  })

  it('rejects a vacuous Rust test filter success', () => {
    expect(() => validateRustGateOutput('test result: ok. 0 passed; 0 failed; 1 filtered out')).toThrow(/did not execute/i)
    expect(() => validateRustGateOutput('test runtime::tests::builder_payload_fixture_is_accepted ... ok')).not.toThrow()
  })


  it.each([
    ['Node license', (entries: PayloadEntry[]) => entries.filter(entry => entry.path !== 'licenses/node-LICENSE')],
    ['DurinDoor license', (entries: PayloadEntry[]) => entries.filter(entry => entry.path !== 'licenses/durindoor-LICENSE')],
    ['dependency notice', (entries: PayloadEntry[]) => [...entries, { path: 'runtime-seed/node_modules/new-dependency/package.json', data: text('{"name":"new-dependency","version":"1.0.0","license":"MIT"}'), mode: 0o644 }, { path: 'runtime-seed/node_modules/new-dependency/LICENSE', data: text('MIT'), mode: 0o644 }]],
    ['notice hash', (entries: PayloadEntry[]) => entries.map(entry => entry.path === 'runtime-seed/node_modules/sql.js/LICENSE' ? { ...entry, data: text('altered') } : entry)],
  ])('rejects missing or altered %s coverage', (_name, mutate) => {
    expect(() => validatePayloadEntries(mutate(fixture()), 'x86_64-unknown-linux-gnu')).toThrow(/license|notice/i)
  })
  it('requires exact committed lock roots and versions', () => {
    expect(() => validateLockedManifests(join(process.cwd(), 'apps/melon-desktop/runtime/durindoor'))).not.toThrow()
  })

  it('rejects a CLI lock whose DurinDoor integrity differs from runtime pins', () => {
    const work = root()
    cpSync(join(process.cwd(), 'apps/melon-desktop/runtime/durindoor'), work, { recursive: true })
    const lockPath = join(work, 'package-lock.json')
    const lock = JSON.parse(readFileSync(lockPath, 'utf8')) as { packages: Record<string, { integrity?: string }> }
    lock.packages['node_modules/durindoor']!.integrity = 'sha512-forged'
    writeFileSync(lockPath, JSON.stringify(lock))
    expect(() => validateLockedManifests(work)).toThrow(/integrity/i)
  })

  it('marks payload as unusable for managed launch until Rust installs the runtime seed', () => {
    const descriptor = JSON.parse(Buffer.from(fixture().find(entry => entry.path === 'payload.json')!.data).toString()) as Record<string, unknown>
    expect(descriptor.managedLaunchReady).not.toBe(true)
    const controller = readFileSync(join(process.cwd(), 'apps/melon-desktop/src-tauri/src/controller.rs'), 'utf8')
    expect(controller).not.toContain('runtimeSeedPath')
  })

  it('runs actual build output through the real Rust gate before publication on a native runner', { timeout: 600_000 }, ({ skip }) => {
    const nodeArchive = process.env.MELON_NODE_ARCHIVE
    const headersArchive = process.env.MELON_NODE_HEADERS_ARCHIVE
    const checksums = process.env.MELON_NODE_CHECKSUMS
    const sandboxRunner = process.env.MELON_SANDBOX_RUNNER
    if (nodeArchive === undefined || headersArchive === undefined || checksums === undefined || sandboxRunner === undefined) {
      skip('requires authenticated Node runtime/headers archives, checksums, and working native sandbox')
      return
    }
    const output = join(root(), 'actual-payload.zip')
    const result = buildPayload({ target: 'x86_64-unknown-linux-gnu', nodeArchive, headersArchive, checksums, sandboxRunner, output })
    expect(result.archive).toBe(output)
    expect(existsSync(output)).toBe(true)
  })
})
