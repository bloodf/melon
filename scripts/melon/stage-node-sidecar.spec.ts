import { existsSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { createHash } from 'node:crypto'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { gzipSync } from 'node:zlib'
import { afterEach, describe, expect, it } from 'vitest'
import { stageNodeSidecar } from './stage-node-sidecar.mts'

const roots: string[] = []
const temporaryRoot = (): string => {
  const path = mkdtempSync(join(tmpdir(), 'melon-node-sidecar-'))
  roots.push(path)
  return path
}
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }) })

const LINUX = 'x86_64-unknown-linux-gnu'
const FILENAME = 'node-v24.19.0-linux-x64.tar.gz'
const MEMBER = 'node-v24.19.0-linux-x64/bin/node'
const PINNED = JSON.parse(readFileSync(join(process.cwd(), 'apps/melon-desktop/runtime/runtime-pins.json'), 'utf8')) as {
  harness: { nodeVersion: string; nodeArchives: Record<string, { filename: string; sha256: string }> }
}

function tarRegular(name: string, data: Buffer, type = '0'): Buffer {
  const header = Buffer.alloc(512)
  header.write(name, 0, 100, 'utf8')
  header.write('0000755\x00', 100, 'ascii')
  header.write('0000000\x00', 108, 'ascii')
  header.write('0000000\x00', 116, 'ascii')
  header.write(`${data.length.toString(8).padStart(11, '0')}\x00`, 124, 'ascii')
  header[156] = type.charCodeAt(0)
  header.fill(0x20, 148, 156)
  let sum = 0
  for (const byte of header) sum += byte
  header.write(`${sum.toString(8).padStart(6, '0')}\0 `, 148, 'ascii')
  const padding = Buffer.alloc((512 - (data.length % 512)) % 512)
  return Buffer.concat([header, data, padding])
}

function archiveWith(member: string, data: Buffer, type = '0'): { archive: string; checksums: string; sha256: string } {
  const root = temporaryRoot()
  const packed = Buffer.concat([tarRegular(member, data, type), Buffer.alloc(1024)])
  const bytes = gzipSync(packed)
  const archive = join(root, FILENAME)
  writeFileSync(archive, bytes)
  const sha256 = createHash('sha256').update(bytes).digest('hex')
  const checksums = join(root, 'SHASUMS256.txt')
  writeFileSync(checksums, `${sha256}  ${FILENAME}\n`)
  return { archive, checksums, sha256 }
}

function repository(root: string, sha256: string): void {
  mkdirSync(join(root, 'apps/melon-desktop/runtime'), { recursive: true })
  mkdirSync(join(root, 'apps/melon-desktop/src-tauri/binaries'), { recursive: true })
  writeFileSync(join(root, 'apps/melon-desktop/runtime/runtime-pins.json'), `${JSON.stringify({
    schemaVersion: 1,
    harness: {
      nodeVersion: '24.19.0',
      nodeArchives: { [LINUX]: { filename: FILENAME, sha256 } },
    },
  }, null, 2)}\n`)
}

describe('Node sidecar pins', () => {
  it('commits official Node 24 archives for every release target', () => {
    expect(PINNED.harness.nodeVersion).toBe('24.19.0')
    expect(PINNED.harness.nodeArchives).toEqual({
      'x86_64-unknown-linux-gnu': {
        filename: 'node-v24.19.0-linux-x64.tar.gz',
        sha256: 'f625d97cd707df4ff96254916fbc5ff014f09c09effe5a1e0ca8f6d41a8789d4',
      },
      'x86_64-apple-darwin': {
        filename: 'node-v24.19.0-darwin-x64.tar.gz',
        sha256: 'd1b5e999db158c62fe8f7267a4476b035d8bd93b1a605bac24a3f0dd166e3316',
      },
      'aarch64-apple-darwin': {
        filename: 'node-v24.19.0-darwin-arm64.tar.gz',
        sha256: '8294b7aa9b03997481c06babf1e8b270c859358f27da57a11509afe537ac381d',
      },
      'x86_64-pc-windows-msvc': {
        filename: 'node-v24.19.0-win-x64.zip',
        sha256: '57f71ab3652e797d84acddc79c81cc9ff1c6ddb2a1974cdb83f00fee9bff4c73',
      },
    })
  })
})

describe('Node sidecar staging', () => {
  it('rejects a checksum mismatch without replacing a prior sidecar', () => {
    const node = Buffer.from('#!/usr/bin/env node\n')
    const fixture = archiveWith(MEMBER, node)
    const root = temporaryRoot()
    repository(root, fixture.sha256)
    const published = join(root, 'apps/melon-desktop/src-tauri/binaries/node-x86_64-unknown-linux-gnu')
    mkdirSync(dirname(published), { recursive: true })
    writeFileSync(published, 'keep\n')
    writeFileSync(fixture.checksums, `${'0'.repeat(64)}  ${FILENAME}\n`)

    expect(() => stageNodeSidecar({
      repoRoot: root,
      target: LINUX,
      archive: fixture.archive,
      checksums: fixture.checksums,
    })).toThrow(/checksum/)
    expect(readFileSync(published, 'utf8')).toBe('keep\n')
  })

  it('rejects an archive whose basename is not the pinned filename', () => {
    const fixture = archiveWith(MEMBER, Buffer.from('node\n'))
    const root = temporaryRoot()
    repository(root, fixture.sha256)
    const renamed = join(dirname(fixture.archive), 'evil.tar.gz')
    writeFileSync(renamed, readFileSync(fixture.archive))

    expect(() => stageNodeSidecar({
      repoRoot: root,
      target: LINUX,
      archive: renamed,
      checksums: fixture.checksums,
    })).toThrow(/does not match|filename/)
  })

  it('rejects a symbolic link member without replacing a prior sidecar', () => {
    const fixture = archiveWith(MEMBER, Buffer.from('/etc/passwd\n'), '2')
    const root = temporaryRoot()
    repository(root, fixture.sha256)
    const published = join(root, 'apps/melon-desktop/src-tauri/binaries/node-x86_64-unknown-linux-gnu')
    writeFileSync(published, 'keep\n')

    expect(() => stageNodeSidecar({
      repoRoot: root,
      target: LINUX,
      archive: fixture.archive,
      checksums: fixture.checksums,
    })).toThrow(/symbolic link|regular file|link or special/)
    expect(readFileSync(published, 'utf8')).toBe('keep\n')
  })

  it('publishes the official node regular file atomically', () => {
    const node = Buffer.from('#!/usr/bin/env node\nfixture-node\n')
    const fixture = archiveWith(MEMBER, node)
    const root = temporaryRoot()
    repository(root, fixture.sha256)

    const result = stageNodeSidecar({
      repoRoot: root,
      target: LINUX,
      archive: fixture.archive,
      checksums: fixture.checksums,
    })

    expect(readFileSync(result.sidecarPath)).toEqual(node)
    expect(existsSync(join(root, 'apps/melon-desktop/src-tauri/binaries/.node-stage'))).toBe(false)
  })
})
