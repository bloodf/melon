/** Build, validate, and atomically publish Melon's generated Harness runtime. */

import { createHash } from 'node:crypto'
import { spawnSync } from 'node:child_process'
import {
  cpSync,
  existsSync,
  globSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  realpathSync,
  renameSync,
  rmSync,
  writeFileSync,
} from 'node:fs'
import { dirname, isAbsolute, join, posix, relative, resolve, sep } from 'node:path'
import { isEntry } from '../release/process.ts'

const REPO_ROOT = resolve(import.meta.dirname, '../..')
const CLI_PACKAGE = '@deepseek-ai/dsh'
const WEB_PACKAGE = '@deepseek-ai/dsh-web-frontend'
const DESCRIPTOR_NAME = 'melon-harness-runtime.json'
const PUBLICATION_RELATIVE = 'apps/melon-desktop/src-tauri/resources/harness'

interface PackageManifest {
  readonly name?: unknown
  readonly version?: unknown
  readonly bin?: unknown
  readonly dependencies?: unknown
  readonly peerDependencies?: unknown
  readonly peerDependenciesMeta?: unknown
}

/** Fixed subprocess invocation issued by the staging pipeline. */
export interface CommandInvocation {
  readonly command: string
  readonly args: readonly string[]
  readonly cwd: string
  readonly env: NodeJS.ProcessEnv
}

/** Validated facts about one deployed Harness closure. */
export interface ValidatedHarnessClosure {
  readonly packageRoot: string
  readonly packageName: string
  readonly packageVersion: string
  readonly dshBin: string
  readonly webAssets: string
  readonly closure: {
    readonly sha256: string
    readonly fileCount: number
    readonly totalBytes: number
  }
}

/** Non-secret generated descriptor consumed by the desktop runtime. */
export interface HarnessRuntimeDescriptor {
  readonly schemaVersion: 1
  readonly packageRoot: '.'
  readonly packageName: string
  readonly packageVersion: string
  readonly dshBin: string
  readonly webAssets: string
  readonly harnessSourceSha: string
  readonly closure: ValidatedHarnessClosure['closure']
}

/** Result paths from one completed staging publication. */
export interface StageHarnessResult {
  readonly publicationPath: string
  readonly candidatePath: string
  readonly descriptor: HarnessRuntimeDescriptor
}

interface StageHarnessOptions {
  readonly repoRoot?: string
  readonly run?: (invocation: CommandInvocation) => void
}

function readManifest(path: string): PackageManifest {
  let parsed: unknown
  try {
    parsed = JSON.parse(readFileSync(path, 'utf8'))
  } catch (error) {
    throw new Error(`Harness staging: cannot read package manifest ${path}: ${String(error)}`)
  }
  if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) {
    throw new Error(`Harness staging: package manifest ${path} must be a JSON object.`)
  }
  return parsed
}

function requireString(value: unknown, field: string, path: string): string {
  if (typeof value !== 'string' || value.length === 0) {
    throw new Error(`Harness staging: ${path} must declare non-empty ${field}.`)
  }
  return value
}

function isInside(root: string, path: string): boolean {
  const offset = relative(root, path)
  return offset === '' || (!offset.startsWith(`..${sep}`) && offset !== '..' && !isAbsolute(offset))
}

function packagePath(root: string, value: string, field: string): string {
  if (value.includes('\\') || value.includes('\0') || isAbsolute(value) || posix.normalize(value) !== value || value === '.' || value.startsWith('../')) {
    throw new Error(`Harness staging: ${field} must be a normalized relative package path.`)
  }
  const path = resolve(root, ...value.split('/'))
  if (!isInside(root, path)) throw new Error(`Harness staging: ${field} escapes its package root.`)
  return path
}

/**
 * Resolve `dsh` from a staged package manifest without assuming its source entry.
 * @param packageRoot - Absolute staged package directory.
 * @param manifest - Parsed staged package manifest.
 * @returns Normalized package-relative executable path.
 */
export function resolveDshBin(packageRoot: string, manifest: PackageManifest): string {
  if (manifest.bin === null || typeof manifest.bin !== 'object' || Array.isArray(manifest.bin)) {
    throw new Error('Harness staging: package manifest must declare bin.dsh.')
  }
  const value = (manifest.bin as Record<string, unknown>).dsh
  if (typeof value !== 'string' || value.length === 0) throw new Error('Harness staging: package manifest must declare string bin.dsh.')
  const path = packagePath(packageRoot, value, 'bin.dsh')
  if (!existsSync(path) || !lstatSync(path).isFile()) throw new Error(`Harness staging: bin.dsh is not a regular file: ${value}`)
  return value
}

