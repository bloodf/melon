/** Build, validate, and atomically publish Melon's generated Harness runtime. */

import { createHash } from 'node:crypto'
import { spawnSync } from 'node:child_process'
import {
  chmodSync,
  copyFileSync,
  existsSync,
  globSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  realpathSync,
  renameSync,
  rmdirSync,
  rmSync,
  unlinkSync,
  writeFileSync,
} from 'node:fs'
import { basename, dirname, isAbsolute, join, posix, relative, resolve, sep } from 'node:path'
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
  readonly main?: unknown
  readonly files?: unknown
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

/** Non-secret descriptor; `closure` covers every staged file except this descriptor itself and identifies exact built bytes, not cross-machine reproducibility. */
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

/** Result from one completed staging publication. */
export interface StageHarnessResult {
  readonly publicationPath: string
  readonly descriptor: HarnessRuntimeDescriptor
}

interface StageHarnessOptions {
  readonly repoRoot?: string
  readonly run?: (invocation: CommandInvocation) => void
  readonly rename?: (from: string, to: string) => void
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
interface RequiredDependency {
  readonly key: string
  readonly name: string
  readonly range: string
}

function requiredDependencies(manifest: PackageManifest): RequiredDependency[] {
  const dependencies = new Map<string, string>()
  if (manifest.dependencies !== null && typeof manifest.dependencies === 'object' && !Array.isArray(manifest.dependencies)) {
    for (const [name, range] of Object.entries(manifest.dependencies)) if (typeof range === 'string') dependencies.set(name, range)
  }
  if (manifest.peerDependencies !== null && typeof manifest.peerDependencies === 'object' && !Array.isArray(manifest.peerDependencies)) {
    const meta = manifest.peerDependenciesMeta !== null && typeof manifest.peerDependenciesMeta === 'object' && !Array.isArray(manifest.peerDependenciesMeta)
      ? manifest.peerDependenciesMeta as Record<string, unknown>
      : {}
    for (const [name, range] of Object.entries(manifest.peerDependencies)) {
      const entry = meta[name]
      if (entry !== null && typeof entry === 'object' && (entry as Record<string, unknown>).optional === true) continue
      if (typeof range === 'string') dependencies.set(name, range)
    }
  }
  return [...dependencies].sort(([left], [right]) => left.localeCompare(right)).map(([key, specifier]) => {
    if (!specifier.startsWith('npm:')) return { key, name: key, range: specifier }
    const alias = specifier.slice(4)
    const split = alias.lastIndexOf('@')
    if (split <= 0) throw new Error(`Harness staging: invalid npm alias ${specifier}.`)
    return { key, name: alias.slice(0, split), range: alias.slice(split + 1) }
  })
}

function versionNumbers(version: string): [number, number, number] | undefined {
  const match = /^(\d+)\.(\d+)\.(\d+)(?:-[0-9A-Za-z.-]+)?$/.exec(version)
  return match === null ? undefined : [Number(match[1]), Number(match[2]), Number(match[3])]
}

function satisfiesComparator(version: string, comparator: string): boolean {
  if (comparator === '' || comparator === '*' || comparator.startsWith('file:') || comparator.startsWith('link:')) return true
  const actual = versionNumbers(version)
  if (actual === undefined) return false
  const match = /^(\^|~|>=|<=|>|<|=)?(\d+)(?:\.(\d+|x|\*))?(?:\.(\d+|x|\*))?(?:-[0-9A-Za-z.-]+)?$/.exec(comparator)
  if (match === null) return comparator.startsWith('workspace:') ? satisfiesRange(version, comparator.slice('workspace:'.length)) : false
  const operator = match[1] ?? '='
  const expected: [number, number, number] = [Number(match[2]), Number(match[3] ?? 0), Number(match[4] ?? 0)]
  if (match[3] === 'x' || match[3] === '*') return actual[0] === expected[0]
  if (match[4] === 'x' || match[4] === '*') return actual[0] === expected[0] && actual[1] === expected[1]
  const comparison = actual[0] - expected[0] || actual[1] - expected[1] || actual[2] - expected[2]
  if (operator === '^') return comparison >= 0 && actual[0] === expected[0]
  if (operator === '~') return comparison >= 0 && actual[0] === expected[0] && actual[1] === expected[1]
  if (operator === '>=') return comparison >= 0
  if (operator === '<=') return comparison <= 0
  if (operator === '>') return comparison > 0
  if (operator === '<') return comparison < 0
  return comparison === 0
}

function satisfiesRange(version: string, range: string): boolean {
  if (range === 'workspace:^' || range === 'workspace:~' || range === 'workspace:*') return true
  return range.split('||').some(disjunction => {
    const tokens = disjunction.trim().split(/\s+/).filter(Boolean)
    const parts: string[] = []
    for (let index = 0; index < tokens.length; index += 1) {
      const token = tokens[index]!
      if ((token === '>=' || token === '<=' || token === '>' || token === '<' || token === '=') && tokens[index + 1] !== undefined) {
        parts.push(`${token}${tokens[index + 1]}`)
        index += 1
        continue
      }
      parts.push(token)
    }
    return parts.every(part => satisfiesComparator(version, part))
  })
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
function findLinks(directory: string): string[] {
  const links: string[] = []
  for (const name of readdirSync(directory).sort()) {
    const path = join(directory, name)
    const status = lstatSync(path)
    if (status.isSymbolicLink()) links.push(path)
    else if (status.isDirectory()) links.push(...findLinks(path))
  }
  return links
}

function copyNoFollow(source: string, destination: string, allowedRoot: string, stack = new Set<string>()): void {
  const status = lstatSync(source)
  if (status.isSymbolicLink()) {
    const target = realpathSync(source)
    if (!isInside(allowedRoot, target)) throw new Error(`Harness staging: symlink target ${target} is outside allowed roots.`)
    copyNoFollow(target, destination, allowedRoot, stack)
    return
  }
  if (status.isDirectory()) {
    const real = realpathSync(source)
    if (stack.has(real)) throw new Error(`Harness staging: link cycle at ${source}.`)
    stack.add(real)
    mkdirSync(destination, { recursive: true })
    for (const name of readdirSync(source).sort()) copyNoFollow(join(source, name), join(destination, name), allowedRoot, stack)
    stack.delete(real)
    return
  }
  if (!status.isFile()) throw new Error(`Harness staging: unsupported linked entry ${source}.`)
  mkdirSync(dirname(destination), { recursive: true })
  copyFileSync(source, destination)
  chmodSync(destination, status.mode & 0o777)
}

function materializeLinks(root: string): void {
  for (const link of findLinks(root)) {
    if (!existsSync(link)) continue
    const source = realpathSync(link)
    if (!isInside(root, source)) throw new Error(`Harness staging: symlink target ${source} is outside allowed roots.`)
    unlinkSync(link)
    copyNoFollow(source, link, root)
  }
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

function globRegex(pattern: string): RegExp {
  let source = '^'
  for (let index = 0; index < pattern.length; index += 1) {
    const char = pattern[index]!
    if (char === '*' && pattern[index + 1] === '*') {
      if (pattern[index + 2] === '/') { source += '(?:.*/)?'; index += 2 }
      else { source += '.*'; index += 1 }
    }
    else if (char === '*') source += '[^/]*'
    else if (char === '?') source += '[^/]'
    else source += char.replace(/[\\^$+?.()|[\]{}]/g, '\\$&')
  }
  return new RegExp(`${source}(?:/.*)?$`)
}

function isPublishable(relativePath: string, mandatory: RegExp, positive: readonly RegExp[], negative: readonly RegExp[], files: readonly string[]): boolean {
  if (negative.some(pattern => pattern.test(relativePath))) return false
  if (mandatory.test(relativePath)) return true
  const explicitlyNamed = files.some(pattern => !pattern.startsWith('!') && pattern.replace(/^\.\//, '') === relativePath)
  if ((/^\.env(?:\.|$)/.test(basename(relativePath)) || relativePath.endsWith('.tsbuildinfo')) && !explicitlyNamed) return false
  return positive.some(pattern => pattern.test(relativePath))
}

function publishedFiles(source: string, manifest: PackageManifest): string[] {
  if (!Array.isArray(manifest.files) || manifest.files.some(value => typeof value !== 'string')) {
    throw new Error(`Harness staging: workspace package ${String(manifest.name)} must declare string files entries.`)
  }
  const files = manifest.files as string[]
  const positive = files.filter(pattern => !pattern.startsWith('!')).map(pattern => globRegex(pattern.replace(/^\.\//, '')))
  const negative = files.filter(pattern => pattern.startsWith('!')).map(pattern => globRegex(pattern.slice(1).replace(/^\.\//, '')))
  const mandatory = /^(?:package\.json|readme(?:\..*)?|licen[cs]e(?:\..*)?|notice(?:\..*)?)$/i
  const selected: string[] = []
  const visit = (directory: string): void => {
    for (const name of readdirSync(directory).sort()) {
      const path = join(directory, name)
      const relativePath = relative(source, path).replaceAll('\\', '/')
      if (
        (relativePath === 'node_modules' || relativePath.startsWith('node_modules/'))
        && !files.some(pattern => {
          if (pattern.startsWith('!')) return false
          const named = pattern.replace(/^\.\//, '')
          return named === 'node_modules' || named.startsWith('node_modules/')
        })
      ) continue
      const status = lstatSync(path)
      if (status.isSymbolicLink()) throw new Error(`Harness staging: workspace publish surface contains symbolic link ${relativePath}.`)
      if (status.isDirectory()) visit(path)
      else if (!status.isFile()) throw new Error(`Harness staging: workspace publish surface contains special entry ${relativePath}.`)
      else if (isPublishable(relativePath, mandatory, positive, negative, files)) selected.push(relativePath)
    }
  }
  visit(source)
  if (!selected.includes('package.json')) throw new Error(`Harness staging: workspace package ${String(manifest.name)} has no package.json.`)
  return selected
}

function copyPublishedWorkspace(source: string, destination: string): void {
  const manifest = readManifest(join(source, 'package.json'))
  for (const relativePath of publishedFiles(source, manifest)) {
    const from = join(source, ...relativePath.split('/'))
    const to = join(destination, ...relativePath.split('/'))
    const status = lstatSync(from)
    mkdirSync(dirname(to), { recursive: true })
    copyFileSync(from, to)
    chmodSync(to, status.mode & 0o777)
  }
}

function restoreWorkspaceDependencies(candidate: string, repoRoot: string): void {
  const workspaces = workspacePackages(repoRoot)
  for (const link of findLinks(candidate)) {
    const source = realpathSync(link)
    if (!isInside(repoRoot, source)) continue
    const manifestPath = join(source, 'package.json')
    if (!existsSync(manifestPath)) continue
    unlinkSync(link)
    copyPublishedWorkspace(source, link)
  }
  for (;;) {
    let restored = false
    for (const installedRoot of packageRoots(candidate)) {
      const manifest = readManifest(join(installedRoot, 'package.json'))
      for (const dependency of requiredDependencies(manifest)) {
        if (installedPackageManifest(candidate, installedRoot, dependency.key) !== undefined) continue
        const source = workspaces.get(dependency.name)
        if (source === undefined) continue
        const destination = join(candidate, 'node_modules', ...dependency.key.split('/'))
        copyPublishedWorkspace(source, destination)
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
    const executable = lstatSync(path).mode & 0o111
    totalBytes += bytes.length
    hash.update(`${name}\0${String(bytes.length)}\0${String(executable)}\0`)
    hash.update(bytes)
  }
  return { sha256: hash.digest('hex'), fileCount: files.length, totalBytes }
}

/**
 * Validate package identities, required dependency resolution, executable, Web assets, and link-free files.
 * @param packageRoot - Absolute pnpm deploy output.
 * @returns Descriptor-safe facts and deterministic closure integrity.
 */
function auditPackageSurfaces(root: string, workspaceNames?: ReadonlySet<string>): void {
  for (const packageRoot of packageRoots(root)) {
    const manifestPath = join(packageRoot, 'package.json')
    const manifest = readManifest(manifestPath)
    if (typeof manifest.name !== 'string') continue
    if (workspaceNames !== undefined && !workspaceNames.has(manifest.name)) continue
    if (!Array.isArray(manifest.files) || manifest.files.some(value => typeof value !== 'string')) continue
    const allowed = new Set(publishedFiles(packageRoot, manifest))
    for (const path of filesInClosure(packageRoot)) {
      const relativePath = relative(packageRoot, path).replaceAll('\\', '/')
      if (allowed.has(relativePath)) continue
      if (relativePath.startsWith('node_modules/')) continue
      if (relativePath === DESCRIPTOR_NAME && packageRoot === root) continue
      throw new Error(`Harness staging: non-publishable package file ${String(manifest.name)}/${relativePath}.`)
    }
  }
}

export function validateHarnessClosure(
  packageRoot: string,
  options: { readonly workspaceNames?: ReadonlySet<string> } = {},
): ValidatedHarnessClosure {
  const root = realpathSync(packageRoot)
  const files = filesInClosure(root)
  auditPackageSurfaces(root, options.workspaceNames)
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
      const dependencyManifestPath = installedPackageManifest(root, installedRoot, dependency.key)
      if (dependencyManifestPath === undefined) {
        throw new Error(`Harness staging: ${installedName} dependency ${dependency.key} is missing from the deployed closure.`)
      }
      const dependencyManifest = readManifest(dependencyManifestPath)
      const dependencyIdentity = requireString(dependencyManifest.name, 'name', dependencyManifestPath)
      const dependencyVersion = requireString(dependencyManifest.version, 'version', dependencyManifestPath)
      if (dependencyIdentity !== dependency.name) {
        throw new Error(`Harness staging: ${installedName} dependency ${dependency.key} resolved to ${dependencyIdentity}.`)
      }
      if (options.workspaceNames?.has(installedName) !== false && !satisfiesRange(dependencyVersion, dependency.range)) {
        throw new Error(`Harness staging: ${installedName} dependency ${dependency.key} resolved to version ${dependencyVersion}, outside ${dependency.range}.`)
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

const BASE_ENV = ['PATH', 'HOME', 'USERPROFILE', 'TMPDIR', 'TEMP', 'TMP', 'SYSTEMROOT', 'WINDIR', 'COMSPEC', 'CI', 'SHELL', 'NODE_EXTRA_CA_CERTS'] as const
const REGISTRY_ENV = ['NPM_CONFIG_REGISTRY', 'npm_config_registry', 'NPM_CONFIG_USERCONFIG', 'npm_config_userconfig', 'NPM_TOKEN', 'NODE_AUTH_TOKEN'] as const

function childEnvironment(registryAuth = false): NodeJS.ProcessEnv {
  const names: readonly string[] = registryAuth ? [...BASE_ENV, ...REGISTRY_ENV] : BASE_ENV
  return Object.fromEntries(names.flatMap(name => process.env[name] === undefined ? [] : [[name, process.env[name]!]]))
}

function pnpmInvocation(args: readonly string[], entrypoint: string): Pick<CommandInvocation, 'command' | 'args'> {
  return process.platform === 'win32'
    ? { command: process.execPath, args: [entrypoint, ...args] }
    : { command: entrypoint, args }
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

function removeOwnedBackup(resources: string, backupRoot: string): void {
  if (dirname(backupRoot) !== resources || !basename(backupRoot).startsWith('.harness-backup-')) {
    throw new Error(`Harness staging: refusing to remove unowned backup path ${backupRoot}.`)
  }
  const remove = (path: string): void => {
    const status = lstatSync(path)
    if (status.isSymbolicLink() || !status.isDirectory()) {
      unlinkSync(path)
      return
    }
    for (const name of readdirSync(path)) remove(join(path, name))
    rmdirSync(path)
  }
  remove(backupRoot)
}

function publishCandidate(
  candidate: string,
  publication: string,
  resources: string,
  rename: (from: string, to: string) => void,
): void {
  if (!existsSync(publication)) {
    rename(candidate, publication)
    return
  }

  const backupRoot = mkdtempSync(join(resources, '.harness-backup-'))
  const backup = join(backupRoot, 'runtime')
  try {
    rename(publication, backup)
  } catch (error) {
    removeOwnedBackup(resources, backupRoot)
    throw error
  }
  try {
    rename(candidate, publication)
  } catch (publishError) {
    try {
      rename(backup, publication)
    } catch (restoreError) {
      throw new Error(
        `Harness staging: candidate publication and prior-runtime restore failed; prior runtime retained for recovery at ${backup}.`,
        { cause: new AggregateError([publishError, restoreError]) },
      )
    }
    removeOwnedBackup(resources, backupRoot)
    throw publishError
  }
  removeOwnedBackup(resources, backupRoot)
}

/**
 * Build through current upstream release/deploy commands, validate, then publish by rename.
 * @param options - Test-only repository and command runner overrides.
 * @returns Published path and descriptor.
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
  const run = options.run ?? defaultRun
  const rename = options.rename ?? renameSync
  const entrypoint = process.env.npm_execpath ?? (run === defaultRun ? undefined : 'pnpm.cjs')
  if (entrypoint === undefined) throw new Error('Harness staging: npm_execpath is unavailable; invoke staging through its pnpm package script.')
  const invoke = (args: readonly string[], registryAuth = false): void => run({ ...pnpmInvocation(args, entrypoint), cwd: repoRoot, env: childEnvironment(registryAuth) })

  try {
    invoke(['run', 'build'])
    invoke(['run', 'release:pack', '--family', 'dsh', '--out', packedDsh])
    invoke(['run', 'release:pack', '--family', 'vendor', '--out', packedVendor])
    invoke(['--dir', 'native/landlock-run', 'run', 'build:ts'])
    invoke(['--dir', 'native/landlock-run/packages/entry', 'pack', '--pack-destination', packedLandlock])
    invoke(['run', 'release:verify-packed-install', '--family', 'dsh', '--from', packedDsh, '--from', packedVendor, '--from', packedLandlock], true)
    invoke([
      '--filter', CLI_PACKAGE, 'deploy', '--legacy', '--prod',
      '--config.node-linker=hoisted', '--config.auto-install-peers=false', '--config.link-workspace-packages=true',
      candidate,
    ])
    restoreWorkspaceDependencies(candidate, repoRoot)
    materializeLinks(candidate)

    const closure = validateHarnessClosure(candidate, { workspaceNames: new Set(workspacePackages(repoRoot).keys()) })
    const descriptor = createRuntimeDescriptor(closure, { harnessSourceSha: readHarnessSourceSha(repoRoot) })
    writeFileSync(join(candidate, DESCRIPTOR_NAME), `${JSON.stringify(descriptor, null, 2)}\n`, { mode: 0o644, flag: 'wx' })
    publishCandidate(candidate, publication, resources, rename)
    return { publicationPath: publication, descriptor }
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
