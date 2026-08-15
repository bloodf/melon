import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { createHash } from 'node:crypto'
import { spawnSync } from 'node:child_process'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import {
  canonicalZip,
  inspectZip,
  thirdPartyNotices,
  targetSpec,
  validateLockedManifests,
  validatePayloadEntries,
  verifyChecksum,
  type PayloadEntry,
} from './build-durindoor-payload.mts'

const roots: string[] = []
const root = () => { const path = mkdtempSync(join(tmpdir(), 'melon-payload-test-')); roots.push(path); return path }
afterEach(() => { for (const path of roots.splice(0)) rmSync(path, { recursive: true, force: true }) })

const elf = Uint8Array.from(Array(24).fill(0)); elf.set([0x7f, 0x45, 0x4c, 0x46]); elf[18] = 0x3e
const machX64 = Uint8Array.from([0xcf, 0xfa, 0xed, 0xfe, 7, 0, 0, 1, 0])
const machArm64 = Uint8Array.from([0xcf, 0xfa, 0xed, 0xfe, 12, 0, 0, 1, 0])
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
  it('emits byte-identical sorted regular-file entries with fixed timestamps and modes', () => {
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

  it.each([
    ['missing CLI', (entries: PayloadEntry[]) => entries.filter(entry => !entry.path.endsWith('/cli.js'))],
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
    expect(() => verifyChecksum(archive, checksums, 'node-v20.20.2-linux-x64.tar.gz')).toThrow(/checksum mismatch/i)
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

  it('marks payload as unusable for managed launch until Rust installs the runtime seed', () => {
    const descriptor = JSON.parse(Buffer.from(fixture().find(entry => entry.path === 'payload.json')!.data).toString()) as Record<string, unknown>
    expect(descriptor.managedLaunchReady).not.toBe(true)
    const controller = readFileSync(join(process.cwd(), 'apps/melon-desktop/src-tauri/src/controller.rs'), 'utf8')
    expect(controller).not.toContain('runtimeSeedPath')
  })

  it('passes emitted fixture through Rust activation and preserves executable modes', () => {
    if (process.platform === 'win32') return
    const work = root()
    const cache = join(work, 'cache')
    const final = join(work, 'final', 'x86_64-unknown-linux-gnu')
    mkdirSync(join(work, 'final'), { recursive: true, mode: 0o700 })
    mkdirSync(cache, { recursive: true, mode: 0o700 })
    const archive = join(cache, 'fixture.zip')
    const bytes = canonicalZip(fixture())
    writeFileSync(archive, bytes)
    const sha256 = createHash('sha256').update(bytes).digest('hex')
    const descriptor = join(work, 'fixture.json')
    writeFileSync(descriptor, JSON.stringify({ root: work, cache, archive: 'fixture.zip', targetId: 'x86_64-unknown-linux-gnu', sha256, finalDir: final }))
    const result = spawnSync('cargo', ['test', '--manifest-path', 'apps/melon-desktop/src-tauri/Cargo.toml', 'builder_payload_fixture_is_accepted', '--', '--ignored', '--nocapture'], {
      cwd: process.cwd(), env: { ...process.env, MELON_PAYLOAD_FIXTURE: descriptor }, encoding: 'utf8',
    })
    expect(result.status, `${result.stdout}\n${result.stderr}`).toBe(0)
    expect(result.stdout + result.stderr).toContain('builder_payload_fixture_is_accepted')
    expect(readFileSync(join(final, 'bin/node')).subarray(0, 4)).toEqual(Buffer.from(elf.subarray(0, 4)))
  })
})
