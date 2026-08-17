/** Verify and atomically publish Melon's official Node 24 sidecar. */

import { spawnSync } from 'node:child_process'
import {
  chmodSync,
  copyFileSync,
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  renameSync,
  rmSync,
} from 'node:fs'
import { tmpdir } from 'node:os'
import { basename, dirname, join, relative, resolve } from 'node:path'
import { isEntry } from '../release/process.ts'
import { preflightNodeArchive, verifyChecksum } from './build-durindoor-payload.mts'

const REPO_ROOT = resolve(import.meta.dirname, '../..')
const TARGETS = {
  'x86_64-unknown-linux-gnu': { archiveRoot: 'node-v24.19.0-linux-x64', nodePath: 'bin/node', suffix: '' },
  'x86_64-apple-darwin': { archiveRoot: 'node-v24.19.0-darwin-x64', nodePath: 'bin/node', suffix: '' },
  'aarch64-apple-darwin': { archiveRoot: 'node-v24.19.0-darwin-arm64', nodePath: 'bin/node', suffix: '' },
  'x86_64-pc-windows-msvc': { archiveRoot: 'node-v24.19.0-win-x64', nodePath: 'node.exe', suffix: '.exe' },
} as const

type TargetId = keyof typeof TARGETS

interface RuntimePins {
  harness?: {
    nodeVersion?: unknown
    nodeArchives?: Record<string, { filename?: unknown; sha256?: unknown }>
  }
}

export interface StageNodeSidecarOptions {
  readonly repoRoot?: string
  readonly target: string
  readonly archive: string
  readonly checksums: string
}

export interface StageNodeSidecarResult {
  readonly sidecarPath: string
}

function readPins(repoRoot: string): RuntimePins {
  return JSON.parse(readFileSync(join(repoRoot, 'apps/melon-desktop/runtime/runtime-pins.json'), 'utf8')) as RuntimePins
}

function requireTarget(target: string): TargetId {
  if (!(target in TARGETS)) throw new Error(`Harness sidecar: unsupported target ${target}.`)
  return target as TargetId
}

function extractOfficialNode(archive: string, destination: string, member: string): string {
  const entries = preflightNodeArchive(archive, [member])
  if (!entries.some(entry => entry.name === member && entry.type === 'file')) {
    throw new Error(`Harness sidecar: archive lacks regular file ${member}.`)
  }
  mkdirSync(destination, { recursive: true })
  const listed = spawnSync('tar', [archive.endsWith('.zip') ? '-xf' : '-xzf', archive, '-C', destination, member], {
    cwd: dirname(archive),
    stdio: 'inherit',
    shell: false,
  })
  if (listed.error !== undefined) throw listed.error
  if (listed.status !== 0) throw new Error(`Harness sidecar: tar extract exited with ${String(listed.status)}.`)
  const extracted = join(destination, ...member.split('/'))
  const status = lstatSync(extracted)
  if (status.isSymbolicLink() || !status.isFile()) throw new Error('Harness sidecar: extracted node is not a regular file.')
  return extracted
}

/**
 * Verify an official Node archive against committed pins and publish the sidecar atomically.
 * @param options - Target, archive, official checksums file, optional test repo root.
 */
export function stageNodeSidecar(options: StageNodeSidecarOptions): StageNodeSidecarResult {
  const repoRoot = resolve(options.repoRoot ?? REPO_ROOT)
  const target = requireTarget(options.target)
  const spec = TARGETS[target]
  const pin = readPins(repoRoot).harness?.nodeArchives?.[target]
  if (typeof pin?.filename !== 'string' || typeof pin.sha256 !== 'string') {
    throw new Error(`Harness sidecar: missing official Node pin for ${target}.`)
  }
  if (basename(options.archive) !== pin.filename) {
    throw new Error(`Harness sidecar: archive filename does not match pin ${pin.filename}.`)
  }
  verifyChecksum(options.archive, options.checksums, pin.filename, pin.sha256)

  const binaries = join(repoRoot, 'apps/melon-desktop/src-tauri/binaries')
  mkdirSync(binaries, { recursive: true })
  const sidecarPath = join(binaries, `node-${target}${spec.suffix}`)
  const workspace = mkdtempSync(join(binaries, '.node-stage-'))
  const unpack = mkdtempSync(join(tmpdir(), 'melon-node-unpack-'))
  const candidate = join(workspace, `node${spec.suffix}`)
  try {
    const extracted = extractOfficialNode(options.archive, unpack, `${spec.archiveRoot}/${spec.nodePath}`)
    copyFileSync(extracted, candidate)
    chmodSync(candidate, lstatSync(extracted).mode & 0o777)
    const previous = existsSync(sidecarPath) ? join(workspace, 'previous') : undefined
    if (previous !== undefined) renameSync(sidecarPath, previous)
    try {
      renameSync(candidate, sidecarPath)
    } catch (error) {
      if (previous !== undefined && existsSync(previous)) renameSync(previous, sidecarPath)
      throw error
    }
    return { sidecarPath }
  } finally {
    rmSync(workspace, { recursive: true, force: true })
    rmSync(unpack, { recursive: true, force: true })
  }
}

function flag(name: string): string {
  const index = process.argv.indexOf(name)
  if (index < 0 || process.argv[index + 1] === undefined) throw new Error(`usage: stage-node-sidecar.mts --target <triple> --archive <path> --checksums <path>`)
  return process.argv[index + 1]!
}

function main(): void {
  if (process.argv.length !== 8) throw new Error('usage: stage-node-sidecar.mts --target <triple> --archive <path> --checksums <path>')
  const result = stageNodeSidecar({ target: flag('--target'), archive: flag('--archive'), checksums: flag('--checksums') })
  console.log(`Node sidecar staged at ${relative(REPO_ROOT, result.sidecarPath).replaceAll('\\', '/')}`)
}

if (isEntry(import.meta.url)) main()