function installedPackageManifest(packageRoot: string, from: string, dependency: string): string | undefined {
  let cursor = from
  for (;;) {
    const candidate = join(cursor, 'node_modules', ...dependency.split('/'), 'package.json')
    if (existsSync(candidate)) return candidate
    if (cursor === packageRoot) return undefined
    const parent = dirname(cursor)
    if (!isInside(packageRoot, parent)) return undefined
    cursor = parent
  }
}

function packageRoots(packageRoot: string): string[] {
  const roots = [packageRoot]
  const visitNodeModules = (nodeModules: string): void => {
    if (!existsSync(nodeModules)) return
    for (const name of readdirSync(nodeModules).sort()) {
      const path = join(nodeModules, name)
      if (name.startsWith('@')) {
        if (!lstatSync(path).isDirectory()) continue
        for (const scoped of readdirSync(path).sort()) {
          const scopedPath = join(path, scoped)
          if (existsSync(join(scopedPath, 'package.json'))) roots.push(scopedPath)
          visitNodeModules(join(scopedPath, 'node_modules'))
        }
      } else {
        if (existsSync(join(path, 'package.json'))) roots.push(path)
        visitNodeModules(join(path, 'node_modules'))
      }
    }
  }
  visitNodeModules(join(packageRoot, 'node_modules'))
  return roots
}

function requiredDependencies(manifest: PackageManifest): string[] {
  const dependencies = new Set<string>()
  if (manifest.dependencies !== null && typeof manifest.dependencies === 'object' && !Array.isArray(manifest.dependencies)) {
    for (const name of Object.keys(manifest.dependencies)) dependencies.add(name)
  }
  if (manifest.peerDependencies !== null && typeof manifest.peerDependencies === 'object' && !Array.isArray(manifest.peerDependencies)) {
    const meta = manifest.peerDependenciesMeta !== null && typeof manifest.peerDependenciesMeta === 'object' && !Array.isArray(manifest.peerDependenciesMeta)
      ? manifest.peerDependenciesMeta as Record<string, unknown>
      : {}
    for (const name of Object.keys(manifest.peerDependencies)) {
      const entry = meta[name]
      if (entry !== null && typeof entry === 'object' && (entry as Record<string, unknown>).optional === true) continue
      dependencies.add(name)
    }
  }
  return [...dependencies].sort()
}

function filesInClosure(root: string): string[] {
  const files: string[] = []
  const visit = (directory: string): void => {
    for (const name of readdirSync(directory).sort()) {
      const path = join(directory, name)
      const status = lstatSync(path)
      const display = relative(root, path).replaceAll('\\', '/')
      if (status.isSymbolicLink()) throw new Error(`Harness staging: staged closure contains symbolic link ${display}.`)
      if (status.isDirectory()) visit(path)
      else if (status.isFile()) files.push(path)
      else throw new Error(`Harness staging: staged closure contains non-regular entry ${display}.`)
    }
  }
  visit(root)
  return files
}

function materializeLinks(root: string): void {
  for (;;) {
    const link = findLink(root)
    if (link === undefined) return
    const source = realpathSync(link)
    const status = lstatSync(source)
    rmSync(link, { recursive: true, force: true })
    cpSync(source, link, { recursive: status.isDirectory(), dereference: true })
  }
}

function findLink(directory: string): string | undefined {
  for (const name of readdirSync(directory).sort()) {
    const path = join(directory, name)
    const status = lstatSync(path)
    if (status.isSymbolicLink()) return path
    if (status.isDirectory()) {
      const nested = findLink(path)
      if (nested !== undefined) return nested
    }
  }
  return undefined
}

function workspacePackages(repoRoot: string): Map<string, string> {
  const packages = new Map<string, string>()
  for (const manifestPath of globSync(['apps/*/package.json', 'packages/*/*/package.json', 'vendor/*/package.json'], { cwd: repoRoot }).sort()) {
    const root = dirname(join(repoRoot, manifestPath))
    const name = readManifest(join(root, 'package.json')).name
    if (typeof name === 'string') packages.set(name, root)
  }
  return packages
}

function restoreWorkspaceDependencies(candidate: string, repoRoot: string): void {
  const workspaces = workspacePackages(repoRoot)
  for (;;) {
    let restored = false
    for (const installedRoot of packageRoots(candidate)) {
      const manifest = readManifest(join(installedRoot, 'package.json'))
      for (const dependency of requiredDependencies(manifest)) {
        if (installedPackageManifest(candidate, installedRoot, dependency) !== undefined) continue
        const source = workspaces.get(dependency)
        if (source === undefined) continue
        const destination = join(candidate, 'node_modules', ...dependency.split('/'))
        const sourceNodeModules = join(source, 'node_modules')
        mkdirSync(dirname(destination), { recursive: true })
        cpSync(source, destination, {
          recursive: true,
          dereference: true,
          filter: path => path !== sourceNodeModules && !path.startsWith(`${sourceNodeModules}${sep}`),
        })
        restored = true
      }
    }
    if (!restored) return
  }
}

