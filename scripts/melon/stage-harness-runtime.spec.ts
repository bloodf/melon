import { chmodSync, existsSync, lstatSync, mkdtempSync, mkdirSync, readFileSync, readdirSync, renameSync, rmSync, symlinkSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import {
  createRuntimeDescriptor,
  resolveDshBin,
  stageHarnessRuntime,
  validateHarnessClosure,
  type CommandInvocation,
} from './stage-harness-runtime.mts'

const roots: string[] = []
const temporaryRoot = (): string => {
  const root = mkdtempSync(join(tmpdir(), 'melon-harness-stage-test-'))
  roots.push(root)
  return root
}
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }) })

function writeJson(path: string, value: unknown): void {
  mkdirSync(dirname(path), { recursive: true })
  writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`)
}

function writeFile(path: string, value = 'fixture\n'): void {
  mkdirSync(dirname(path), { recursive: true })
  writeFileSync(path, value)
}

function closure(root: string): void {
  writeJson(join(root, 'package.json'), {
    name: '@deepseek-ai/dsh',
    version: '0.1.0-rc.5',
    bin: { dsh: 'lib/bin.js' },
    dependencies: {
      '@deepseek-ai/dsh-web-frontend': 'workspace:^',
      'fixture-helper': '^1.0.0',
    },
  })
  writeFile(join(root, 'lib/bin.js'), '#!/usr/bin/env node\n')
  chmodSync(join(root, 'lib/bin.js'), 0o755)
  writeJson(join(root, 'node_modules/@deepseek-ai/dsh-web-frontend/package.json'), {
    name: '@deepseek-ai/dsh-web-frontend',
    version: '0.1.0-rc.5',
  })
  writeFile(join(root, 'node_modules/@deepseek-ai/dsh-web-frontend/dist/index.html'), '<main>Harness</main>\n')
  writeJson(join(root, 'node_modules/fixture-helper/package.json'), {
    name: 'fixture-helper',
    version: '1.0.0',
  })
}

function repository(root: string): void {
  writeJson(join(root, 'package.json'), { name: '@deepseek-ai/dsh-root', private: true })
  writeJson(join(root, 'apps/cli/package.json'), { name: '@deepseek-ai/dsh', version: '0.1.0-rc.5' })
  writeJson(join(root, 'apps/melon-desktop/runtime/runtime-pins.json'), {
    schemaVersion: 1,
    melonVersion: '0.1.0',
    harness: { sourceSha: '47f943859bef60e4160492346772ded9b24f765a' },
  })
  writeJson(join(root, 'vendor/cosmokit/package.json'), { name: '@deepseek-ai/cosmokit', version: '1.8.2' })
  mkdirSync(join(root, 'apps/melon-desktop/src-tauri/resources'), { recursive: true })
}

function fixtureRunner(invocations: CommandInvocation[]): (invocation: CommandInvocation) => void {
  return (invocation) => {
    invocations.push(invocation)
    const deployIndex = invocation.args.indexOf('deploy')
    if (deployIndex !== -1) {
      const candidate = invocation.args.at(-1)!
      closure(candidate)
      const realPackage = join(dirname(candidate), 'linked-package-source')
      writeJson(join(realPackage, 'package.json'), { name: 'linked-package', version: '1.0.0' })
      symlinkSync(realPackage, join(candidate, 'node_modules/linked-package'))
    }
  }
}

describe('Harness package metadata', () => {
  it('resolves dsh from package bin metadata', () => {
    const root = temporaryRoot()
    writeFile(join(root, 'lib/entry.js'))
    expect(resolveDshBin(root, { name: '@deepseek-ai/dsh', bin: { dsh: 'lib/entry.js' } })).toBe('lib/entry.js')
  })

  it.each([
    ['missing', { name: '@deepseek-ai/dsh' }],
    ['wrong command', { name: '@deepseek-ai/dsh', bin: { other: 'lib/bin.js' } }],
    ['non-string', { name: '@deepseek-ai/dsh', bin: { dsh: 7 } }],
    ['escaping', { name: '@deepseek-ai/dsh', bin: { dsh: '../bin.js' } }],
  ])('rejects %s bin metadata', (_label, manifest) => {
    expect(() => resolveDshBin(temporaryRoot(), manifest)).toThrow(/bin\.dsh|relative package path/)
  })
})

describe('Harness closure validation', () => {
  it('requires every production dependency in the deployed closure', () => {
    const root = temporaryRoot()
    closure(root)
    rmSync(join(root, 'node_modules/fixture-helper'), { recursive: true })
    expect(() => validateHarnessClosure(root)).toThrow(/fixture-helper.*missing/)
  })

  it('rejects links anywhere in the staged closure', () => {
    const root = temporaryRoot()
    closure(root)
    symlinkSync(join(root, 'lib/bin.js'), join(root, 'linked-bin.js'))
    expect(() => validateHarnessClosure(root)).toThrow(/symbolic link/)
  })

  it('requires built Web assets and expected package identities', () => {
    const root = temporaryRoot()
    closure(root)
    rmSync(join(root, 'node_modules/@deepseek-ai/dsh-web-frontend/dist/index.html'))
    expect(() => validateHarnessClosure(root)).toThrow(/Web.*index\.html/)
  })
})

describe('Harness runtime descriptor', () => {
  it('is deterministic, relative, and contains no ambient secret', () => {
    const root = temporaryRoot()
    closure(root)
    const validated = validateHarnessClosure(root)
    const facts = { harnessSourceSha: '47f943859bef60e4160492346772ded9b24f765a' }
    const first = createRuntimeDescriptor(validated, facts)
    process.env.MELON_DURINDOOR_API_KEY = 'sk-do-not-stage'
    const second = createRuntimeDescriptor(validateHarnessClosure(root), facts)
    delete process.env.MELON_DURINDOOR_API_KEY

    expect(second).toEqual(first)
    expect(first.packageRoot).toBe('.')
    expect(first.dshBin).toBe('lib/bin.js')
    expect(first.webAssets).toBe('node_modules/@deepseek-ai/dsh-web-frontend/dist')
    expect(JSON.stringify(first)).not.toContain(root)
    expect(JSON.stringify(first)).not.toMatch(/sk-do-not-stage|api.?key|secret|token/i)
    expect(first.closure.sha256).toMatch(/^[a-f0-9]{64}$/)
  })
})

describe('Harness runtime staging', () => {
  it('preserves the previous publication when verification fails', () => {
    const root = temporaryRoot()
    repository(root)
    const published = join(root, 'apps/melon-desktop/src-tauri/resources/harness')
    writeFile(join(published, 'previous.txt'), 'keep\n')
    const invocations: CommandInvocation[] = []
    const run = fixtureRunner(invocations)

    expect(() => stageHarnessRuntime({
      repoRoot: root,
      run: (invocation) => {
        run(invocation)
        if (invocation.args.includes('release:verify-packed-install')) throw new Error('fixture verification failed')
      },
    })).toThrow(/fixture verification failed/)

    expect(readFileSync(join(published, 'previous.txt'), 'utf8')).toBe('keep\n')
    expect(invocations.some(invocation => invocation.args.includes('release:verify-packed-install'))).toBe(true)
  })

  it('restores the prior publication after one candidate rename failure', () => {
    const root = temporaryRoot()
    repository(root)
    const resources = join(root, 'apps/melon-desktop/src-tauri/resources')
    const published = join(resources, 'harness')
    writeFile(join(published, 'previous.txt'), 'keep\n')
    let renames = 0

    expect(() => stageHarnessRuntime({
      repoRoot: root,
      run: fixtureRunner([]),
      rename: (from, to) => {
        renames += 1
        if (renames === 2) throw new Error('candidate publication failed')
        renameSync(from, to)
      },
    })).toThrow(/candidate publication failed/)

    expect(readFileSync(join(published, 'previous.txt'), 'utf8')).toBe('keep\n')
    expect(readdirSync(resources).filter(name => name.startsWith('.harness-backup-'))).toEqual([])
  })

  it('retains the prior publication outside candidate cleanup when publication and restore both fail', () => {
    const root = temporaryRoot()
    repository(root)
    const resources = join(root, 'apps/melon-desktop/src-tauri/resources')
    const published = join(resources, 'harness')
    writeFile(join(published, 'previous.txt'), 'recover me\n')
    let renames = 0

    expect(() => stageHarnessRuntime({
      repoRoot: root,
      run: fixtureRunner([]),
      rename: (from, to) => {
        renames += 1
        if (renames >= 2) throw new Error(`rename ${String(renames)} failed`)
        renameSync(from, to)
      },
    })).toThrow(/prior runtime retained for recovery at .*\.harness-backup-/)

    expect(existsSync(published)).toBe(false)
    const backups = readdirSync(resources).filter(name => name.startsWith('.harness-backup-'))
    expect(backups).toHaveLength(1)
    expect(readFileSync(join(resources, backups[0]!, 'runtime', 'previous.txt'), 'utf8')).toBe('recover me\n')
    expect(readdirSync(resources).some(name => name.startsWith('.harness-stage-'))).toBe(false)
  })

  it('publishes a validated candidate and cleans staging state', () => {
    const root = temporaryRoot()
    repository(root)
    const published = join(root, 'apps/melon-desktop/src-tauri/resources/harness')
    writeFile(join(published, 'previous.txt'), 'replace\n')
    const invocations: CommandInvocation[] = []

    const result = stageHarnessRuntime({ repoRoot: root, run: fixtureRunner(invocations) })

    expect(result.publicationPath).toBe(published)
    expect(lstatSync(join(published, 'node_modules/linked-package')).isDirectory()).toBe(true)
    expect(lstatSync(join(published, 'node_modules/linked-package')).isSymbolicLink()).toBe(false)
    expect(existsSync(join(published, 'previous.txt'))).toBe(false)
    expect(lstatSync(join(published, 'lib/bin.js')).isFile()).toBe(true)
    const descriptor = JSON.parse(readFileSync(join(published, 'melon-harness-runtime.json'), 'utf8')) as { dshBin: string; harnessSourceSha: string }
    expect(descriptor).toMatchObject({
      dshBin: 'lib/bin.js',
      harnessSourceSha: '47f943859bef60e4160492346772ded9b24f765a',
    })
    expect(invocations.map(invocation => [invocation.command, ...invocation.args])).toEqual([
      ['pnpm', 'run', 'build'],
      ['pnpm', 'run', 'release:pack', '--family', 'dsh', '--out', expect.any(String)],
      ['pnpm', 'run', 'release:pack', '--family', 'vendor', '--out', expect.any(String)],
      ['pnpm', '--dir', 'native/landlock-run', 'run', 'build:ts'],
      ['pnpm', '--dir', 'native/landlock-run/packages/entry', 'pack', '--pack-destination', expect.any(String)],
      ['pnpm', 'run', 'release:verify-packed-install', '--family', 'dsh', '--from', expect.any(String), '--from', expect.any(String), '--from', expect.any(String)],
      ['pnpm', '--filter', '@deepseek-ai/dsh', 'deploy', '--legacy', '--prod', '--config.node-linker=hoisted', '--config.auto-install-peers=false', '--config.link-workspace-packages=true', expect.any(String)],
    ])
    expect(existsSync(dirname(result.candidatePath))).toBe(false)
    expect(readdirSync(join(root, 'apps/melon-desktop/src-tauri/resources')).filter(name => name.startsWith('.harness-backup-'))).toEqual([])
    expect(existsSync(result.candidatePath)).toBe(false)
  })
})