function closureDigest(root: string, files: readonly string[]): ValidatedHarnessClosure['closure'] {
  const hash = createHash('sha256')
  let totalBytes = 0
  for (const path of files) {
    const name = relative(root, path).replaceAll('\\', '/')
    const bytes = readFileSync(path)
    totalBytes += bytes.length
    hash.update(`${name}\0${String(bytes.length)}\0`)
    hash.update(bytes)
  }
  return { sha256: hash.digest('hex'), fileCount: files.length, totalBytes }
}

/**
 * Validate package identities, required dependency resolution, executable, Web assets, and link-free files.
 * @param packageRoot - Absolute pnpm deploy output.
 * @returns Descriptor-safe facts and deterministic closure integrity.
 */
export function validateHarnessClosure(packageRoot: string): ValidatedHarnessClosure {
  const root = realpathSync(packageRoot)
  const files = filesInClosure(root)
  const manifestPath = join(root, 'package.json')
  const manifest = readManifest(manifestPath)
  const packageName = requireString(manifest.name, 'name', manifestPath)
  if (packageName !== CLI_PACKAGE) throw new Error(`Harness staging: deployed root is ${packageName}, expected ${CLI_PACKAGE}.`)
  const packageVersion = requireString(manifest.version, 'version', manifestPath)
  const dshBin = resolveDshBin(root, manifest)

  for (const installedRoot of packageRoots(root)) {
    const installedManifestPath = join(installedRoot, 'package.json')
    const installedManifest = readManifest(installedManifestPath)
    const installedName = requireString(installedManifest.name, 'name', installedManifestPath)
    for (const dependency of requiredDependencies(installedManifest)) {
      const dependencyManifest = installedPackageManifest(root, installedRoot, dependency)
      if (dependencyManifest === undefined) {
        throw new Error(`Harness staging: ${installedName} dependency ${dependency} is missing from the deployed closure.`)
      }
      const dependencyIdentity = requireString(readManifest(dependencyManifest).name, 'name', dependencyManifest)
      if (dependencyIdentity !== dependency) {
        throw new Error(`Harness staging: ${installedName} dependency ${dependency} resolved to ${dependencyIdentity}.`)
      }
    }
  }

  const webRoot = join(root, 'node_modules', ...WEB_PACKAGE.split('/'))
  const webManifestPath = join(webRoot, 'package.json')
  if (!existsSync(webManifestPath)) throw new Error(`Harness staging: expected Web package ${WEB_PACKAGE} is missing.`)
  const webName = requireString(readManifest(webManifestPath).name, 'name', webManifestPath)
  if (webName !== WEB_PACKAGE) throw new Error(`Harness staging: Web package identity is ${webName}, expected ${WEB_PACKAGE}.`)
  const webAssets = `node_modules/${WEB_PACKAGE}/dist`
  const webIndex = join(root, ...webAssets.split('/'), 'index.html')
  if (!existsSync(webIndex) || !lstatSync(webIndex).isFile()) {
    throw new Error(`Harness staging: built Web asset ${webAssets}/index.html is missing or not a regular file.`)
  }

  return { packageRoot: root, packageName, packageVersion, dshBin, webAssets, closure: closureDigest(root, files) }
}

/**
 * Create deterministic allowlisted runtime metadata without environment or absolute paths.
 * @param closure - Validated staged closure facts.
 * @param facts - Source facts pinned by the repository.
 * @returns Stable descriptor consumed later by Rust.
 */
export function createRuntimeDescriptor(
  closure: ValidatedHarnessClosure,
  facts: { readonly harnessSourceSha: string },
): HarnessRuntimeDescriptor {
  if (!/^[a-f0-9]{40}$/.test(facts.harnessSourceSha)) throw new Error('Harness staging: harness source SHA must be 40 lowercase hexadecimal characters.')
  return {
    schemaVersion: 1,
    packageRoot: '.',
    packageName: closure.packageName,
    packageVersion: closure.packageVersion,
    dshBin: closure.dshBin,
    webAssets: closure.webAssets,
    harnessSourceSha: facts.harnessSourceSha,
    closure: closure.closure,
  }
}

function childEnvironment(): NodeJS.ProcessEnv {
  return Object.fromEntries(Object.entries(process.env).filter(([name]) => !/(?:KEY|SECRET|TOKEN|PASSWORD)/i.test(name)))
}

function defaultRun(invocation: CommandInvocation): void {
  const result = spawnSync(invocation.command, [...invocation.args], {
    cwd: invocation.cwd,
    env: invocation.env,
    stdio: 'inherit',
    shell: false,
  })
  if (result.error !== undefined) throw result.error
  if (result.status !== 0) throw new Error(`Harness staging: ${invocation.command} ${invocation.args.join(' ')} exited with ${String(result.status)}.`)
}

function readHarnessSourceSha(repoRoot: string): string {
  const pinsPath = join(repoRoot, 'apps/melon-desktop/runtime/runtime-pins.json')
  const pins = JSON.parse(readFileSync(pinsPath, 'utf8')) as { harness?: { sourceSha?: unknown } }
  return requireString(pins.harness?.sourceSha, 'harness.sourceSha', pinsPath)
}

function publishCandidate(candidate: string, publication: string, backup: string): void {
  const hadPublication = existsSync(publication)
  if (hadPublication) renameSync(publication, backup)
  try {
    renameSync(candidate, publication)
  } catch (error) {
    if (hadPublication) renameSync(backup, publication)
    throw error
  }
  if (hadPublication) rmSync(backup, { recursive: true, force: true })
}

/**
 * Build through current upstream release/deploy commands, validate, then publish by rename.
 * @param options - Test-only repository and command runner overrides.
 * @returns Published path, consumed candidate path, and descriptor.
 */
export function stageHarnessRuntime(options: StageHarnessOptions = {}): StageHarnessResult {
  const repoRoot = realpathSync(options.repoRoot ?? REPO_ROOT)
  const rootManifest = readManifest(join(repoRoot, 'package.json'))
  if (rootManifest.name !== '@deepseek-ai/dsh-root') throw new Error(`Harness staging: ${repoRoot} is not the DeepSeek Harness repository root.`)

  const resources = join(repoRoot, 'apps/melon-desktop/src-tauri/resources')
  mkdirSync(resources, { recursive: true })
  const workspace = mkdtempSync(join(resources, '.harness-stage-'))
  const candidate = join(workspace, 'candidate')
  const packedDsh = join(workspace, 'packed-dsh')
  const packedVendor = join(workspace, 'packed-vendor')
  const packedLandlock = join(workspace, 'packed-landlock')
  const publication = join(repoRoot, PUBLICATION_RELATIVE)
  const backup = join(workspace, 'previous')
  const run = options.run ?? defaultRun
  const pnpm = process.platform === 'win32' ? 'pnpm.cmd' : 'pnpm'
  const invoke = (args: readonly string[]): void => run({ command: pnpm, args, cwd: repoRoot, env: childEnvironment() })

  try {
    invoke(['run', 'build'])
    invoke(['run', 'release:pack', '--family', 'dsh', '--out', packedDsh])
    invoke(['run', 'release:pack', '--family', 'vendor', '--out', packedVendor])
    invoke(['--dir', 'native/landlock-run', 'run', 'build:ts'])
    invoke(['--dir', 'native/landlock-run/packages/entry', 'pack', '--pack-destination', packedLandlock])
    invoke(['run', 'release:verify-packed-install', '--family', 'dsh', '--from', packedDsh, '--from', packedVendor, '--from', packedLandlock])
    invoke([
      '--filter', CLI_PACKAGE, 'deploy', '--legacy', '--prod',
      '--config.node-linker=hoisted', '--config.auto-install-peers=false', '--config.link-workspace-packages=true',
      candidate,
    ])
    restoreWorkspaceDependencies(candidate, repoRoot)
    materializeLinks(candidate)

    const closure = validateHarnessClosure(candidate)
    const descriptor = createRuntimeDescriptor(closure, { harnessSourceSha: readHarnessSourceSha(repoRoot) })
    writeFileSync(join(candidate, DESCRIPTOR_NAME), `${JSON.stringify(descriptor, null, 2)}\n`, { mode: 0o644, flag: 'wx' })
    publishCandidate(candidate, publication, backup)
    return { publicationPath: publication, candidatePath: candidate, descriptor }
  } finally {
    rmSync(workspace, { recursive: true, force: true })
  }
}

function main(): void {
  if (process.argv.length !== 2) throw new Error('usage: stage-harness-runtime.mts')
  const result = stageHarnessRuntime()
  console.log(`Harness runtime staged at ${relative(REPO_ROOT, result.publicationPath).replaceAll('\\', '/')}`)
}

if (isEntry(import.meta.url)) main()
